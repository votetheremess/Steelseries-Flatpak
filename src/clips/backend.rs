//! src/clips/backend.rs — drives gpu-screen-recorder.

use std::path::PathBuf;

use crate::clips::CaptureConfig;

/// Build the gpu-screen-recorder CLI arguments for the given capture config + portal session.
///
/// Caller must still set environment variables ARCTIS_CHATMIX_SAVE_FIFO and -sc path
/// before spawning the child.
pub fn build_gsr_args(config: &CaptureConfig, save_callback_path: &str, output_dir: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-w".into(), "portal".into(),
        "-restore-portal-session".into(), "yes".into(),
        "-r".into(), config.buffer_secs.to_string(),
        "-bm".into(), "cbr".into(),
        "-q".into(), config.bitrate_mbps.to_string(),
        "-k".into(), "h264".into(),
        "-ac".into(), "aac".into(),
        "-f".into(), config.framerate.to_string(),
        "-o".into(), output_dir.into(),
        "-sc".into(), save_callback_path.into(),
        "-restart-replay-on-save".into(), "yes".into(),
    ];

    // Track 0: headset mix-down
    args.push("-a".into());
    args.push(format!("device:{}", config.headset_sink_monitor));

    if config.include_per_source_tracks {
        for sink_monitor in [
            "SteelSeries_Game.monitor",
            "SteelSeries_Chat.monitor",
            "SteelSeries_Music.monitor",
            "SteelSeries_Aux.monitor",
        ] {
            args.push("-a".into());
            args.push(format!("device:{sink_monitor}"));
        }
    }

    if config.include_mic {
        args.push("-a".into());
        args.push("device:SteelSeries_Mic.monitor".into());
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CaptureConfig {
        let mut c = CaptureConfig::default();
        c.headset_sink_monitor = "alsa_output.usb-headset.monitor".into();
        c.output_dir = PathBuf::from("/home/u/Videos/Clips");
        c
    }

    #[test]
    fn build_args_includes_replay_buffer_seconds() {
        let args = build_gsr_args(&cfg(), "/tmp/cb.sh", "/home/u/Videos/Clips");
        assert!(args.windows(2).any(|w| w[0] == "-r" && w[1] == "60"));
    }

    #[test]
    fn build_args_uses_cbr() {
        let args = build_gsr_args(&cfg(), "/tmp/cb.sh", "/home/u/Videos/Clips");
        assert!(args.windows(2).any(|w| w[0] == "-bm" && w[1] == "cbr"));
    }

    #[test]
    fn build_args_includes_six_audio_tracks_by_default() {
        let args = build_gsr_args(&cfg(), "/tmp/cb.sh", "/home/u/Videos/Clips");
        let count = args.iter().filter(|a| *a == "-a").count();
        assert_eq!(count, 6, "track 0 mix + 4 per-source + mic = 6");
    }

    #[test]
    fn build_args_omits_per_source_when_disabled() {
        let mut c = cfg();
        c.include_per_source_tracks = false;
        let args = build_gsr_args(&c, "/tmp/cb.sh", "/home/u/Videos/Clips");
        let count = args.iter().filter(|a| *a == "-a").count();
        assert_eq!(count, 2, "track 0 mix + mic = 2");
    }

    #[test]
    fn build_args_omits_mic_when_disabled() {
        let mut c = cfg();
        c.include_mic = false;
        let args = build_gsr_args(&c, "/tmp/cb.sh", "/home/u/Videos/Clips");
        let count = args.iter().filter(|a| *a == "-a").count();
        assert_eq!(count, 5, "track 0 mix + 4 per-source = 5");
    }

    #[test]
    fn build_args_passes_save_callback_path() {
        let args = build_gsr_args(&cfg(), "/tmp/cb.sh", "/home/u/Videos/Clips");
        assert!(args.windows(2).any(|w| w[0] == "-sc" && w[1] == "/tmp/cb.sh"));
    }
}

use std::fs;
use std::os::unix::fs::OpenOptionsExt;

const SAVE_CALLBACK_BYTES: &[u8] = include_bytes!("../../data/clips/save_callback.sh");

/// Default storage directory. Both the callback script and the FIFO live in
/// `<storage>/.arctis/` so the GSR Flatpak's `--filesystem=xdg-videos:create`
/// permission covers them without requiring any user-side `flatpak override`.
pub fn default_storage_dir() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home).join("Videos/Clips")
}

/// Hidden subdirectory inside the storage dir for save-callback fixtures.
fn fixtures_dir(storage_dir: &PathBuf) -> PathBuf {
    storage_dir.join(".arctis")
}

/// Path to the extracted save-callback script.
pub fn save_callback_path(storage_dir: &PathBuf) -> PathBuf {
    fixtures_dir(storage_dir).join("save_callback.sh")
}

/// Path to the FIFO the callback writes to.
pub fn save_fifo_path(storage_dir: &PathBuf) -> PathBuf {
    fixtures_dir(storage_dir).join("save.fifo")
}

/// Extract the bundled save-callback script into `<storage>/.arctis/`.
/// Idempotent (size-mismatch check, mirrors the HRIR/Lucide-icon pattern).
pub fn ensure_save_callback(storage_dir: &PathBuf) -> std::io::Result<PathBuf> {
    let dir = fixtures_dir(storage_dir);
    fs::create_dir_all(&dir)?;
    let path = save_callback_path(storage_dir);
    let needs_write = match fs::metadata(&path) {
        Ok(m) => m.len() != SAVE_CALLBACK_BYTES.len() as u64,
        Err(_) => true,
    };
    if needs_write {
        let mut opts = fs::OpenOptions::new();
        opts.create(true).truncate(true).write(true).mode(0o755);
        let mut f = opts.open(&path)?;
        std::io::Write::write_all(&mut f, SAVE_CALLBACK_BYTES)?;
    }
    Ok(path)
}

/// Create the save FIFO inside `<storage>/.arctis/` if it doesn't already exist.
/// Idempotent.
pub fn ensure_save_fifo(storage_dir: &PathBuf) -> std::io::Result<PathBuf> {
    let path = save_fifo_path(storage_dir);
    fs::create_dir_all(path.parent().unwrap())?;
    if !path.exists() {
        // mkfifo via libc — no shell-out for a single syscall.
        let cstr = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let rc = unsafe { libc::mkfifo(cstr.as_ptr(), 0o600) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(path)
}

#[cfg(test)]
mod fixture_tests {
    use super::*;

    fn temp_storage_dir() -> PathBuf {
        std::env::temp_dir().join(format!("arctis-clips-test-{}", std::process::id()))
    }

    #[test]
    fn ensure_save_callback_creates_executable_script() {
        let dir = temp_storage_dir();
        let path = ensure_save_callback(&dir).expect("write callback");
        assert!(path.exists());
        let m = fs::metadata(&path).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(&m.permissions());
        assert_eq!(mode & 0o100, 0o100, "owner-execute bit must be set");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_save_callback_is_idempotent() {
        let dir = temp_storage_dir().join("idem");
        let p1 = ensure_save_callback(&dir).unwrap();
        let p2 = ensure_save_callback(&dir).unwrap();
        assert_eq!(p1, p2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_save_fifo_creates_fifo() {
        let dir = temp_storage_dir().join("fifo");
        let p = save_fifo_path(&dir);
        let _ = fs::remove_file(&p);
        let path = ensure_save_fifo(&dir).expect("create fifo");
        let m = fs::metadata(&path).unwrap();
        let ft = m.file_type();
        use std::os::unix::fs::FileTypeExt;
        assert!(ft.is_fifo());
        let _ = fs::remove_dir_all(&dir);
    }
}
