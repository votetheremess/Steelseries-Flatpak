//! Clip metadata index, directory scan, filename sanitization.

use std::path::Path;
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
