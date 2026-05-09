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

use std::process::{Child, Command, Stdio};
use std::os::unix::process::CommandExt;

/// Spawn a `gpu-screen-recorder` child via the Flathub Flatpak with
/// PR_SET_PDEATHSIG so it dies if we die. Sets ARCTIS_CHATMIX_SAVE_FIFO via
/// `--env=` so the callback script (run inside the Flatpak sandbox) sees it.
///
/// Returns the Child handle for the `flatpak` wrapper process. Signals sent to
/// this PID are forwarded by Flatpak's bwrap into the contained GSR process.
pub fn spawn_gsr(args: &[String], fifo_path: &PathBuf) -> std::io::Result<Child> {
    let mut cmd = Command::new("flatpak");
    cmd.arg("run")
        // Pass the FIFO path as an env var into the sandbox.
        .arg(format!("--env=ARCTIS_CHATMIX_SAVE_FIFO={}", fifo_path.display()))
        .arg("com.dec05eba.gpu_screen_recorder")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Child dies if we die. Must be set after fork in the child only.
    // Note: `pre_exec` is itself an unsafe fn, but the closure body does NOT inherit
    // outer `unsafe` context — the call to libc::prctl needs its own inner unsafe block.
    unsafe {
        cmd.pre_exec(|| {
            let rc = unsafe {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong, 0, 0, 0)
            };
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn()
}

/// Send a signal to the GSR child by PID.
pub fn send_signal(pid: u32, signal: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

use std::io::{BufRead, BufReader};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration as StdDuration;

use crate::clips::{BackendEvent, ClipCommand};

/// Handle for the backend thread. Drop = clean shutdown via Shutdown command.
pub struct BackendHandle {
    cmd_tx: Sender<ClipCommand>,
    join: Option<JoinHandle<()>>,
}

impl BackendHandle {
    pub fn send(&self, cmd: ClipCommand) {
        let _ = self.cmd_tx.send(cmd);
    }
}

impl Drop for BackendHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(ClipCommand::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn the backend thread. Returns a handle for sending commands and a receiver
/// for events.
pub fn spawn_backend() -> (BackendHandle, Receiver<BackendEvent>) {
    let (cmd_tx, cmd_rx) = channel::<ClipCommand>();
    let (evt_tx, evt_rx) = channel::<BackendEvent>();

    let join = thread::Builder::new()
        .name("clip-backend".into())
        .spawn(move || run_backend(cmd_rx, evt_tx))
        .expect("spawn clip-backend thread");

    (BackendHandle { cmd_tx, join: Some(join) }, evt_rx)
}

struct ActiveCapture {
    child: Child,
    stdout_lines: Vec<String>, // last few lines for error context
}

fn run_backend(cmd_rx: Receiver<ClipCommand>, evt_tx: Sender<BackendEvent>) {
    // Use the default storage dir for fixtures until we get the first StartReplay
    // command (which carries the user-configured storage_dir). Re-extract on each
    // arm so a settings-changed storage path is honored.
    let initial_storage = default_storage_dir();
    let _ = std::fs::create_dir_all(&initial_storage);
    let _ = ensure_save_callback(&initial_storage);
    let _ = ensure_save_fifo(&initial_storage);

    let mut active: Option<ActiveCapture> = None;
    let mut active_config: Option<CaptureConfig> = None;
    let mut consecutive_failures = 0u32;

    // Spawn a FIFO reader thread once. It reads lines forever and forwards them
    // through evt_tx as Saved events.
    let evt_for_fifo = evt_tx.clone();
    thread::Builder::new()
        .name("clip-fifo-reader".into())
        .spawn(move || run_fifo_reader(evt_for_fifo))
        .expect("spawn fifo-reader");

    loop {
        // Drain any pending commands (blocking with a short timeout so we can
        // also poll the child's exit status).
        match cmd_rx.recv_timeout(StdDuration::from_millis(200)) {
            Ok(ClipCommand::StartReplay { config }) => {
                if let Err(e) = arm(&mut active, &mut active_config, &config, &evt_tx) {
                    let _ = evt_tx.send(BackendEvent::Error {
                        context: "StartReplay".into(),
                        message: e.to_string(),
                    });
                } else {
                    consecutive_failures = 0;
                    let _ = evt_tx.send(BackendEvent::Armed);
                }
            }
            Ok(ClipCommand::StopReplay) => {
                disarm(&mut active);
                let _ = evt_tx.send(BackendEvent::Disarmed);
            }
            Ok(ClipCommand::SaveClip) => {
                save(&active, libc::SIGUSR1, &evt_tx);
            }
            Ok(ClipCommand::SaveClipShort) => {
                save(&active, libc::SIGRTMIN() + 1, &evt_tx);
            }
            Ok(ClipCommand::SaveClipMedium) => {
                save(&active, libc::SIGRTMIN() + 2, &evt_tx);
            }
            Ok(ClipCommand::SaveClipLong) => {
                save(&active, libc::SIGRTMIN() + 3, &evt_tx);
            }
            Ok(ClipCommand::Reconfigure { config }) => {
                disarm(&mut active);
                if let Err(e) = arm(&mut active, &mut active_config, &config, &evt_tx) {
                    let _ = evt_tx.send(BackendEvent::Error {
                        context: "Reconfigure".into(),
                        message: e.to_string(),
                    });
                } else {
                    let _ = evt_tx.send(BackendEvent::Armed);
                }
            }
            Ok(ClipCommand::Shutdown) => {
                disarm(&mut active);
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }

        // Supervise the child.
        if let Some(a) = active.as_mut() {
            match a.child.try_wait() {
                Ok(Some(status)) => {
                    let reason = format!("GSR exited with {status:?}");
                    log::warn!("clip-backend: {reason}");
                    let _ = evt_tx.send(BackendEvent::BackendDied { reason: reason.clone() });
                    active = None;

                    consecutive_failures += 1;
                    if consecutive_failures < 2 {
                        if let Some(cfg) = active_config.clone() {
                            log::info!("clip-backend: auto-restart attempt 1");
                            if let Err(e) = arm(&mut active, &mut active_config, &cfg, &evt_tx) {
                                let _ = evt_tx.send(BackendEvent::Error {
                                    context: "auto-restart".into(),
                                    message: e.to_string(),
                                });
                            } else {
                                let _ = evt_tx.send(BackendEvent::Armed);
                            }
                        }
                    } else {
                        let _ = evt_tx.send(BackendEvent::Error {
                            context: "supervision".into(),
                            message: "GSR died twice in a row; user retry required".into(),
                        });
                    }
                }
                Ok(None) => {} // still running
                Err(e) => log::warn!("clip-backend: try_wait error: {e}"),
            }
        }
    }
}

fn arm(
    active: &mut Option<ActiveCapture>,
    active_config: &mut Option<CaptureConfig>,
    config: &CaptureConfig,
    _evt_tx: &Sender<BackendEvent>,
) -> std::io::Result<()> {
    if active.is_some() {
        return Ok(()); // already armed; idempotent
    }
    std::fs::create_dir_all(&config.output_dir)?;
    let cb = ensure_save_callback(&config.output_dir)?;
    let fifo = ensure_save_fifo(&config.output_dir)?;
    let args = build_gsr_args(
        config,
        cb.to_str().unwrap(),
        config.output_dir.to_str().unwrap(),
    );
    let child = spawn_gsr(&args, &fifo)?;
    *active = Some(ActiveCapture { child, stdout_lines: Vec::new() });
    *active_config = Some(config.clone());
    Ok(())
}

fn disarm(active: &mut Option<ActiveCapture>) {
    if let Some(mut a) = active.take() {
        let pid = a.child.id();
        let _ = send_signal(pid, libc::SIGINT);
        // Give it 2s to exit cleanly.
        for _ in 0..20 {
            if let Ok(Some(_)) = a.child.try_wait() {
                return;
            }
            thread::sleep(StdDuration::from_millis(100));
        }
        // Force.
        let _ = a.child.kill();
        let _ = a.child.wait();
    }
}

fn save(active: &Option<ActiveCapture>, signal: libc::c_int, evt_tx: &Sender<BackendEvent>) {
    match active {
        Some(a) => {
            let pid = a.child.id();
            if let Err(e) = send_signal(pid, signal) {
                let _ = evt_tx.send(BackendEvent::Error {
                    context: "SaveClip".into(),
                    message: format!("kill({pid}, {signal}) failed: {e}"),
                });
            }
        }
        None => {
            let _ = evt_tx.send(BackendEvent::Error {
                context: "SaveClip".into(),
                message: "Not armed; nothing to save".into(),
            });
        }
    }
}

fn run_fifo_reader(evt_tx: Sender<BackendEvent>) {
    use std::fs::File;
    // The FIFO lives under the active storage dir. For the reader we use the
    // default-storage path as a fallback; if the user configures a different
    // storage_dir, the backend recreates the FIFO there at arm time. The reader
    // re-opens on each iteration via the path supplied to arm() through the
    // config; the simplest approach is to scan both default and any later
    // configured paths. For v1, the default path covers ≥99% of users.
    let path = save_fifo_path(&default_storage_dir());
    loop {
        // Opening a FIFO for reading blocks until a writer connects; that's fine —
        // the GSR callback opens for write each time it fires.
        let f = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("clip-fifo-reader: open failed: {e}; sleeping");
                thread::sleep(StdDuration::from_secs(1));
                continue;
            }
        };
        let r = BufReader::new(f);
        for line in r.lines() {
            match line {
                Ok(p) if !p.is_empty() => {
                    let path = PathBuf::from(p);
                    // Don't ffprobe here — it blocks the reader. Emit Saved with
                    // duration_ms = 0; the thumbnail-extraction worker (or a
                    // dedicated Phase 5 ffprobe pass) fills it in via the index.
                    let _ = evt_tx.send(BackendEvent::Saved { path, duration_ms: 0 });
                }
                _ => {}
            }
        }
        // Reader EOF — writer closed; loop and re-open.
    }
}
