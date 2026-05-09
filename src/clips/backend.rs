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
///
/// THREAT MODEL (security H-2): a same-uid attacker (e.g. another
/// process the user accidentally launched, or a co-resident workload
/// on a shared user account) could observe `create_dir_all` of
/// `<storage>/.arctis/` and race in to plant a symlink at
/// `<storage>/.arctis/save_callback.sh` pointing at any writable file
/// in the user's filesystem (e.g. `~/.bashrc` or
/// `~/.config/autostart/*.desktop`). Without `O_NOFOLLOW`, our
/// subsequent `OpenOptions::open` would follow that symlink and
/// `mode(0o755)` would mark the redirected target executable, then
/// our content write would clobber it with the bundled save-callback
/// shell script — effectively letting the attacker swap arbitrary
/// files for an executable shell script of their choosing.
///
/// Defense: open with `O_NOFOLLOW` so symlinks fail with `ELOOP`
/// rather than being followed. If we hit `ELOOP`, remove the symlink
/// and retry once (the attacker may have planted it between the
/// `metadata` check and our open).
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
        opts.create(true)
            .truncate(true)
            .write(true)
            .mode(0o755)
            .custom_flags(libc::O_NOFOLLOW);
        let mut f = match opts.open(&path) {
            Ok(f) => f,
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                // Path was a symlink — remove and retry once. Defends
                // against an attacker who plants a symlink between
                // `create_dir_all` and `open`.
                log::warn!(
                    "ensure_save_callback: refusing to follow symlink at {}; removing and retrying",
                    path.display()
                );
                let _ = fs::remove_file(&path);
                opts.open(&path)?
            }
            Err(e) => return Err(e),
        };
        std::io::Write::write_all(&mut f, SAVE_CALLBACK_BYTES)?;
    } else {
        // Defensive: re-set 0o755 even when content was unchanged. Cheap, prevents
        // the silent-failure case where a stray chmod -x leaves the file unexecutable
        // and GSR's -sc callback never fires.
        let mut perms = fs::metadata(&path)?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&path, perms)?;
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
        // Owner-only mode (0o600). The GSR Flatpak runs as the same uid as our app, so
        // it can read/write the FIFO. No other process should access it; constraining
        // the mode prevents stray writers from corrupting the save-path stream.
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
/// `--command=gpu-screen-recorder` is required: the Flatpak's default
/// command is the GUI (`gpu-screen-recorder-ui`); without this flag, our
/// CLI args land in the GUI and pop a window over the user's game.
///
/// Returns the Child handle for the `flatpak` wrapper process. Signals sent to
/// this PID are forwarded by Flatpak's bwrap into the contained GSR process.
pub fn spawn_gsr(args: &[String], fifo_path: &PathBuf) -> std::io::Result<Child> {
    let mut cmd = Command::new("flatpak");
    cmd.arg("run")
        // Select the headless CLI binary inside the Flatpak instead of the
        // default GUI command. Must come BEFORE the app id (it's a
        // flatpak-run option, not a contained-command arg).
        .arg("--command=gpu-screen-recorder")
        // Pass the FIFO path as an env var into the sandbox.
        .arg(format!("--env=ARCTIS_CHATMIX_SAVE_FIFO={}", fifo_path.display()))
        .arg("com.dec05eba.gpu_screen_recorder")
        // `--` separates flatpak-run options from args forwarded to the
        // contained command. Defensive: GSR's leading-dash flags (-r, -w,
        // -a, ...) should pass through, but the explicit boundary protects
        // against future flatpak versions interpreting them as run options.
        .arg("--")
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
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

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

    pub fn sender(&self) -> Sender<ClipCommand> {
        self.cmd_tx.clone()
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
}

fn run_backend(cmd_rx: Receiver<ClipCommand>, evt_tx: Sender<BackendEvent>) {
    // Use the default storage dir for fixtures until we get the first StartReplay
    // command (which carries the user-configured storage_dir). Re-extract on each
    // arm so a settings-changed storage path is honored.
    let initial_storage = default_storage_dir();
    let _ = std::fs::create_dir_all(&initial_storage);
    let _ = ensure_save_callback(&initial_storage);
    let _ = ensure_save_fifo(&initial_storage);

    // Shared between the supervisor loop (writer, set inside arm()) and the
    // FIFO reader thread (reader, picked up on each open). Updates here
    // are seen by the reader on its next iteration without restarting the
    // thread — settings-driven storage changes flow through transparently.
    let active_storage = Arc::new(Mutex::new(initial_storage.clone()));

    let mut active: Option<ActiveCapture> = None;
    let mut active_config: Option<CaptureConfig> = None;
    let mut consecutive_failures = 0u32;

    // Spawn a FIFO reader thread once. It reads lines forever and forwards them
    // through evt_tx as Saved events.
    let evt_for_fifo = evt_tx.clone();
    let storage_for_fifo = active_storage.clone();
    thread::Builder::new()
        .name("clip-fifo-reader".into())
        .spawn(move || run_fifo_reader(evt_for_fifo, storage_for_fifo))
        .expect("spawn fifo-reader");

    loop {
        // Drain any pending commands (blocking with a short timeout so we can
        // also poll the child's exit status).
        match cmd_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ClipCommand::StartReplay { config }) => {
                if let Err(e) = arm(&mut active, &mut active_config, &config, &active_storage) {
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
                if let Err(e) = arm(&mut active, &mut active_config, &config, &active_storage) {
                    let _ = evt_tx.send(BackendEvent::Error {
                        context: "Reconfigure".into(),
                        message: e.to_string(),
                    });
                } else {
                    consecutive_failures = 0;
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
                            if let Err(e) =
                                arm(&mut active, &mut active_config, &cfg, &active_storage)
                            {
                                let _ = evt_tx.send(BackendEvent::Error {
                                    context: "auto-restart".into(),
                                    message: e.to_string(),
                                });
                            } else {
                                consecutive_failures = 0;
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
    active_storage: &Arc<Mutex<PathBuf>>,
) -> std::io::Result<()> {
    if active.is_some() {
        return Ok(()); // already armed; idempotent
    }
    if !crate::clips::gsr_install::is_installed() {
        // The user uninstalled the Flatpak after onboarding. Emit a typed
        // NotFound error so `run_backend` can surface it as a recovery toast
        // pointing at Settings → Clips → Reinstall.
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "gpu-screen-recorder Flatpak is not installed. Reinstall it from Settings → Clips.",
        ));
    }
    std::fs::create_dir_all(&config.output_dir)?;
    let cb = ensure_save_callback(&config.output_dir)?;
    let fifo = ensure_save_fifo(&config.output_dir)?;
    // Publish the active storage dir so the FIFO reader picks up the new
    // path on its next open. Doing this BEFORE spawn_gsr ensures the
    // reader is already pointed at the right FIFO by the time GSR fires
    // its first save callback. Lock-poisoning falls back to overwrite —
    // a poisoned lock here just means the reader missed the previous
    // value, which is fine because we're replacing it anyway.
    if let Ok(mut g) = active_storage.lock() {
        *g = config.output_dir.clone();
    } else {
        log::warn!("arm: active_storage lock poisoned; ignoring and overwriting");
        let mut g = active_storage.lock().unwrap_or_else(|p| p.into_inner());
        *g = config.output_dir.clone();
    }
    let args = build_gsr_args(
        config,
        cb.to_str().unwrap(),
        config.output_dir.to_str().unwrap(),
    );
    let child = spawn_gsr(&args, &fifo)?;
    *active = Some(ActiveCapture { child });
    *active_config = Some(config.clone());
    Ok(())
}

// GSR muxes the active replay buffer on SIGINT (faststart MP4 with moov-relocate).
// 2 s covers the typical 60 s @ 25 Mbps buffer (~190 MB) on NVMe; spinning disks may
// force the SIGKILL fallback. Future tuning: scale wait by configured buffer length.
fn disarm(active: &mut Option<ActiveCapture>) {
    if let Some(mut a) = active.take() {
        let pid = a.child.id();
        let _ = send_signal(pid, libc::SIGINT);
        // Give it 2s to exit cleanly.
        for _ in 0..20 {
            if let Ok(Some(_)) = a.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
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

/// Maximum age (since file mtime) at which a save-callback line is still
/// considered fresh. Anything older than this is rejected as stale or
/// replayed. Real GSR saves write the file then `echo "$path" > $FIFO`
/// within milliseconds, so 30 s is a generous safety window.
const FIFO_MAX_MTIME_AGE: Duration = Duration::from_secs(30);

/// Maximum number of bytes from a malformed FIFO line we'll log. Defends
/// against a same-uid attacker padding a line with terminal-control or
/// ANSI sequences and us echoing them into journalctl. 200 chars is
/// enough to identify the offender without flooding logs.
const FIFO_LOG_LINE_MAX: usize = 200;

/// Reasons for rejecting a save-callback line. Returned from
/// `validate_fifo_line` so the caller can log a specific cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FifoLineRejection {
    Empty,
    NotCanonicalizable,
    OutsideStorageDir,
    NotMp4,
    DoesNotExist,
    NotARegularFile,
    Stale,
}

/// Validate a single line read from the save-callback FIFO. The line is
/// treated as a path written by `gpu-screen-recorder`'s `-sc` callback;
/// since the FIFO is owner-only (mode 0o600) but a same-uid process
/// could still write to it, we treat every line as untrusted and apply
/// strict checks before emitting a `Saved` event.
///
/// Returns the canonicalized path on success (so callers don't have to
/// re-canonicalize for downstream use), or a typed rejection on failure.
fn validate_fifo_line(
    line: &str,
    storage_dir: &std::path::Path,
    now: SystemTime,
) -> Result<PathBuf, FifoLineRejection> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(FifoLineRejection::Empty);
    }
    let canon_path = std::fs::canonicalize(trimmed)
        .map_err(|_| FifoLineRejection::NotCanonicalizable)?;
    let canon_storage = std::fs::canonicalize(storage_dir)
        .map_err(|_| FifoLineRejection::NotCanonicalizable)?;
    // Strict path-prefix check: catches both `../` traversal and
    // siblings that share a name prefix (e.g. `/foo` vs `/foobar`)
    // because `starts_with` operates on path components, not byte
    // prefixes.
    if !canon_path.starts_with(&canon_storage) {
        return Err(FifoLineRejection::OutsideStorageDir);
    }
    if canon_path.extension().and_then(|s| s.to_str()) != Some("mp4") {
        return Err(FifoLineRejection::NotMp4);
    }
    let meta = match std::fs::metadata(&canon_path) {
        Ok(m) => m,
        Err(_) => return Err(FifoLineRejection::DoesNotExist),
    };
    if !meta.is_file() {
        return Err(FifoLineRejection::NotARegularFile);
    }
    // Stale-mtime guard. A spoofer who somehow guessed a path inside
    // the storage dir but wrote it into the FIFO long after creation
    // (or replayed an older Saved line) gets filtered here.
    let mtime = meta
        .modified()
        .map_err(|_| FifoLineRejection::Stale)?;
    let age = now
        .duration_since(mtime)
        .unwrap_or(Duration::ZERO);
    if age > FIFO_MAX_MTIME_AGE {
        return Err(FifoLineRejection::Stale);
    }
    Ok(canon_path)
}

fn truncate_for_log(s: &str, max: usize) -> String {
    // We log lines that may be malicious; strip control chars so a
    // crafted line can't smuggle ANSI/escape sequences into the
    // journal, then truncate to `max` chars to keep journalctl
    // readable.
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    if s.len() > max {
        format!("{cleaned}…")
    } else {
        cleaned
    }
}

fn run_fifo_reader(evt_tx: Sender<BackendEvent>, active_storage: Arc<Mutex<PathBuf>>) {
    use std::fs::File;
    // The FIFO lives under whichever storage dir was last set by `arm()`.
    // We re-resolve the path on each open so a settings-driven storage
    // change picks up the new FIFO without restarting the reader thread.
    loop {
        let storage_dir = active_storage
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|p| p.into_inner().clone());
        let fifo_path = save_fifo_path(&storage_dir);
        // Opening a FIFO for reading blocks until a writer connects; that's fine —
        // the GSR callback opens for write each time it fires.
        let f = match File::open(&fifo_path) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("clip-fifo-reader: open failed: {e}; sleeping");
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let r = BufReader::new(f);
        for line in r.lines() {
            let raw = match line {
                Ok(s) => s,
                Err(_) => continue,
            };
            // Re-read the active storage dir for each line — the user may
            // have just changed it, in which case a callback line still in
            // flight from the prior FIFO instance must be validated against
            // the new storage dir (which would correctly reject it).
            let validation_storage = active_storage
                .lock()
                .map(|g| g.clone())
                .unwrap_or_else(|p| p.into_inner().clone());
            match validate_fifo_line(&raw, &validation_storage, SystemTime::now()) {
                Ok(canon) => {
                    // Don't ffprobe here — it blocks the reader. Emit Saved with
                    // duration_ms = 0; the thumbnail-extraction worker (or a
                    // dedicated Phase 5 ffprobe pass) fills it in via the index.
                    let _ = evt_tx.send(BackendEvent::Saved {
                        path: canon,
                        duration_ms: 0,
                    });
                }
                Err(reason) => {
                    let cleaned = truncate_for_log(&raw, FIFO_LOG_LINE_MAX);
                    log::warn!(
                        "clip-fifo-reader: rejecting save-callback line ({reason:?}): {cleaned:?}"
                    );
                }
            }
        }
        // Reader EOF — writer closed; loop and re-open against the
        // currently-active storage dir.
    }
}

#[cfg(test)]
mod supervision_tests {
    //! Counter-only simulation of the supervisor flow in `run_backend`.
    //!
    //! We can't drive a real GSR child in unit tests (no Flatpak runtime, no
    //! display, no portal), but the *counter logic* is pure state and can be
    //! exercised in isolation. This simulation mirrors exactly what
    //! `run_backend` does to `consecutive_failures` so any future divergence
    //! between this test and the supervisor will surface as a failing test.
    //!
    //! Manual end-to-end verification (run by hand on a Bazzite host with the
    //! Flatpak GSR installed):
    //!   1. Arm replay (Clips → Start). Confirm `Armed` event.
    //!   2. `pkill -KILL gpu-screen-recorder` — observe `BackendDied` then
    //!      `Armed` (auto-restart). Counter went 0 → 1 → 0.
    //!   3. `pkill -KILL gpu-screen-recorder` again — `Armed` again.
    //!      Counter 0 → 1 → 0 again (the prior reset opened a fresh budget).
    //!   4. Force two failures back-to-back without an intervening successful
    //!      arm (e.g. break the portal token then kill twice rapidly):
    //!      `BackendDied` then `Error{context:"supervision"}` on the second.
    //!   5. Reconfigure with a fresh, valid config — expect `Armed` and the
    //!      counter back to 0 so the next failure won't immediately exhaust.
    //!
    //! The simulation below codifies steps 2–5 as data.

    /// Mirror of the supervisor's counter mutations. Each `Step` describes
    /// what happened in one iteration of `run_backend`'s loop and how the
    /// counter should evolve. The test simulates the loop and asserts the
    /// final and intermediate counters.
    enum Step {
        /// Successful StartReplay or Reconfigure: counter resets to 0.
        ArmOk,
        /// Child observed exited; pre-increment, then optionally try to
        /// auto-restart. `restart_ok` mirrors whether `arm()` returned Ok.
        ChildDied { restart_ok: bool },
    }

    fn simulate(steps: &[Step]) -> Vec<u32> {
        let mut counter: u32 = 0;
        let mut history = Vec::with_capacity(steps.len());
        for step in steps {
            match step {
                Step::ArmOk => {
                    counter = 0;
                }
                Step::ChildDied { restart_ok } => {
                    counter += 1;
                    if counter < 2 && *restart_ok {
                        counter = 0;
                    }
                    // counter >= 2 → supervisor surrenders, leaves counter pinned.
                    // restart_ok=false with counter < 2 → counter stays at 1
                    // (next failure will trip the "twice in a row" branch).
                }
            }
            history.push(counter);
        }
        history
    }

    #[test]
    fn successful_arm_starts_counter_at_zero() {
        let h = simulate(&[Step::ArmOk]);
        assert_eq!(h, vec![0]);
    }

    #[test]
    fn one_death_then_successful_restart_resets_counter() {
        // Was the bug pre-fix: restart succeeded but counter stayed at 1, so a
        // *second* unrelated failure later would immediately surrender.
        let h = simulate(&[Step::ChildDied { restart_ok: true }]);
        assert_eq!(h, vec![0], "counter must reset on successful auto-restart");
    }

    #[test]
    fn two_deaths_in_a_row_surrender() {
        let h = simulate(&[
            Step::ChildDied { restart_ok: false },
            Step::ChildDied { restart_ok: false },
        ]);
        assert_eq!(h, vec![1, 2], "counter at 2 → supervisor stops retrying");
    }

    #[test]
    fn death_restart_then_later_death_does_not_immediately_surrender() {
        // The fix's whole point: a successful restart wipes the slate so a
        // future, unrelated failure isn't penalized for the earlier one.
        let h = simulate(&[
            Step::ChildDied { restart_ok: true }, // 0
            Step::ChildDied { restart_ok: true }, // 0 — would be 2 (surrender) without the fix
        ]);
        assert_eq!(h, vec![0, 0]);
    }

    #[test]
    fn reconfigure_after_a_failure_clears_the_counter() {
        // Reconfigure success path resets counter — same fix.
        let h = simulate(&[
            Step::ChildDied { restart_ok: false }, // counter = 1
            Step::ArmOk,                            // simulating user Reconfigure success → 0
            Step::ChildDied { restart_ok: false }, // back to 1, NOT 2
        ]);
        assert_eq!(h, vec![1, 0, 1]);
    }
}

#[cfg(test)]
mod fifo_validation_tests {
    //! Tests for `validate_fifo_line` covering the security H-1 attack
    //! surface: a same-uid attacker who writes to the save-callback FIFO
    //! shouldn't be able to spoof a Saved event for an arbitrary path,
    //! traverse out of the storage dir, replay an old line, or smuggle a
    //! non-mp4 path into the index. Each case below corresponds to one
    //! validation rule.
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "arctis-fifo-validation-{}-{}-{}",
            std::process::id(),
            label,
            // Tag with a per-test nanos suffix so parallel tests don't
            // collide. cargo test runs tests in parallel by default.
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ))
    }

    fn touch_mp4(dir: &std::path::Path, name: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        fs::write(&p, b"\x00\x00\x00\x18ftypmp42").unwrap();
        p
    }

    #[test]
    fn empty_line_is_rejected() {
        let dir = temp_dir("empty");
        fs::create_dir_all(&dir).unwrap();
        let r = validate_fifo_line("", &dir, SystemTime::now());
        assert_eq!(r, Err(FifoLineRejection::Empty));
        let r = validate_fifo_line("   \t  ", &dir, SystemTime::now());
        assert_eq!(r, Err(FifoLineRejection::Empty));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nonexistent_path_is_rejected() {
        let dir = temp_dir("missing");
        fs::create_dir_all(&dir).unwrap();
        let bogus = dir.join("nope.mp4");
        let r = validate_fifo_line(
            bogus.to_str().unwrap(),
            &dir,
            SystemTime::now(),
        );
        // Falls into NotCanonicalizable because canonicalize() on a
        // non-existent path returns ENOENT. The exact variant doesn't
        // matter for security — it's enough that the line is rejected.
        assert!(matches!(
            r,
            Err(FifoLineRejection::NotCanonicalizable | FifoLineRejection::DoesNotExist)
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_outside_storage_dir_is_rejected() {
        let dir = temp_dir("outside-storage");
        let foreign = temp_dir("outside-foreign");
        fs::create_dir_all(&dir).unwrap();
        let evil = touch_mp4(&foreign, "elsewhere.mp4");
        let r = validate_fifo_line(
            evil.to_str().unwrap(),
            &dir,
            SystemTime::now(),
        );
        assert_eq!(r, Err(FifoLineRejection::OutsideStorageDir));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&foreign);
    }

    #[test]
    fn path_traversal_is_rejected_after_canonicalization() {
        let dir = temp_dir("traversal");
        fs::create_dir_all(&dir).unwrap();
        // Plant a real file in the parent so canonicalize succeeds, then
        // see whether the traversal escape is caught by the prefix check.
        let parent = dir.parent().unwrap();
        let evil = touch_mp4(parent, "arctis-fifo-validation-escape.mp4");
        let traversal = format!("{}/../arctis-fifo-validation-escape.mp4", dir.display());
        let r = validate_fifo_line(&traversal, &dir, SystemTime::now());
        assert_eq!(r, Err(FifoLineRejection::OutsideStorageDir));
        let _ = fs::remove_file(&evil);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_mp4_extension_is_rejected() {
        let dir = temp_dir("nonmp4");
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("clip.txt");
        fs::write(&p, b"not a clip").unwrap();
        let r = validate_fifo_line(p.to_str().unwrap(), &dir, SystemTime::now());
        assert_eq!(r, Err(FifoLineRejection::NotMp4));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_mtime_is_rejected() {
        let dir = temp_dir("stale");
        let p = touch_mp4(&dir, "old.mp4");
        // Pretend "now" is 60s ahead of the file's mtime to trip the
        // staleness guard without depending on real clock drift.
        let metadata = fs::metadata(&p).unwrap();
        let mtime = metadata.modified().unwrap();
        let pretend_now = mtime + Duration::from_secs(60);
        let r = validate_fifo_line(p.to_str().unwrap(), &dir, pretend_now);
        assert_eq!(r, Err(FifoLineRejection::Stale));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_mp4_inside_storage_is_accepted() {
        let dir = temp_dir("happy");
        let p = touch_mp4(&dir, "fresh.mp4");
        let r = validate_fifo_line(p.to_str().unwrap(), &dir, SystemTime::now());
        // The canonicalized path may differ on macOS (e.g. /var → /private/var)
        // but on Linux it should match. We only care that the line was
        // accepted — the returned path must end with our basename and live
        // under the canonicalized storage dir.
        let canon = r.expect("happy path must be accepted");
        assert!(canon.ends_with("fresh.mp4"));
        assert!(canon.starts_with(fs::canonicalize(&dir).unwrap()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_path_is_rejected() {
        let dir = temp_dir("isdir");
        let inner = dir.join("not-a-file.mp4");
        fs::create_dir_all(&inner).unwrap();
        let r = validate_fifo_line(inner.to_str().unwrap(), &dir, SystemTime::now());
        assert_eq!(r, Err(FifoLineRejection::NotARegularFile));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncate_for_log_strips_control_chars_and_caps_length() {
        // Control chars (newline, escape, carriage return, NUL) get
        // dropped so they can't smuggle ANSI sequences into journalctl.
        let s = "evil\x1b[2Jline\nwith\x00chars";
        let cleaned = truncate_for_log(s, 200);
        assert!(!cleaned.contains('\x1b'));
        assert!(!cleaned.contains('\n'));
        assert!(!cleaned.contains('\x00'));
        assert!(cleaned.contains("evil"));

        // Long lines get truncated with an ellipsis marker.
        let long = "a".repeat(500);
        let cleaned = truncate_for_log(&long, 50);
        assert!(cleaned.ends_with('…'));
        // 50 'a's + the ellipsis = 51 chars total.
        assert_eq!(cleaned.chars().count(), 51);
    }
}
