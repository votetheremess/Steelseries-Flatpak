//! Clip metadata index, directory scan, filename sanitization.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipMeta {
    pub filename: String,
    pub duration_ms: u64,
    pub game_name: String,
    pub created_unix: u64,
    pub bitrate_kbps: u32,
    pub resolution: String,
}

/// Sanitize a game name for inclusion in a filename.
/// - Whitespace -> hyphen
/// - Non-alphanumeric (except hyphen) -> stripped
/// - Truncate to 40 chars
pub fn sanitize_game_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    s.chars().take(40).collect()
}

/// Build a base filename (no extension) from a creation timestamp + game name.
/// Format: `YYYY-MM-DD-HHMM-<sanitized-game>` (local time).
pub fn build_base_filename(created: SystemTime, game_name: &str) -> String {
    use std::time::UNIX_EPOCH;
    let secs = created
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let game = if game_name.is_empty() { "Untitled" } else { game_name };
    let sanitized = sanitize_game_name(game);
    let final_game = if sanitized.is_empty() {
        "Untitled".to_string()
    } else {
        sanitized
    };
    // Convert Unix seconds to local-time `tm` via libc to avoid pulling in `chrono`.
    // SAFETY: `localtime` returns a pointer to a thread-local static `tm` that is
    // valid until the next call to localtime/gmtime on this thread. We copy it
    // immediately into an owned value before any further libc calls.
    let t = secs as libc::time_t;
    let tm_ptr = unsafe { libc::localtime(&t) };
    let date = if tm_ptr.is_null() {
        // Fall back to epoch if localtime can't resolve (e.g., on extreme overflow).
        "1970-01-01-0000".to_string()
    } else {
        let tm = unsafe { *tm_ptr };
        format!(
            "{:04}-{:02}-{:02}-{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        )
    };
    format!("{date}-{final_game}")
}

/// Resolve filename collisions by appending `-2`, `-3`, ... before the extension.
/// Falls back to a PID suffix if 1000 candidates are all taken.
pub fn resolve_collision(dir: &Path, base: &str, ext: &str) -> String {
    let mut candidate = format!("{base}.{ext}");
    if !dir.join(&candidate).exists() {
        return candidate;
    }
    for n in 2..1000 {
        candidate = format!("{base}-{n}.{ext}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{base}-{}.{ext}", std::process::id())
}

const INDEX_FILENAME: &str = "clips_index.txt";

fn index_path() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home)
        .join(".config/arctis-chatmix")
        .join(INDEX_FILENAME)
}

/// Serialize a clip meta into one tab-separated index line.
pub fn serialize_meta(m: &ClipMeta) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        m.filename, m.duration_ms, m.game_name, m.created_unix, m.bitrate_kbps, m.resolution
    )
}

/// Parse one index line. Returns `None` on malformed input.
pub fn parse_meta(line: &str) -> Option<ClipMeta> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() != 6 {
        return None;
    }
    Some(ClipMeta {
        filename: parts[0].to_string(),
        duration_ms: parts[1].parse().ok()?,
        game_name: parts[2].to_string(),
        created_unix: parts[3].parse().ok()?,
        bitrate_kbps: parts[4].parse().ok()?,
        resolution: parts[5].to_string(),
    })
}

/// Load the on-disk clip index. Missing or unreadable file -> empty list.
/// Malformed lines are silently skipped.
pub fn load_index() -> Vec<ClipMeta> {
    let p = index_path();
    let s = std::fs::read_to_string(p).unwrap_or_default();
    s.lines().filter_map(parse_meta).collect()
}

/// Persist the clip index. Creates the parent directory if it doesn't exist.
pub fn save_index(items: &[ClipMeta]) -> std::io::Result<()> {
    let p = index_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body: String = items
        .iter()
        .map(serialize_meta)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(p, body)
}

/// Scan the storage dir, reconcile with the index, return the reconciled list.
/// - Removes index entries whose `.mp4` files no longer exist on disk.
/// - Adds entries for `.mp4` files not yet indexed (with default metadata —
///   ffprobe-augmented later in a worker thread).
pub fn reconcile(storage_dir: &Path) -> Vec<ClipMeta> {
    let mut indexed = load_index();
    let on_disk: std::collections::HashSet<String> = std::fs::read_dir(storage_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".mp4"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    indexed.retain(|m| on_disk.contains(&m.filename));
    let known: std::collections::HashSet<String> =
        indexed.iter().map(|m| m.filename.clone()).collect();
    for filename in &on_disk {
        if !known.contains(filename) {
            indexed.push(ClipMeta {
                filename: filename.clone(),
                duration_ms: 0,
                game_name: String::new(),
                created_unix: 0,
                bitrate_kbps: 0,
                resolution: String::new(),
            });
        }
    }
    indexed
}

/// Probe a media file's duration via `ffprobe`.
///
/// Returns `None` on any failure (binary not found, file unreadable, output
/// not parseable). Callers treat `None` as "skip this entry, try again
/// later" — no error propagation needed.
pub fn ffprobe_duration_ms(path: &Path) -> Option<u64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let secs: f64 = s.trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some((secs * 1000.0) as u64)
}

/// Fill missing `duration_ms` in the on-disk index by ffprobing each clip
/// whose entry currently has `duration_ms == 0`. Designed to be invoked
/// from a worker thread spawned at browser-open time.
///
/// **Update-propagation strategy:** the worker writes the updated index
/// to disk via [`save_index`] but does **not** signal the GTK side to
/// refresh the visible model. The next time `loaded_page()` is built
/// (next browser open or app restart) the new durations are picked up
/// via `reconcile()`. This avoids the complexity of a cross-thread
/// channel + visible-model rewrite for a piece of data the GridView
/// does not yet display in this chunk (durations land in Phase 5C/D
/// alongside the kebab menu and visual mockups). If a future task
/// renders durations on the cards, swap this for an mpsc + glib timer
/// or `MainContext::default().invoke()` callback.
pub fn backfill_durations(storage_dir: &Path) -> std::io::Result<()> {
    let mut idx = load_index();
    let mut changed = false;
    for m in idx.iter_mut() {
        if m.duration_ms == 0 {
            let p = storage_dir.join(&m.filename);
            if let Some(ms) = ffprobe_duration_ms(&p) {
                m.duration_ms = ms;
                changed = true;
            }
        }
    }
    if changed {
        save_index(&idx)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_normal_name() {
        assert_eq!(sanitize_game_name("Apex Legends"), "Apex-Legends");
    }

    #[test]
    fn sanitize_strips_special_chars() {
        assert_eq!(sanitize_game_name("ELDEN RING\u{2122} \u{00A9}"), "ELDEN-RING-");
    }

    #[test]
    fn sanitize_truncates_to_40_chars() {
        let s = sanitize_game_name(&"x".repeat(100));
        assert_eq!(s.len(), 40);
    }

    #[test]
    fn build_filename_includes_date_and_name() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1715000000);
        let f = build_base_filename(t, "Apex Legends");
        assert!(f.starts_with("20"));
        assert!(f.ends_with("-Apex-Legends"));
    }

    #[test]
    fn collision_appends_suffix() {
        let dir = std::env::temp_dir();
        let base = format!("collision-test-{}", std::process::id());
        let _ = std::fs::write(dir.join(format!("{base}.mp4")), b"x");
        let resolved = resolve_collision(&dir, &base, "mp4");
        assert_eq!(resolved, format!("{base}-2.mp4"));
        let _ = std::fs::remove_file(dir.join(format!("{base}.mp4")));
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;

    fn meta() -> ClipMeta {
        ClipMeta {
            filename: "2026-05-08-1934-Apex-Legends.mp4".into(),
            duration_ms: 60000,
            game_name: "Apex Legends".into(),
            created_unix: 1715000000,
            bitrate_kbps: 25000,
            resolution: "1920x1080".into(),
        }
    }

    #[test]
    fn round_trip_one_entry() {
        let m = meta();
        let line = serialize_meta(&m);
        let parsed = parse_meta(&line);
        assert_eq!(parsed, Some(m));
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse_meta("not enough tabs").is_none());
        assert!(parse_meta("a\tb\tc\td\te").is_none());
    }
}
