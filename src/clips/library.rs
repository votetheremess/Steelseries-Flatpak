//! Clip metadata index, directory scan, filename collision resolution.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipMeta {
    pub filename: String,
    pub duration_ms: u64,
    pub created_unix: u64,
    pub bitrate_kbps: u32,
    pub resolution: String,
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
///
/// Current (5-field) shape: `filename\tduration_ms\tcreated_unix\tbitrate_kbps\tresolution`.
/// The legacy 6-field shape included a `game_name` after `duration_ms` — see
/// [`parse_meta`] for the back-compat path.
pub fn serialize_meta(m: &ClipMeta) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        m.filename, m.duration_ms, m.created_unix, m.bitrate_kbps, m.resolution
    )
}

/// Parse one index line. Returns `None` on malformed input.
///
/// Accepts both the current 5-field shape and the legacy 6-field shape
/// (`filename\tduration_ms\tgame_name\tcreated_unix\tbitrate_kbps\tresolution`)
/// produced by older builds. The `game_name` field is silently dropped
/// when present.
pub fn parse_meta(line: &str) -> Option<ClipMeta> {
    let parts: Vec<&str> = line.split('\t').collect();
    match parts.len() {
        5 => Some(ClipMeta {
            filename: parts[0].to_string(),
            duration_ms: parts[1].parse().ok()?,
            created_unix: parts[2].parse().ok()?,
            bitrate_kbps: parts[3].parse().ok()?,
            resolution: parts[4].to_string(),
        }),
        6 => Some(ClipMeta {
            filename: parts[0].to_string(),
            duration_ms: parts[1].parse().ok()?,
            // parts[2] = legacy game_name; dropped.
            created_unix: parts[3].parse().ok()?,
            bitrate_kbps: parts[4].parse().ok()?,
            resolution: parts[5].to_string(),
        }),
        _ => None,
    }
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

/// The set of `.mp4` filenames (basenames, non-recursive) directly under
/// `storage_dir`. Returns an empty set if the directory can't be read.
///
/// Factored out so the poll-while-visible live-refresh in `app.rs` can cheaply
/// detect add/remove changes (compare two of these sets) without rebuilding
/// the whole index, and `reconcile` can reuse the same scan.
pub fn current_mp4_set(storage_dir: &Path) -> std::collections::HashSet<String> {
    std::fs::read_dir(storage_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".mp4"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Scan the storage dir, reconcile with the index, return the reconciled list.
/// - Removes index entries whose `.mp4` files no longer exist on disk.
/// - Adds entries for `.mp4` files not yet indexed (with default metadata —
///   ffprobe-augmented later in a worker thread).
pub fn reconcile(storage_dir: &Path) -> Vec<ClipMeta> {
    let mut indexed = load_index();
    let on_disk = current_mp4_set(storage_dir);

    let indexed_before = indexed.len();
    indexed.retain(|m| on_disk.contains(&m.filename));
    let retained = indexed.len();
    let dropped = indexed_before - retained;
    let known: std::collections::HashSet<String> =
        indexed.iter().map(|m| m.filename.clone()).collect();
    let mut added = 0usize;
    for filename in &on_disk {
        if !known.contains(filename) {
            indexed.push(ClipMeta {
                filename: filename.clone(),
                duration_ms: 0,
                created_unix: 0,
                bitrate_kbps: 0,
                resolution: String::new(),
            });
            added += 1;
        }
    }
    log::info!(
        "[clip-lib] reconcile({}): {} mp4 on disk, {} index entries retained, {} dropped (file gone), {} new added -> {} total",
        storage_dir.display(),
        on_disk.len(),
        retained,
        dropped,
        added,
        indexed.len()
    );
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

/// Total bytes occupied by all `.mp4` files directly under `storage_dir`.
///
/// Thumbnails (which live in `storage_dir/.cache/thumbs/*.jpg`) are excluded
/// — `read_dir` is non-recursive so they're naturally skipped, but the
/// `.mp4` filter also makes the intent explicit. Returns 0 if the directory
/// can't be read at all (cleaner than panicking; the caller's retention
/// loop is a no-op when the total is already under the cap).
pub fn total_bytes(storage_dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(storage_dir) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".mp4") {
                if let Ok(m) = entry.metadata() {
                    total += m.len();
                }
            }
        }
    }
    total
}

/// Enforce the disk-cap by deleting oldest clips (by mtime) until the total
/// is under the cap. Returns the list of deleted filenames so the caller can
/// log it / refresh any open browser views.
///
/// Both the `.mp4` and its matching `.cache/thumbs/<stem>.jpg` are removed.
/// If the deletion of the .mp4 itself fails (permissions, FS error) we skip
/// the entry and continue with the next-oldest — partial progress is better
/// than no progress, and the caller's index reconcile picks up the missing
/// file on next browser open.
///
/// Note: this does NOT update the on-disk index file (`clips_index.txt`);
/// `reconcile()` removes index entries whose `.mp4` files no longer exist,
/// so the next browser open / app launch picks up the deletion automatically.
pub fn enforce_retention(storage_dir: &Path, cap_gb: u32) -> Vec<String> {
    let cap_bytes = (cap_gb as u64) * 1024 * 1024 * 1024;
    let mut total = total_bytes(storage_dir);
    if total <= cap_bytes {
        return vec![];
    }

    let mut entries: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(storage_dir) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".mp4") {
                if let Ok(m) = entry.metadata() {
                    if let Ok(t) = m.modified() {
                        entries.push((t, entry.path(), m.len()));
                    }
                }
            }
        }
    }
    entries.sort_by_key(|(t, _, _)| *t);

    let mut deleted = Vec::new();
    for (_, path, size) in entries {
        if total <= cap_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            // Also remove the matching thumbnail. Failure is silent — a
            // stale thumb is harmless; reconcile() will eventually drop it.
            if let Some(stem) = path.file_stem() {
                let thumb = storage_dir
                    .join(".cache/thumbs")
                    .join(format!("{}.jpg", stem.to_string_lossy()));
                let _ = std::fs::remove_file(thumb);
            }
            deleted.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            total = total.saturating_sub(size);
        }
    }
    deleted
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

    fn unique_set_dir(suffix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "arctis-mp4set-test-{}-{}-{}",
            std::process::id(),
            nanos,
            suffix
        ))
    }

    #[test]
    fn current_mp4_set_filters_to_mp4_only() {
        let dir = unique_set_dir("filter");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.mp4"), b"x").unwrap();
        std::fs::write(dir.join("b.mp4"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("thumb.jpg"), b"x").unwrap();
        let set = current_mp4_set(&dir);
        assert_eq!(set.len(), 2, "only the two .mp4 files: {set:?}");
        assert!(set.contains("a.mp4"));
        assert!(set.contains("b.mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_mp4_set_missing_dir_is_empty() {
        let dir = unique_set_dir("missing").join("does-not-exist");
        assert!(current_mp4_set(&dir).is_empty());
    }

    #[test]
    fn current_mp4_set_detects_add_and_remove() {
        // The poll-while-visible refresh fires only when this set changes
        // (added OR removed files). Verify set inequality catches both.
        let dir = unique_set_dir("diff");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.mp4"), b"x").unwrap();
        let before = current_mp4_set(&dir);

        // Add a file -> set differs.
        std::fs::write(dir.join("b.mp4"), b"x").unwrap();
        let after_add = current_mp4_set(&dir);
        assert_ne!(before, after_add, "adding a clip must change the set");

        // Remove a file -> set differs again.
        std::fs::remove_file(dir.join("a.mp4")).unwrap();
        let after_remove = current_mp4_set(&dir);
        assert_ne!(after_add, after_remove, "deleting a clip must change the set");

        // No change -> sets equal (refresh would be skipped).
        let again = current_mp4_set(&dir);
        assert_eq!(after_remove, again, "no fs change must yield an equal set");
        let _ = std::fs::remove_dir_all(&dir);
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
            filename: "2026-05-08-1934-clip.mp4".into(),
            duration_ms: 60000,
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
        // 4 fields — too few.
        assert!(parse_meta("a\tb\tc\td").is_none());
        // 7 fields — too many.
        assert!(parse_meta("a\tb\tc\td\te\tf\tg").is_none());
    }

    /// Legacy 6-field shape (pre-deletion of game-detection): the
    /// `game_name` field was the 3rd column. The current parser must
    /// silently drop that field so a pre-existing `clips_index.txt` from
    /// an earlier build still loads after upgrading.
    #[test]
    fn parse_meta_accepts_legacy_6_field_lines() {
        let legacy = "2026-05-08-1934-Apex-Legends.mp4\t60000\tApex Legends\t1715000000\t25000\t1920x1080";
        let m = parse_meta(legacy).expect("legacy 6-field line must parse");
        assert_eq!(m.filename, "2026-05-08-1934-Apex-Legends.mp4");
        assert_eq!(m.duration_ms, 60000);
        assert_eq!(m.created_unix, 1715000000);
        assert_eq!(m.bitrate_kbps, 25000);
        assert_eq!(m.resolution, "1920x1080");
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    /// Returns a fresh per-test temp dir with PID + a nanosecond-tagged
    /// suffix so parallel test threads, prior phases, and `cargo test`
    /// reruns can't collide on the same path.
    fn unique_dir(suffix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "arctis-retention-test-{}-{}-{}",
            std::process::id(),
            nanos,
            suffix
        ))
    }

    #[test]
    fn enforce_retention_skips_when_under_cap() {
        let dir = unique_dir("under");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.mp4"), vec![0u8; 1024]).unwrap();
        let deleted = enforce_retention(&dir, 1);
        assert!(deleted.is_empty(), "no deletions expected when under cap");
        assert!(dir.join("a.mp4").exists(), "file should still exist");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enforce_retention_deletes_oldest_first() {
        let dir = unique_dir("over");
        let _ = std::fs::create_dir_all(&dir);
        // Write three 500MB sparse files. set_len() doesn't allocate
        // physical blocks on tmpfs/ext4, so this is essentially free.
        for name in ["a.mp4", "b.mp4", "c.mp4"] {
            let f = std::fs::File::create(dir.join(name)).unwrap();
            f.set_len(500 * 1024 * 1024).unwrap();
            // Bump mtimes so a < b < c. 10 ms is comfortably above any
            // FS mtime resolution we're likely to encounter (ext4 = ns,
            // tmpfs = ns, FAT = 2 s — but no test runs on FAT).
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let deleted = enforce_retention(&dir, 1);
        assert!(
            deleted.contains(&"a.mp4".to_string()),
            "oldest (a.mp4) should be deleted, got {deleted:?}"
        );
        assert!(
            !dir.join("a.mp4").exists(),
            "a.mp4 should be gone after retention"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
