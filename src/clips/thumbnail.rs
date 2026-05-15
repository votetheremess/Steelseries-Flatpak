//! Thumbnail extraction via ffmpeg.

use std::path::{Path, PathBuf};
use std::process::Command;

const THUMB_W: u32 = 320;
const THUMB_H: u32 = 180;

/// Directory under the clips storage root where thumbnails are cached.
pub fn thumb_dir(storage_dir: &Path) -> PathBuf {
    storage_dir.join(".cache/thumbs")
}

/// Path to the thumbnail JPEG for a given clip filename.
/// e.g. `2026-05-08-1934-Apex.mp4` -> `<storage>/.cache/thumbs/2026-05-08-1934-Apex.jpg`.
pub fn thumb_path(storage_dir: &Path, clip_filename: &str) -> PathBuf {
    let stem = Path::new(clip_filename).file_stem().unwrap_or_default();
    thumb_dir(storage_dir).join(format!("{}.jpg", stem.to_string_lossy()))
}

/// Extract a 320x180 JPEG thumbnail at offset 1.0s into the clip.
/// Idempotent — if the thumb file already exists with non-zero size, skip.
/// Returns the path to the thumbnail (whether newly created or pre-existing).
pub fn ensure_thumbnail(storage_dir: &Path, clip_filename: &str) -> std::io::Result<PathBuf> {
    let thumb = thumb_path(storage_dir, clip_filename);
    if let Ok(m) = std::fs::metadata(&thumb) {
        if m.len() > 0 {
            return Ok(thumb);
        }
    }
    std::fs::create_dir_all(thumb_dir(storage_dir))?;
    let clip_path = storage_dir.join(clip_filename);
    let status = Command::new("ffmpeg")
        .args(["-y", "-ss", "1.0", "-i"])
        .arg(&clip_path)
        .args([
            "-vframes",
            "1",
            "-vf",
            &format!("scale={THUMB_W}:{THUMB_H}"),
            "-q:v",
            "4",
        ])
        .arg(&thumb)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("ffmpeg failed with status {status:?}"),
        ));
    }
    Ok(thumb)
}
