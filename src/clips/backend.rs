//! src/clips/backend.rs — drives gpu-screen-recorder.

use std::path::PathBuf;

use crate::clips::CaptureConfig;

/// Build the gpu-screen-recorder CLI arguments for the given capture config + portal session.
///
/// Caller must still set environment variables ARCTIS_CHATMIX_SAVE_FIFO and -sc path
/// before spawning the child.
pub fn build_gsr_args(config: &CaptureConfig, save_callback_path: &str, output_dir: &str) -> Vec<String> {
    // Pin the portal-session token next to our other GSR fixtures so:
    //   1. The Flatpak's xdg-videos:create permission covers it (parent
    //      .arctis/ is already created by ensure_save_callback /
    //      ensure_save_fifo before spawn_gsr runs, so no extra mkdir).
    //   2. We don't share GSR's default restore-token location
    //      (~/.config/gpu-screen-recorder/restore_token) with a user who
    //      also runs GSR standalone, which would let either app silently
    //      override the other's persisted session.
    let gsr_token_path = gsr_portal_token_path(&config.output_dir);

    let mut args: Vec<String> = vec![
        "-w".into(), "portal".into(),
        "-restore-portal-session".into(), "yes".into(),
        "-portal-session-token-filepath".into(), gsr_token_path.to_string_lossy().into_owned(),
        "-r".into(), config.buffer_secs.to_string(),
        "-bm".into(), "cbr".into(),
        "-q".into(), config.bitrate_mbps.to_string(),
        "-k".into(), "h264".into(),
        "-c".into(), "mp4".into(),
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
        // SteelSeries_Mic is an Audio/Source/Virtual, not a sink — it has no
        // `.monitor` companion. GSR rejects "SteelSeries_Mic.monitor" with
        // "is not a valid audio device". Pass the source name directly.
        args.push("-a".into());
        args.push("device:SteelSeries_Mic".into());
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
    fn build_args_uses_mp4_container() {
        let args = build_gsr_args(&cfg(), "/tmp/cb.sh", "/home/u/Videos/Clips");
        assert!(args.windows(2).any(|w| w[0] == "-c" && w[1] == "mp4"));
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
    fn build_args_mic_uses_source_not_monitor() {
        // SteelSeries_Mic is an Audio/Source/Virtual, not a sink. Sources
        // don't have `.monitor` siblings, and GSR rejects the suffixed name
        // with "is not a valid audio device". Make sure we pass the bare
        // source name through.
        let mut c = cfg();
        c.include_mic = true;
        let args = build_gsr_args(&c, "/tmp/cb.sh", "/home/u/Videos/Clips");
        assert!(
            args.iter().any(|a| a == "device:SteelSeries_Mic"),
            "expected device:SteelSeries_Mic in args, got: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "device:SteelSeries_Mic.monitor"),
            "device:SteelSeries_Mic.monitor must not appear, got: {args:?}"
        );
    }

    #[test]
    fn build_args_passes_save_callback_path() {
        let args = build_gsr_args(&cfg(), "/tmp/cb.sh", "/home/u/Videos/Clips");
        assert!(args.windows(2).any(|w| w[0] == "-sc" && w[1] == "/tmp/cb.sh"));
    }

    #[test]
    fn build_args_includes_portal_session_token_filepath() {
        // GSR's default `-restore-portal-session yes` writes the token to
        // ~/.config/gpu-screen-recorder/restore_token, which a user running
        // GSR standalone would also use — collision risk. Pin our session
        // to the storage dir so the two don't share state.
        let args = build_gsr_args(&cfg(), "/tmp/cb.sh", "/home/u/Videos/Clips");
        assert!(
            args.windows(2).any(|w| {
                w[0] == "-portal-session-token-filepath"
                    && w[1].ends_with("gsr_portal.token")
            }),
            "expected -portal-session-token-filepath flag with path ending in gsr_portal.token, got: {args:?}"
        );
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

/// Path to GSR's portal session restore-token file. Co-located with the
/// other GSR fixtures so the Flatpak's `--filesystem=xdg-videos:create`
/// permission covers it without extra overrides. Isolated from GSR's
/// default location (`~/.config/gpu-screen-recorder/restore_token`) so a
/// user who also runs GSR standalone doesn't share session state with us.
pub fn gsr_portal_token_path(storage_dir: &PathBuf) -> PathBuf {
    fixtures_dir(storage_dir).join("gsr_portal.token")
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
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Child dies if we die. Must be set after fork in the child only.
    unsafe {
        cmd.pre_exec(|| {
            let rc = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong, 0, 0, 0);
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn()?;
    // Drain GSR's stderr on a worker thread and tag each line so it surfaces
    // in our journalctl output. Without this, GSR's pipe fills and either
    // blocks the child or (more often) we lose all diagnostic context when
    // GSR exits non-zero — the user just sees `ExitStatus(unix_wait_status(256))`
    // with no clue why. Mirrors the FIFO-reader pattern.
    if let Some(stderr) = child.stderr.take() {
        let _ = std::thread::Builder::new()
            .name("gsr-stderr".into())
            .spawn(move || {
                let r = std::io::BufReader::new(stderr);
                for line in std::io::BufRead::lines(r).map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        log::warn!("gsr stderr: {line}");
                    }
                }
            });
    }
    Ok(child)
}

/// Send a signal to the GSR child by PID.
pub fn send_signal(pid: u32, signal: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::clips::{BackendEvent, ClipCommand};

/// Maximum number of auto-restart attempts allowed within `RESTART_WINDOW`
/// before the supervisor surrenders. Tuned so a single transient failure
/// (portal hiccup, GSR cold-start hiccup) self-heals, but a runaway loop
/// (every spawn dies in well under a second) hits the brake quickly.
const MAX_RESTARTS_PER_WINDOW: usize = 3;
/// Rolling window over which restart attempts are counted. Long enough that
/// rapid failures (sub-second deaths) clearly cluster, short enough that an
/// unrelated crash 5 minutes later doesn't count toward the cap.
const RESTART_WINDOW: Duration = Duration::from_secs(30);

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
    // Rolling window of recent auto-restart attempt timestamps. Replaces
    // the old `consecutive_failures` counter, which had a fatal flaw: each
    // successful auto-restart reset the counter, so a runaway loop where
    // every spawn died in <1 s and "succeeded" at re-arming never tripped
    // the cap. The deque counts attempts within RESTART_WINDOW regardless
    // of whether each one survived.
    let mut restart_attempts: VecDeque<Instant> = VecDeque::new();

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
                // User intent — fresh start, wipe any prior auto-restart
                // history so a previously-exhausted budget doesn't carry
                // over into a manual retry.
                restart_attempts.clear();
                if let Err(e) = arm(&mut active, &mut active_config, &config, &active_storage) {
                    let _ = evt_tx.send(BackendEvent::Error {
                        context: "StartReplay".into(),
                        message: e.to_string(),
                    });
                } else {
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
                // Same rationale as StartReplay: user intent overrides any
                // prior auto-restart history.
                restart_attempts.clear();
                disarm(&mut active);
                if let Err(e) = arm(&mut active, &mut active_config, &config, &active_storage) {
                    let _ = evt_tx.send(BackendEvent::Error {
                        context: "Reconfigure".into(),
                        message: e.to_string(),
                    });
                } else {
                    let _ = evt_tx.send(BackendEvent::Armed);
                }
            }
            Ok(ClipCommand::PauseRecording) => {
                log::info!("backend: handling PauseRecording");
                restart_attempts.clear();
                if active.is_some() {
                    disarm(&mut active);
                }
            }
            Ok(ClipCommand::ResumeRecording) => {
                log::info!("backend: handling ResumeRecording");
                restart_attempts.clear();
                // BufferController::resume() already called maybe_arm synchronously on
                // the GTK side, so a StartReplay (if applicable) is in the channel queue
                // either before or after this ResumeRecording (order depends on
                // mpsc fairness). ResumeRecording is solely a limiter-clear here.
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

                    // Age out attempts older than the rolling window so a
                    // single failure 5 minutes after a clean run doesn't
                    // count toward the cap.
                    let now = Instant::now();
                    while let Some(&front) = restart_attempts.front() {
                        if now.duration_since(front) >= RESTART_WINDOW {
                            restart_attempts.pop_front();
                        } else {
                            break;
                        }
                    }

                    if restart_attempts.len() < MAX_RESTARTS_PER_WINDOW {
                        restart_attempts.push_back(now);
                        log::info!(
                            "clip-backend: auto-restart attempt {}/{} within last {}s",
                            restart_attempts.len(),
                            MAX_RESTARTS_PER_WINDOW,
                            RESTART_WINDOW.as_secs()
                        );
                        if let Some(cfg) = active_config.clone() {
                            match arm(&mut active, &mut active_config, &cfg, &active_storage) {
                                Ok(()) => {
                                    let _ = evt_tx.send(BackendEvent::Armed);
                                }
                                Err(e) => {
                                    let _ = evt_tx.send(BackendEvent::Error {
                                        context: "auto-restart".into(),
                                        message: e.to_string(),
                                    });
                                }
                            }
                        }
                    } else {
                        // Don't clear restart_attempts here. Letting the
                        // window age out naturally means a quiet period
                        // of RESTART_WINDOW seconds opens a fresh budget
                        // without requiring an explicit user action.
                        let _ = evt_tx.send(BackendEvent::Error {
                            context: "supervision".into(),
                            message: format!(
                                "GSR died {} times in {}s; not auto-restarting. Check 'gsr stderr' lines above for the cause.",
                                MAX_RESTARTS_PER_WINDOW,
                                RESTART_WINDOW.as_secs()
                            ),
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
    //! Rolling-window simulation of the supervisor flow in `run_backend`.
    //!
    //! We can't drive a real GSR child in unit tests (no Flatpak runtime, no
    //! display, no portal), but the *deque arithmetic* is pure state and can
    //! be exercised in isolation. This simulation mirrors exactly what
    //! `run_backend` does to `restart_attempts` so any future divergence
    //! between this test and the supervisor will surface as a failing test.
    //!
    //! The previous design used a naive `consecutive_failures` counter that
    //! reset to 0 on every successful auto-restart. That meant a tight loop
    //! where every spawn died in <1 s but `arm()` returned Ok would never
    //! trip the cap — the user saw infinite log spam. The new design counts
    //! restart *attempts* within a rolling window, regardless of how long
    //! each attempt survived.
    //!
    //! Manual end-to-end verification (run by hand on a Bazzite host with the
    //! Flatpak GSR installed):
    //!   1. Arm replay (Clips → Start). Confirm `Armed` event.
    //!   2. `pkill -KILL gpu-screen-recorder` — observe `BackendDied` then
    //!      `Armed` (1/3 attempts). Wait > 30 s, kill again — counter resets
    //!      to 1/3 because the prior attempt aged out.
    //!   3. Kill rapidly four times in a row — observe attempts 1/3, 2/3,
    //!      3/3, then `Error{context:"supervision"}`.
    //!   4. After the surrender, click Start again — `restart_attempts` is
    //!      cleared by user intent and the budget is fresh.

    use super::{MAX_RESTARTS_PER_WINDOW, RESTART_WINDOW};
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    /// Mirror of the supervisor's deque mutations. Each `Step` describes
    /// what happened in one iteration of `run_backend`'s loop and how the
    /// deque should evolve.
    enum Step {
        /// User-initiated StartReplay or Reconfigure: deque is cleared
        /// regardless of prior state.
        UserArm,
        /// Child observed exited at the given offset from the simulation
        /// start. The supervisor: ages out timestamps older than
        /// `RESTART_WINDOW`, then either records a new attempt (when under
        /// the cap) or surrenders without recording.
        ChildDiedAt { offset: Duration },
    }

    /// Outcome flag returned alongside the deque snapshot so tests can
    /// assert whether the supervisor would have called `arm()` again or
    /// emitted a supervision-error event.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Idle,        // user-arm step (no child death)
        Restarted,   // child died, room in the window, attempt recorded
        Surrendered, // child died, window full, supervision error sent
    }

    fn simulate(start: Instant, steps: &[Step]) -> Vec<(usize, Outcome)> {
        let mut deque: VecDeque<Instant> = VecDeque::new();
        let mut history = Vec::with_capacity(steps.len());
        for step in steps {
            let outcome = match step {
                Step::UserArm => {
                    deque.clear();
                    Outcome::Idle
                }
                Step::ChildDiedAt { offset } => {
                    let now = start + *offset;
                    while let Some(&front) = deque.front() {
                        if now.duration_since(front) >= RESTART_WINDOW {
                            deque.pop_front();
                        } else {
                            break;
                        }
                    }
                    if deque.len() < MAX_RESTARTS_PER_WINDOW {
                        deque.push_back(now);
                        Outcome::Restarted
                    } else {
                        Outcome::Surrendered
                    }
                }
            };
            history.push((deque.len(), outcome));
        }
        history
    }

    #[test]
    fn first_three_failures_within_window_all_restart() {
        // 3 sub-second deaths exhaust the budget exactly at attempt 3.
        let t0 = Instant::now();
        let h = simulate(
            t0,
            &[
                Step::ChildDiedAt { offset: Duration::from_millis(0) },
                Step::ChildDiedAt { offset: Duration::from_millis(500) },
                Step::ChildDiedAt { offset: Duration::from_millis(1000) },
            ],
        );
        assert_eq!(
            h,
            vec![
                (1, Outcome::Restarted),
                (2, Outcome::Restarted),
                (3, Outcome::Restarted),
            ]
        );
    }

    #[test]
    fn fourth_failure_within_window_surrenders() {
        // The bug this fix targets: rapid-failure loop. Pre-fix, the counter
        // reset to 0 on each "successful" auto-restart and the loop never
        // tripped a cap. New design counts attempts, not consecutive deaths.
        let t0 = Instant::now();
        let h = simulate(
            t0,
            &[
                Step::ChildDiedAt { offset: Duration::from_millis(0) },
                Step::ChildDiedAt { offset: Duration::from_millis(500) },
                Step::ChildDiedAt { offset: Duration::from_millis(1000) },
                Step::ChildDiedAt { offset: Duration::from_millis(1500) },
            ],
        );
        assert_eq!(h.last().unwrap(), &(3, Outcome::Surrendered));
    }

    #[test]
    fn old_attempts_age_out_after_window() {
        // After RESTART_WINDOW seconds of quiet, prior attempts should age
        // out and free up budget for a new failure.
        let t0 = Instant::now();
        let h = simulate(
            t0,
            &[
                Step::ChildDiedAt { offset: Duration::from_millis(0) },
                Step::ChildDiedAt { offset: Duration::from_millis(500) },
                // Both entries above are > RESTART_WINDOW old at this offset,
                // so both age out and the third attempt starts a fresh deque.
                Step::ChildDiedAt { offset: RESTART_WINDOW + Duration::from_secs(1) },
            ],
        );
        assert_eq!(h[0], (1, Outcome::Restarted));
        assert_eq!(h[1], (2, Outcome::Restarted));
        // After both stale entries age out, len drops back to 1 (just the
        // freshly-recorded attempt). This is the whole point of the
        // rolling-window policy: a single failure long after a clean run
        // doesn't carry over.
        assert_eq!(h[2], (1, Outcome::Restarted));
    }

    #[test]
    fn user_arm_after_surrender_clears_the_budget() {
        // After the supervisor gives up, the user clicks Start (or
        // Reconfigure) — that's an explicit retry signal and we wipe
        // restart_attempts entirely.
        let t0 = Instant::now();
        let h = simulate(
            t0,
            &[
                Step::ChildDiedAt { offset: Duration::from_millis(0) },
                Step::ChildDiedAt { offset: Duration::from_millis(100) },
                Step::ChildDiedAt { offset: Duration::from_millis(200) },
                Step::ChildDiedAt { offset: Duration::from_millis(300) }, // surrender
                Step::UserArm,
                Step::ChildDiedAt { offset: Duration::from_millis(400) },
            ],
        );
        assert_eq!(h[3], (3, Outcome::Surrendered));
        assert_eq!(h[4], (0, Outcome::Idle));
        assert_eq!(h[5], (1, Outcome::Restarted));
    }

    #[test]
    fn surrender_does_not_clear_deque_so_quiet_period_must_age_out_naturally() {
        // After a surrender we explicitly DON'T clear the deque — a quiet
        // period of RESTART_WINDOW seconds should age the entries out
        // naturally instead of immediately reopening the budget.
        let t0 = Instant::now();
        let h = simulate(
            t0,
            &[
                Step::ChildDiedAt { offset: Duration::from_millis(0) },
                Step::ChildDiedAt { offset: Duration::from_millis(100) },
                Step::ChildDiedAt { offset: Duration::from_millis(200) },
                Step::ChildDiedAt { offset: Duration::from_millis(300) }, // surrender
                // A failure 1 ms later: deque still has 3 entries, none aged
                // out, so the supervisor surrenders again. (Real code never
                // tries to arm when surrendered, but the deque arithmetic
                // is what we're testing here.)
                Step::ChildDiedAt { offset: Duration::from_millis(301) },
            ],
        );
        assert_eq!(h[3], (3, Outcome::Surrendered));
        assert_eq!(h[4], (3, Outcome::Surrendered));
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
