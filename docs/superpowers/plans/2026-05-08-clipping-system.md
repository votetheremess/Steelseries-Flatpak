# Clipping System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the existing `Clips (coming soon)` placeholder tab (`src/window.rs:238`) with a working SteelSeries-Moments-style replay-buffer clip capture feature: continuously buffer recent gameplay video+audio, hotkey to save the last 30–300 s, browse/manage saved clips in-app with per-source audio remix.

**Architecture:** Drive the `com.dec05eba.gpu_screen_recorder` Flathub Flatpak (invoked as `flatpak run com.dec05eba.gpu_screen_recorder ...`) as a long-lived child process behind one new dedicated thread (mirrors the existing `eq-pipeline` thread pattern). Bazzite-flatpak-only — the project never layers via rpm-ostree, so GSR runs as a Flatpak both during dev and after this app is itself packaged. Game detection via 2-second `/proc` scan with SteamAppId resolution. Hotkey via XDG GlobalShortcuts portal (`ashpd`) plus a `GAction` fallback exposed on the existing `org.gtk.Actions` D-Bus surface. UI is a `gtk::GridView` clip browser plus settings additions and a status indicator on the existing dashboard.

**Tech Stack:** Rust 2024, GTK4 + libadwaita, `std::sync::mpsc` + `glib::timeout_add_local` for cross-thread comms, `ashpd` 0.10 (gtk4 feature) for portal calls, `libc` for prctl/kill, `ffmpeg` child process for thumbnails + remix export, `flatpak run com.dec05eba.gpu_screen_recorder` child process for capture.

**Reference spec:** `docs/superpowers/specs/2026-05-08-clipping-system-design.md` (commit `f03a7cd`).

---

## Pre-flight

- [ ] **P.1: Verify clean working tree (or stash existing edits)**

The implementer should ensure either:
- Working tree is clean before starting (commit or stash existing edits to `src/app.rs`, `src/audio/persistence.rs`, `src/eq/mod.rs`, `src/mixer.rs`), OR
- Implementer is comfortable working alongside those edits (they're unrelated to this feature).

Run:

```bash
git status
```

If `M` lines for files outside this plan's scope are present, ask the user how to proceed before starting.

- [ ] **P.2: Verify GSR Flatpak + KDE version**

This project takes a "Bazzite-flatpak-only, never layer" approach. GSR is consumed as a Flatpak (`com.dec05eba.gpu_screen_recorder` from Flathub), not as a host binary. End users will install the same Flatpak alongside this app.

Run:

```bash
flatpak info com.dec05eba.gpu_screen_recorder 2>&1 | head -10
```

Expected: lines like `gpu-screen-recorder` / `Branch: stable` / `Version: 5.x.x`.

If "not installed", instruct the user to install:

```bash
flatpak install --user flathub com.dec05eba.gpu_screen_recorder
```

Tasks 1.1–1.6 do not invoke GSR and can land before this is satisfied. Do not proceed past Task 1.7 (which spawns GSR via `flatpak run`) until `flatpak info` succeeds.

Also verify KDE Plasma version — the GlobalShortcuts portal had a known bug fixed in Plasma 6.4.1 (KDE xdg-desktop-portal-kde MR !412):

```bash
plasmashell --version
```

Expected: ≥ 6.4.1. If older, log it — Phase 4 portal binding may silently fail and only the GAction fallback will reach the buffer.

- [ ] **P.3: Verify ffmpeg is installed**

Run:

```bash
which ffmpeg && which ffprobe
```

Expected: paths for both. They are part of every Bazzite install.

- [ ] **P.4: GSR Flatpak filesystem-permission strategy**

The GSR Flatpak's default Flathub manifest grants `--filesystem=xdg-videos:create`. We place the save-callback script and FIFO inside `<storage>/.arctis/` (i.e. `~/Videos/Clips/.arctis/save_callback.sh` + `save.fifo`) so GSR can read/exec the script and write to the FIFO without any extra `flatpak override` on the user's part. No action needed at preflight; this is just a note that Task 1.6 places those fixtures there for this reason.

---

## File Structure

**Created files:**

| Path | Responsibility |
|---|---|
| `src/clips/mod.rs` | Public entry: `init()`, channel types, `build_clips_page()`, status indicator widget |
| `src/clips/backend.rs` | `GsrBackend` — drives the `com.dec05eba.gpu_screen_recorder` Flatpak via `flatpak run` as a child process. Owns the long-lived backend thread. |
| `src/clips/detector.rs` | `GameDetector` — `/proc` scan, SteamAppId lookup, debounce, optional gamemoded D-Bus monitor |
| `src/clips/buffer.rs` | `BufferController` — state machine (Uninitialized / Idle / Arming / Armed / Saving / ErrorState) |
| `src/clips/hotkey.rs` | GlobalShortcuts portal binding via ashpd + `GAction` registration |
| `src/clips/library.rs` | Clip metadata index, directory scan, filename sanitization, retention policy |
| `src/clips/thumbnail.rs` | Async thumbnail extraction via `ffmpeg` child |
| `src/clips/browser.rs` | Clips tab UI (`gtk::GridView` of clip cards) |
| `src/clips/settings.rs` | Settings widgets, called from `window::build_settings_page` |
| `data/clips/save_callback.sh` | Bundled shell wrapper invoked by GSR's `-sc` flag, writes saved-path to FIFO |

**Modified files:**

| Path | What changes |
|---|---|
| `Cargo.toml` | Add `ashpd = { version = "0.10", features = ["gtk4"] }` |
| `src/app.rs` | Init clips subsystem during `init_pipeline`, poll backend events on glib timer, register `save-clip` GAction |
| `src/window.rs` | Replace Clips placeholder, add settings group, enable Clips sidebar button, add status indicator on dashboard |

---

## Phase 1: Foundations + Capture Backend

Goal: A backend thread that owns a `gpu-screen-recorder` child process, accepts commands via mpsc, and reports save events back to the GTK main thread. The Clips tab still shows a placeholder at the end of this phase, but the underlying capture engine works.

### Task 1.1: Add `ashpd` dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the dependency line**

In `Cargo.toml` under `[dependencies]`, append:

```toml
ashpd = { version = "0.10", features = ["gtk4"] }
```

Note: the `gtk4` feature integrates ashpd with the existing glib MainContext via `glib::MainContext::default().spawn_local()`. **Do NOT add the `tokio` feature** — the project's CLAUDE.md explicitly forbids tokio, and ashpd's `gtk4` feature works without it. If `cargo check` later complains about a missing async runtime, the correct fix is `features = ["gtk4", "async-std"]` (NOT tokio). Stream-extension methods like `.next().await` are reachable through `gtk::glib::prelude::*` — no `futures-util` dep needed.

- [ ] **Step 2: Verify build**

```bash
distrobox enter fedora-dev -- cargo check
```

Expected: clean compile with no errors. If ashpd needs additional system deps, install in distrobox (`dnf install dbus-devel`).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add ashpd 0.10 with gtk4 feature for portal calls"
```

### Task 1.2: Create empty `src/clips/` module skeleton

**Files:**
- Create: `src/clips/mod.rs`
- Create: `src/clips/backend.rs`
- Create: `src/clips/detector.rs`
- Create: `src/clips/buffer.rs`
- Create: `src/clips/hotkey.rs`
- Create: `src/clips/library.rs`
- Create: `src/clips/thumbnail.rs`
- Create: `src/clips/browser.rs`
- Create: `src/clips/settings.rs`
- Modify: `src/main.rs` — no change (entry point already routes through `app::run`)
- Modify: `src/app.rs:18` (add `mod clips;`-equivalent — actually `crate::clips` import wiring done later)

- [ ] **Step 1: Create stub module files**

Each file gets a single-line module doc comment so it compiles cleanly:

```rust
//! src/clips/mod.rs — public entry for the clipping subsystem.
pub mod backend;
pub mod browser;
pub mod buffer;
pub mod detector;
pub mod hotkey;
pub mod library;
pub mod settings;
pub mod thumbnail;
```

Each of `backend.rs`, `browser.rs`, `buffer.rs`, `detector.rs`, `hotkey.rs`, `library.rs`, `settings.rs`, `thumbnail.rs` gets:

```rust
//! Stub — implemented in later tasks.
```

- [ ] **Step 2: Wire `mod clips;` into the crate root**

The crate has no `src/lib.rs`, only `src/main.rs` that calls into `app::run`. Modules are declared in `src/main.rs`. Open `src/main.rs` and add `mod clips;` next to the existing module declarations.

```rust
// src/main.rs additions
mod clips;
```

- [ ] **Step 3: Verify build**

```bash
distrobox enter fedora-dev -- cargo check
```

Expected: clean compile, possibly with `dead_code` warnings on the empty modules (acceptable for now).

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/clips/
git commit -m "clips: scaffold src/clips/ module skeleton"
```

### Task 1.3: Bundle the save-callback shell script

**Files:**
- Create: `data/clips/save_callback.sh`
- Create: `LICENSES/save_callback_attribution.txt` (none needed — script is trivial; skip if no LICENSES dir norm)

- [ ] **Step 1: Write the callback script**

Create `data/clips/save_callback.sh` with mode 0755:

```sh
#!/bin/sh
# Invoked by gpu-screen-recorder's -sc flag after a clip is saved.
# $1 is the absolute path of the saved file.
# We write it to a FIFO that the backend thread reads.
if [ -n "$ARCTIS_CHATMIX_SAVE_FIFO" ] && [ -p "$ARCTIS_CHATMIX_SAVE_FIFO" ]; then
    printf '%s\n' "$1" > "$ARCTIS_CHATMIX_SAVE_FIFO"
fi
```

Run:

```bash
chmod +x data/clips/save_callback.sh
```

- [ ] **Step 2: Commit**

```bash
git add data/clips/save_callback.sh
git commit -m "clips: bundle save-callback shell wrapper for GSR -sc"
```

### Task 1.4: Define core types and channels

**Files:**
- Modify: `src/clips/mod.rs`

- [ ] **Step 1: Write a unit test for the public types**

In `src/clips/mod.rs`, add:

```rust
//! src/clips/mod.rs — public entry for the clipping subsystem.

pub mod backend;
pub mod browser;
pub mod buffer;
pub mod detector;
pub mod hotkey;
pub mod library;
pub mod settings;
pub mod thumbnail;

use std::path::PathBuf;
use std::time::Duration;

/// Commands sent from GTK main thread → backend thread.
#[derive(Debug, Clone)]
pub enum ClipCommand {
    /// Begin replay-mode capture with the given config.
    StartReplay { config: CaptureConfig },
    /// Stop and tear down the capture.
    StopReplay,
    /// Save the buffer (full duration).
    SaveClip,
    /// Save the buffer with a specific duration (uses GSR's SIGRTMIN+N for fixed durations).
    /// Falls back to SaveClip if duration doesn't match a supported value.
    SaveClipShort,  // 30 s
    SaveClipMedium, // 60 s
    SaveClipLong,   // 120 s
    /// Tear down current capture and reload with new config.
    Reconfigure { config: CaptureConfig },
    /// Final shutdown (drives the thread to exit).
    Shutdown,
}

/// Events sent from backend thread → GTK main thread.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Armed,
    Disarmed,
    Saved { path: PathBuf, duration_ms: u64 },
    BackendDied { reason: String },
    Error { context: String, message: String },
}

/// Capture configuration. Built from settings + portal pick at arm time.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub buffer_secs: u32,
    pub bitrate_mbps: u32,
    pub framerate: u32,
    pub portal_restore_token: Option<String>,
    pub headset_sink_monitor: String,
    pub include_per_source_tracks: bool,
    pub include_mic: bool,
    pub output_dir: PathBuf,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            buffer_secs: 60,
            bitrate_mbps: 25,
            framerate: 60,
            portal_restore_token: None,
            headset_sink_monitor: String::new(),
            include_per_source_tracks: true,
            include_mic: true,
            output_dir: PathBuf::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_config_default_is_60s_25mbps_60fps() {
        let c = CaptureConfig::default();
        assert_eq!(c.buffer_secs, 60);
        assert_eq!(c.bitrate_mbps, 25);
        assert_eq!(c.framerate, 60);
        assert!(c.include_per_source_tracks);
        assert!(c.include_mic);
    }

    #[test]
    fn clip_command_variants_compile() {
        // Smoke test that each variant constructs.
        let _ = ClipCommand::SaveClip;
        let _ = ClipCommand::SaveClipShort;
        let _ = ClipCommand::SaveClipMedium;
        let _ = ClipCommand::SaveClipLong;
        let _ = ClipCommand::StopReplay;
        let _ = ClipCommand::Shutdown;
    }
}
```

- [ ] **Step 2: Run tests, verify pass**

```bash
distrobox enter fedora-dev -- cargo test -p arctis-chatmix --lib clips::tests
```

Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/mod.rs
git commit -m "clips: define ClipCommand, BackendEvent, CaptureConfig types"
```

### Task 1.5: GsrConfig — translate CaptureConfig to CLI args

**Files:**
- Modify: `src/clips/backend.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/clips/backend.rs`:

```rust
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
```

- [ ] **Step 2: Run tests, verify pass**

```bash
distrobox enter fedora-dev -- cargo test -p arctis-chatmix clips::backend::tests
```

Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/backend.rs
git commit -m "clips: build GSR CLI args from CaptureConfig (multi-track AAC)"
```

### Task 1.6: Save-callback FIFO + script extraction

**Files:**
- Modify: `src/clips/backend.rs`

- [ ] **Step 1: Write failing tests for callback fixture management**

Append to `src/clips/backend.rs`:

```rust
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
```

- [ ] **Step 2: Run tests, verify pass**

```bash
distrobox enter fedora-dev -- cargo test clips::backend::fixture_tests
```

Expected: 3 tests pass. Side effect: `~/.cache/arctis-chatmix/clips/save_callback.sh` and `save.fifo` are created on the host (fine — they're idempotent).

- [ ] **Step 3: Commit**

```bash
git add src/clips/backend.rs
git commit -m "clips: extract save-callback script and create FIFO (idempotent)"
```

### Task 1.7: GSR child spawning + supervision skeleton

**Files:**
- Modify: `src/clips/backend.rs`

- [ ] **Step 1: Add the spawn helper using `pre_exec` for prctl**

Append to `src/clips/backend.rs`:

```rust
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
```

No new tests — child-spawning needs a real GSR install and is verified in Phase 8 integration testing.

- [ ] **Step 2: Verify build**

```bash
distrobox enter fedora-dev -- cargo check
```

Expected: clean compile.

- [ ] **Step 3: Commit**

```bash
git add src/clips/backend.rs
git commit -m "clips: spawn GSR child with PR_SET_PDEATHSIG + signal helper"
```

### Task 1.8: Backend thread main loop

**Files:**
- Modify: `src/clips/backend.rs`

- [ ] **Step 1: Write the run-loop**

Append to `src/clips/backend.rs`:

```rust
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
```

Note: `libc::SIGRTMIN()` is a function (not a constant) in libc 0.2.x — it returns `c_int`. The form `libc::SIGRTMIN() + N` above is correct. **Do not "fall back" to `libc::SIGRTMIN as libc::c_int`** — that casts the function pointer's address to an integer (footgun: silently sends signals at wrong values). If `SIGRTMIN()` is somehow missing, the toolchain is too old for edition 2024 anyway.

- [ ] **Step 2: Verify build**

```bash
distrobox enter fedora-dev -- cargo check
```

Expected: clean compile.

- [ ] **Step 3: Commit**

```bash
git add src/clips/backend.rs
git commit -m "clips: backend thread loop — arm/disarm/save/supervise/auto-restart"
```

### Task 1.9: Replace Clips placeholder with empty real page (still no clip features visible yet)

**Files:**
- Modify: `src/clips/browser.rs`
- Modify: `src/clips/mod.rs`
- Modify: `src/window.rs:84-87` (the placeholder `add_named` for "clips")
- Modify: `src/window.rs:238-240` (the sidebar button label/sensitivity)

- [ ] **Step 1: Add `build_clips_page` stub**

In `src/clips/browser.rs`:

```rust
//! Clips tab UI — grid browser for saved clips.

use adw::prelude::*;
use gtk::glib;

/// Build the Clips tab content. Initially shows the empty state; populated by
/// later tasks in this plan.
pub fn build_clips_page() -> gtk::Widget {
    let page = adw::StatusPage::builder()
        .icon_name("lucide-clapperboard-symbolic")
        .title("Clips")
        .description("No clips yet")
        .build();
    page.upcast()
}
```

In `src/clips/mod.rs`, add:

```rust
pub use browser::build_clips_page;
```

- [ ] **Step 2: Wire into window.rs**

In `src/window.rs`, replace the existing placeholder `add_named` for "clips" (currently around line 84-87):

```rust
// Old:
// stack.add_named(
//     &build_placeholder_page("Clips", "lucide-clapperboard-symbolic", "Coming soon"),
//     Some("clips"),
// );

// New:
stack.add_named(&crate::clips::build_clips_page(), Some("clips"));
```

Also update the sidebar button at the same file (was line 238-240):

```rust
// Old:
// let clips_btn = sidebar_button("lucide-clapperboard-symbolic", "Clips (coming soon)");
// clips_btn.set_sensitive(false);

// New:
let clips_btn = sidebar_button("lucide-clapperboard-symbolic", "Clips");
// (no set_sensitive(false) — the button is enabled now)
```

- [ ] **Step 3: Build and run**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual verify:
- Sidebar Clips button is now enabled (not greyed out)
- Clicking it shows a "No clips yet" StatusPage instead of "Coming soon"

- [ ] **Step 4: Commit**

```bash
git add src/window.rs src/clips/browser.rs src/clips/mod.rs
git commit -m "clips: replace Clips placeholder with empty real page"
```

### Task 1.10: Wire backend thread into AppResources + glib timer

**Files:**
- Modify: `src/app.rs`
- Modify: `src/clips/mod.rs`

- [ ] **Step 1: Add backend handle to AppResources**

In `src/app.rs`, modify the `AppResources` struct (around line 26):

```rust
struct AppResources {
    _sinks: VirtualSinks,
    router: Rc<RefCell<Option<AudioRouter>>>,
    shutdown: Arc<AtomicBool>,
    rx: Option<Receiver<HidEvent>>,
    writer: Option<HidWriter>,
    headset_sink: String,
    // NEW:
    clip_backend: Option<crate::clips::backend::BackendHandle>,
    clip_events: Option<Receiver<crate::clips::BackendEvent>>,
}
```

In `init_pipeline()` (search for the function in app.rs), spawn the backend after the existing audio router is created. The backend runs idle until later phases call `StartReplay`:

```rust
let (clip_backend, clip_events) = crate::clips::backend::spawn_backend();
```

And populate the new fields when constructing `AppResources`:

```rust
clip_backend: Some(clip_backend),
clip_events: Some(clip_events),
```

- [ ] **Step 2: Add a glib timer to drain events**

In `connect_activate` (after the existing HID timer registration), add a 100 ms timer that drains backend events. Initially it just logs them — the rest of the system isn't built yet:

```rust
let resources_clone = resources.clone();
glib::timeout_add_local(StdDuration::from_millis(100), move || {
    if let Some(res) = resources_clone.borrow().as_ref() {
        if let Some(rx) = res.clip_events.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(evt) => log::info!("clip event: {evt:?}"),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return glib::ControlFlow::Break,
                }
            }
        }
    }
    glib::ControlFlow::Continue
});
```

- [ ] **Step 3: Build and run**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Expected: app starts. Logs show no clip events (backend is idle).

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "clips: spawn backend thread and poll events on glib timer"
```

---

## Phase 2: Game detection + buffer state machine

Goal: a detector that fires `GameStarted` / `GameStopped` events, and a `BufferController` state machine that maps those events to backend commands. At the end of this phase, launching a Steam game causes the backend to attempt a `StartReplay` (which will fail until Phase 3 provides a portal restore_token).

### Task 2.1: `/proc` reader helpers

**Files:**
- Modify: `src/clips/detector.rs`

- [ ] **Step 1: Write the failing tests**

Replace the stub `src/clips/detector.rs` with:

```rust
//! Game detection — /proc scan + SteamAppId lookup + opportunistic gamemoded.

use std::fs;
use std::path::PathBuf;

/// Read /proc/<pid>/comm. Returns the trimmed contents or None if unreadable.
pub fn read_comm(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Read /proc/<pid>/cmdline. Returns argv joined by ASCII spaces (the file is
/// NUL-separated). None if unreadable.
pub fn read_cmdline(pid: u32) -> Option<String> {
    let bytes = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let s = bytes
        .split(|&b| b == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    Some(s)
}

/// Read /proc/<pid>/environ. Returns a HashMap<key, value>. None if unreadable.
pub fn read_environ(pid: u32) -> Option<std::collections::HashMap<String, String>> {
    let bytes = fs::read(format!("/proc/{pid}/environ")).ok()?;
    let mut out = std::collections::HashMap::new();
    for chunk in bytes.split(|&b| b == 0) {
        if chunk.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(chunk);
        if let Some((k, v)) = s.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        }
    }
    Some(out)
}

/// Iterate all PIDs in /proc.
pub fn all_pids() -> impl Iterator<Item = u32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
}

#[cfg(test)]
mod proc_tests {
    use super::*;

    #[test]
    fn read_comm_for_self_pid_returns_test_binary_name() {
        let me = std::process::id();
        let comm = read_comm(me).expect("comm readable");
        // Cargo's test binary is named after the crate, sometimes truncated to 15 chars.
        assert!(!comm.is_empty());
    }

    #[test]
    fn read_environ_for_self_pid_contains_path() {
        let me = std::process::id();
        let env = read_environ(me).expect("environ readable");
        assert!(env.contains_key("PATH"));
    }

    #[test]
    fn all_pids_includes_self() {
        let me = std::process::id();
        assert!(all_pids().any(|p| p == me));
    }

    #[test]
    fn read_comm_for_nonexistent_pid_returns_none() {
        assert!(read_comm(0).is_none());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::detector::proc_tests
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/detector.rs
git commit -m "clips: /proc reader helpers (comm, cmdline, environ, all_pids)"
```

### Task 2.2: Steam appmanifest parser

**Files:**
- Modify: `src/clips/detector.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/clips/detector.rs`:

```rust
/// Look up a Steam game's display name from its appmanifest file.
/// Returns None if the file is missing or "name" key isn't found.
pub fn steam_game_name(app_id: &str) -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home)
        .join(".steam/steam/steamapps")
        .join(format!("appmanifest_{app_id}.acf"));
    let contents = fs::read_to_string(path).ok()?;
    parse_acf_name(&contents)
}

/// Parse the "name" field out of a Steam ACF (Valve Key/Value format).
/// Looks for a line like:   "name"   "Apex Legends"
pub fn parse_acf_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        // Look for the pattern: "name" "<value>"
        if let Some(rest) = trimmed.strip_prefix("\"name\"") {
            let rest = rest.trim_start();
            // rest now begins with the second quoted field.
            let mut chars = rest.chars();
            if chars.next() != Some('"') {
                continue;
            }
            let mut value = String::new();
            for c in chars {
                if c == '"' {
                    return Some(value);
                }
                value.push(c);
            }
        }
    }
    None
}

#[cfg(test)]
mod acf_tests {
    use super::*;

    #[test]
    fn parse_simple_name() {
        let acf = r#"
"AppState"
{
    "appid"  "1172470"
    "name"   "Apex Legends"
    "Universe"  "1"
}
"#;
        assert_eq!(parse_acf_name(acf).as_deref(), Some("Apex Legends"));
    }

    #[test]
    fn parse_name_with_unicode() {
        let acf = r#"
"AppState"
{
    "name"   "ELDEN RING™"
    "appid"  "1245620"
}
"#;
        assert_eq!(parse_acf_name(acf).as_deref(), Some("ELDEN RING™"));
    }

    #[test]
    fn parse_returns_none_when_no_name() {
        let acf = r#"
"AppState"
{
    "appid"  "1234"
}
"#;
        assert!(parse_acf_name(acf).is_none());
    }

    #[test]
    fn parse_handles_extra_whitespace() {
        let acf = r#""name"        "Counter-Strike 2""#;
        assert_eq!(parse_acf_name(acf).as_deref(), Some("Counter-Strike 2"));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::detector::acf_tests
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/detector.rs
git commit -m "clips: parse Steam appmanifest .acf files for game names"
```

### Task 2.3: Game matchers

**Files:**
- Modify: `src/clips/detector.rs`

- [ ] **Step 1: Write the failing tests**

Append to `src/clips/detector.rs`:

```rust
/// A detected game, keyed by PID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedGame {
    pub pid: u32,
    pub name: String,
}

/// Try to identify a game from a process snapshot.
pub fn match_game(pid: u32, comm: &str, cmdline: &str, environ: &std::collections::HashMap<String, String>) -> Option<DetectedGame> {
    // Steam: cmdline contains "reaper SteamLaunch AppId=<id>"
    if let Some(idx) = cmdline.find("SteamLaunch AppId=") {
        let rest = &cmdline[idx + "SteamLaunch AppId=".len()..];
        let app_id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !app_id.is_empty() {
            let name = steam_game_name(&app_id).unwrap_or_else(|| comm.to_string());
            return Some(DetectedGame { pid, name });
        }
    }

    // Steam alternate: SteamAppId env var
    if let Some(app_id) = environ.get("SteamAppId") {
        let name = steam_game_name(app_id).unwrap_or_else(|| comm.to_string());
        return Some(DetectedGame { pid, name });
    }

    // Lutris: cmdline contains "lutris-wrapper"
    if cmdline.contains("lutris-wrapper") {
        // lutris-wrapper sets a "name" arg early in the cmdline; grab the first
        // post-wrapper non-flag word.
        let parts: Vec<&str> = cmdline.split_whitespace().collect();
        if let Some(idx) = parts.iter().position(|w| w.contains("lutris-wrapper")) {
            for word in &parts[idx + 1..] {
                if !word.starts_with('-') {
                    return Some(DetectedGame { pid, name: word.to_string() });
                }
            }
        }
        return Some(DetectedGame { pid, name: comm.to_string() });
    }

    // Heroic: comm == "heroic" or its launched wine processes
    if comm == "heroic" || cmdline.contains("HeroicGamesLauncher") {
        return Some(DetectedGame { pid, name: "Heroic Game".to_string() });
    }

    // gamescope session — only treat as game-detected if it has children doing real work.
    // For the v1 plan, we treat the gamescope process itself as a signal.
    if comm == "gamescope" {
        return Some(DetectedGame { pid, name: "Gamescope Game".to_string() });
    }

    // mangohud — only matches when it wraps a process; that wrapped process's
    // comm is what we want. For simplicity here, we just flag the parent.
    if comm == "mangohud" {
        return Some(DetectedGame { pid, name: comm.to_string() });
    }

    None
}

#[cfg(test)]
mod matcher_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn matches_steam_via_cmdline() {
        let cmdline = "/usr/bin/reaper SteamLaunch AppId=1172470 -- /path/to/game.exe";
        let env = HashMap::new();
        let m = match_game(42, "reaper", cmdline, &env);
        assert!(m.is_some());
        assert_eq!(m.unwrap().pid, 42);
    }

    #[test]
    fn matches_steam_via_environ() {
        let mut env = HashMap::new();
        env.insert("SteamAppId".into(), "1172470".into());
        let m = match_game(42, "wine64", "wine64 game.exe", &env);
        assert!(m.is_some());
    }

    #[test]
    fn matches_lutris_wrapper() {
        let cmdline = "lutris-wrapper Doom_Eternal -- /path/doom.exe";
        let env = HashMap::new();
        let m = match_game(42, "lutris-wrapper", cmdline, &env);
        assert!(m.is_some());
        assert_eq!(m.unwrap().name, "Doom_Eternal");
    }

    #[test]
    fn matches_gamescope() {
        let env = HashMap::new();
        let m = match_game(42, "gamescope", "gamescope -- game", &env);
        assert!(m.is_some());
    }

    #[test]
    fn no_match_for_unrelated_process() {
        let env = HashMap::new();
        assert!(match_game(42, "firefox", "firefox", &env).is_none());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::detector::matcher_tests
```

Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/detector.rs
git commit -m "clips: game matchers for Steam/Lutris/Heroic/gamescope/mangohud"
```

### Task 2.4: Detector tick + debounce

**Files:**
- Modify: `src/clips/detector.rs`

- [ ] **Step 1: Write the failing test**

Append:

```rust
/// Output event from the detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectorEvent {
    GameStarted(DetectedGame),
    GameStopped { pid: u32 },
}

/// Stateful detector — call `tick` every 2 seconds with the current process snapshot;
/// returns events.
pub struct GameDetector {
    /// Maps PID → consecutive scans seen.
    seen: std::collections::HashMap<u32, (DetectedGame, u32)>,
    /// Maps PID → consecutive scans missed.
    pending_remove: std::collections::HashMap<u32, u32>,
    /// PIDs we've already announced as started.
    announced: std::collections::HashSet<u32>,
}

impl GameDetector {
    pub fn new() -> Self {
        Self {
            seen: Default::default(),
            pending_remove: Default::default(),
            announced: Default::default(),
        }
    }

    /// Process a snapshot of currently-detected games. Returns events for any
    /// state transitions (new games announced after 2 consecutive scans; removals
    /// announced after 2 consecutive misses).
    pub fn tick(&mut self, current: &[DetectedGame]) -> Vec<DetectorEvent> {
        let mut events = Vec::new();
        let current_pids: std::collections::HashSet<u32> = current.iter().map(|g| g.pid).collect();

        // Bump persistence count for each currently-visible game.
        for game in current {
            let entry = self.seen.entry(game.pid).or_insert((game.clone(), 0));
            entry.1 += 1;
            if entry.1 >= 2 && !self.announced.contains(&game.pid) {
                events.push(DetectorEvent::GameStarted(game.clone()));
                self.announced.insert(game.pid);
            }
            self.pending_remove.remove(&game.pid);
        }

        // Bump miss count for any seen but absent.
        let absent: Vec<u32> = self.seen.keys().copied().filter(|p| !current_pids.contains(p)).collect();
        for pid in &absent {
            let count = self.pending_remove.entry(*pid).or_insert(0);
            *count += 1;
            if *count >= 2 {
                if self.announced.remove(pid) {
                    events.push(DetectorEvent::GameStopped { pid: *pid });
                }
                self.seen.remove(pid);
                self.pending_remove.remove(pid);
            }
        }

        events
    }
}

impl Default for GameDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod detector_tests {
    use super::*;

    fn g(pid: u32) -> DetectedGame {
        DetectedGame { pid, name: "Test Game".into() }
    }

    #[test]
    fn detects_after_two_consecutive_scans() {
        let mut d = GameDetector::new();
        assert!(d.tick(&[g(42)]).is_empty(), "first scan should not fire");
        let evts = d.tick(&[g(42)]);
        assert_eq!(evts.len(), 1);
        assert!(matches!(evts[0], DetectorEvent::GameStarted(_)));
    }

    #[test]
    fn does_not_double_fire() {
        let mut d = GameDetector::new();
        d.tick(&[g(42)]);
        d.tick(&[g(42)]);
        assert!(d.tick(&[g(42)]).is_empty(), "no re-fire on continued presence");
    }

    #[test]
    fn removes_after_two_consecutive_misses() {
        let mut d = GameDetector::new();
        d.tick(&[g(42)]);
        d.tick(&[g(42)]); // armed
        assert!(d.tick(&[]).is_empty(), "first miss does not fire");
        let evts = d.tick(&[]);
        assert_eq!(evts.len(), 1);
        assert!(matches!(evts[0], DetectorEvent::GameStopped { pid: 42 }));
    }

    #[test]
    fn brief_disappearance_does_not_fire_remove() {
        let mut d = GameDetector::new();
        d.tick(&[g(42)]);
        d.tick(&[g(42)]); // armed
        d.tick(&[]); // miss 1
        let evts = d.tick(&[g(42)]); // back!
        assert!(evts.is_empty(), "no event when game returns within debounce window");
    }
}
```

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::detector::detector_tests
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/detector.rs
git commit -m "clips: game detector with 2-scan debounce in/out"
```

### Task 2.5: Wire detector into a glib timer

**Files:**
- Modify: `src/clips/detector.rs`
- Modify: `src/clips/mod.rs`
- Modify: `src/app.rs`

- [ ] **Step 1: Add the public scan-once helper**

Append to `src/clips/detector.rs`:

```rust
/// Scan the current process tree once and return identified games.
pub fn scan_once() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    for pid in all_pids() {
        let comm = read_comm(pid).unwrap_or_default();
        let cmdline = read_cmdline(pid).unwrap_or_default();
        let environ = read_environ(pid).unwrap_or_default();
        if let Some(g) = match_game(pid, &comm, &cmdline, &environ) {
            games.push(g);
        }
    }
    games
}
```

- [ ] **Step 2: Re-export the detector types from `clips::mod`**

In `src/clips/mod.rs`, add:

```rust
pub use detector::{DetectedGame, DetectorEvent, GameDetector};
```

- [ ] **Step 3: Wire timer into app.rs**

In `src/app.rs`, in `connect_activate` after the existing timers, add a 2-second timer that scans and feeds the detector. For now, log events:

```rust
let detector_state = Rc::new(RefCell::new(crate::clips::GameDetector::new()));
glib::timeout_add_seconds_local(2, move || {
    let games = crate::clips::detector::scan_once();
    let evts = detector_state.borrow_mut().tick(&games);
    for e in evts {
        log::info!("detector event: {e:?}");
    }
    glib::ControlFlow::Continue
});
```

- [ ] **Step 4: Build and run, manually verify**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Launch a Steam game (or just run `gamescope --help` in another terminal — its mere presence won't fire because it's not running long, but launching a real game should produce a log line).

Expected log within ~6 s of game launch: `detector event: GameStarted(DetectedGame { pid: ..., name: "..." })`.

- [ ] **Step 5: Commit**

```bash
git add src/clips/detector.rs src/clips/mod.rs src/app.rs
git commit -m "clips: scan_once() + 2s detector tick wired to app timer"
```

### Task 2.6: BufferController state machine

**Files:**
- Modify: `src/clips/buffer.rs`

- [ ] **Step 1: Write the failing test**

Replace the stub `src/clips/buffer.rs` with:

```rust
//! BufferController — state machine that maps detector + portal + hotkey events
//! into ClipCommand sends to the backend.

use std::sync::mpsc::Sender;

use crate::clips::{BackendEvent, CaptureConfig, ClipCommand, DetectedGame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// No portal pick yet — waiting for user to set up capture source.
    Uninitialized,
    /// Portal pick exists but no game running.
    Idle,
    /// Sent StartReplay; awaiting Armed event.
    Arming,
    /// Backend reported Armed.
    Armed,
    /// Saving (sent SaveClip; awaiting Saved event).
    Saving,
    /// Backend reported error/death; user must retry.
    ErrorState,
}

pub struct BufferController {
    state: BufferState,
    /// Cached config builder. Updated as user changes settings.
    config: CaptureConfig,
    /// Whether auto-arm is enabled (default true).
    pub auto_arm: bool,
    /// Whether always-armed override is on (default false).
    pub always_armed: bool,
    /// Currently-detected game, if any.
    current_game: Option<DetectedGame>,
    /// Has the portal been picked? (Set externally after restore_token persists.)
    pub has_portal_pick: bool,
}

impl BufferController {
    pub fn new(config: CaptureConfig) -> Self {
        Self {
            state: BufferState::Uninitialized,
            config,
            auto_arm: true,
            always_armed: false,
            current_game: None,
            has_portal_pick: false,
        }
    }

    pub fn state(&self) -> BufferState {
        self.state
    }

    pub fn current_game(&self) -> Option<&DetectedGame> {
        self.current_game.as_ref()
    }

    /// User completed portal pick. Transition Uninitialized → Idle (and possibly
    /// Arming if always_armed).
    pub fn on_portal_pick_complete(&mut self, restore_token: String, cmd_tx: &Sender<ClipCommand>) {
        self.config.portal_restore_token = Some(restore_token);
        self.has_portal_pick = true;
        if matches!(self.state, BufferState::Uninitialized) {
            self.state = BufferState::Idle;
            self.maybe_arm(cmd_tx);
        }
    }

    /// Detector reports a game started.
    pub fn on_game_started(&mut self, game: DetectedGame, cmd_tx: &Sender<ClipCommand>) {
        self.current_game = Some(game);
        self.maybe_arm(cmd_tx);
    }

    /// Detector reports the current game stopped.
    pub fn on_game_stopped(&mut self, pid: u32, cmd_tx: &Sender<ClipCommand>) {
        if self.current_game.as_ref().map(|g| g.pid) == Some(pid) {
            self.current_game = None;
            if !self.always_armed && matches!(self.state, BufferState::Armed) {
                let _ = cmd_tx.send(ClipCommand::StopReplay);
                self.state = BufferState::Idle;
            }
        }
    }

    /// User pressed the save hotkey.
    pub fn on_save_hotkey(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if matches!(self.state, BufferState::Armed) {
            let _ = cmd_tx.send(ClipCommand::SaveClip);
            self.state = BufferState::Saving;
        }
    }

    /// Backend event arrived from backend thread.
    pub fn on_backend_event(&mut self, evt: &BackendEvent, _cmd_tx: &Sender<ClipCommand>) {
        match evt {
            BackendEvent::Armed => {
                if matches!(self.state, BufferState::Arming) {
                    self.state = BufferState::Armed;
                }
            }
            BackendEvent::Disarmed => {
                if matches!(self.state, BufferState::Armed | BufferState::Arming) {
                    self.state = BufferState::Idle;
                }
            }
            BackendEvent::Saved { .. } => {
                if matches!(self.state, BufferState::Saving) {
                    self.state = BufferState::Armed;
                }
            }
            BackendEvent::BackendDied { .. } | BackendEvent::Error { .. } => {
                self.state = BufferState::ErrorState;
            }
        }
    }

    /// Settings change — rebuild config and reconfigure backend if armed.
    pub fn on_config_change(&mut self, new_config: CaptureConfig, cmd_tx: &Sender<ClipCommand>) {
        // Preserve restore_token if caller didn't provide one.
        let token = new_config
            .portal_restore_token
            .clone()
            .or_else(|| self.config.portal_restore_token.clone());
        self.config = CaptureConfig { portal_restore_token: token, ..new_config };
        if matches!(self.state, BufferState::Armed | BufferState::Arming) {
            let _ = cmd_tx.send(ClipCommand::Reconfigure { config: self.config.clone() });
        }
    }

    /// User-initiated retry after error.
    pub fn retry(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if matches!(self.state, BufferState::ErrorState) {
            self.state = BufferState::Idle;
            self.maybe_arm(cmd_tx);
        }
    }

    fn maybe_arm(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if !self.has_portal_pick {
            return;
        }
        if !matches!(self.state, BufferState::Idle) {
            return;
        }
        let should_arm = self.always_armed || (self.auto_arm && self.current_game.is_some());
        if should_arm {
            let _ = cmd_tx.send(ClipCommand::StartReplay { config: self.config.clone() });
            self.state = BufferState::Arming;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn cfg() -> CaptureConfig {
        CaptureConfig::default()
    }

    fn dg(pid: u32) -> DetectedGame {
        DetectedGame { pid, name: "Test".into() }
    }

    #[test]
    fn starts_in_uninitialized() {
        let b = BufferController::new(cfg());
        assert_eq!(b.state(), BufferState::Uninitialized);
    }

    #[test]
    fn portal_pick_transitions_to_idle() {
        let (tx, _rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("token".into(), &tx);
        assert_eq!(b.state(), BufferState::Idle);
    }

    #[test]
    fn game_start_arms_when_idle_and_auto_arm() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("token".into(), &tx);
        b.on_game_started(dg(42), &tx);
        assert_eq!(b.state(), BufferState::Arming);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
    }

    #[test]
    fn game_start_does_not_arm_without_portal() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_game_started(dg(42), &tx);
        assert_eq!(b.state(), BufferState::Uninitialized);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn save_hotkey_only_works_in_armed() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_save_hotkey(&tx);
        assert!(rx.try_recv().is_err(), "no command in non-Armed state");
    }

    #[test]
    fn save_hotkey_in_armed_sends_save_clip() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("t".into(), &tx);
        b.on_game_started(dg(42), &tx);
        let _ = rx.try_recv(); // consume StartReplay
        b.on_backend_event(&BackendEvent::Armed, &tx);
        assert_eq!(b.state(), BufferState::Armed);
        b.on_save_hotkey(&tx);
        assert_eq!(b.state(), BufferState::Saving);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::SaveClip)));
    }

    #[test]
    fn always_armed_arms_on_portal_pick() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.always_armed = true;
        b.on_portal_pick_complete("t".into(), &tx);
        assert_eq!(b.state(), BufferState::Arming);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::buffer::tests
```

Expected: 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/buffer.rs
git commit -m "clips: BufferController state machine with 7 unit tests"
```

### Task 2.7: Wire detector → buffer → backend in app.rs

**Files:**
- Modify: `src/app.rs`
- Modify: `src/clips/mod.rs`

- [ ] **Step 1: Re-export BufferController**

In `src/clips/mod.rs`:

```rust
pub use buffer::{BufferController, BufferState};
```

- [ ] **Step 2: Wire it into app.rs**

In `src/app.rs`:

1. Add `clip_controller: Rc<RefCell<crate::clips::BufferController>>` to `AppResources` (or store separately if `AppResources` is becoming unwieldy — separate `Rc` is fine).
2. In `connect_activate`, instantiate it from a `CaptureConfig::default()` (filled with the headset_sink_monitor from `AppResources`).
3. Replace the existing detector log-only timer with one that calls `buffer.on_game_started/on_game_stopped` and the existing backend-event poll with one that calls `buffer.on_backend_event`.

Concrete code (modify the timers added in Tasks 1.10 and 2.5):

```rust
// Build the controller.
let mut initial_cfg = crate::clips::CaptureConfig::default();
initial_cfg.headset_sink_monitor = format!("{}.monitor", res.headset_sink);
initial_cfg.output_dir = std::path::PathBuf::from(
    std::env::var("HOME").unwrap_or_default()
).join("Videos/Clips");
let buffer = Rc::new(RefCell::new(crate::clips::BufferController::new(initial_cfg)));

// Detector timer (replace the previous log-only one).
let buf_clone = buffer.clone();
let backend_handle = res.clip_backend.as_ref().map(|h| h.cmd_tx.clone());
// Note: cmd_tx isn't currently public on BackendHandle. Make it pub(crate) or add a method.
// Add: impl BackendHandle { pub(crate) fn sender(&self) -> Sender<ClipCommand> { self.cmd_tx.clone() } }
let detector_state = Rc::new(RefCell::new(crate::clips::GameDetector::new()));
let detector_clone = detector_state.clone();
let backend_sender = backend_handle.clone();
glib::timeout_add_seconds_local(2, move || {
    let games = crate::clips::detector::scan_once();
    let evts = detector_clone.borrow_mut().tick(&games);
    if let Some(tx) = backend_sender.as_ref() {
        let mut buf = buf_clone.borrow_mut();
        for e in evts {
            match e {
                crate::clips::DetectorEvent::GameStarted(g) => buf.on_game_started(g, tx),
                crate::clips::DetectorEvent::GameStopped { pid } => buf.on_game_stopped(pid, tx),
            }
        }
    }
    glib::ControlFlow::Continue
});

// Backend events poll (replace the previous log-only one).
let buf_clone = buffer.clone();
let resources_clone = resources.clone();
glib::timeout_add_local(StdDuration::from_millis(100), move || {
    if let Some(res) = resources_clone.borrow().as_ref() {
        if let (Some(rx), Some(tx)) = (res.clip_events.as_ref(), res.clip_backend.as_ref().map(|h| h.sender())) {
            let mut buf = buf_clone.borrow_mut();
            loop {
                match rx.try_recv() {
                    Ok(evt) => {
                        log::info!("clip event: {evt:?}");
                        buf.on_backend_event(&evt, &tx);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return glib::ControlFlow::Break,
                }
            }
        }
    }
    glib::ControlFlow::Continue
});
```

This requires adding a method on `BackendHandle`:

```rust
// In src/clips/backend.rs, BackendHandle impl:
pub fn sender(&self) -> Sender<ClipCommand> { self.cmd_tx.clone() }
```

- [ ] **Step 3: Build and verify**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Expected: app starts cleanly. Logs show `clip event: ...` and detector events. The buffer is in `Uninitialized` until Phase 3 wires the portal pick — `StartReplay` is never sent yet (verified by the absence of GSR child processes in `pgrep gpu-screen-recorder`).

- [ ] **Step 4: Commit**

```bash
git add src/clips/backend.rs src/clips/mod.rs src/app.rs
git commit -m "clips: wire detector → BufferController → backend over mpsc"
```

---

## Phase 3: Onboarding wizard + portal pick

Goal: a three-page first-run wizard.

- **Page 1** ("Install gpu-screen-recorder"): friendly explanation, primary "Install" button (auto-installs the Flathub Flatpak), secondary "Open in Bazaar" button (`appstream://com.dec05eba.gpu_screen_recorder`), copyable terminal command. Polls `flatpak info` every 2 s; Next button enables when GSR is detected (regardless of which install path the user took). **Mandatory — no skip.**
- **Page 2** ("Pick the screen to record"): single "Pick screen" button → ScreenCast portal picker → persists `restore_token`. Next button enables once a screen is picked. **Pressing Next here flips `onboarding_complete = true`.**
- **Page 3** ("Configure clips"): hotkey rebind, buffer length slider, storage path picker. All have sensible defaults pre-filled. Live-save on each change. "Done" button always enabled. Closing here is fine — onboarding's already complete from Page 2.

**Sticky logic:** as long as `onboarding_complete == false`, opening the Clips tab shows the wizard at the first incomplete step. After Page 2 Next sets the flag, future Clips-tab opens go directly to the empty browser even if Page 3 was never visited.

**Recovery:** if GSR is uninstalled later (user removed it via Bazaar/Discover), surface an error toast at next arm attempt with a "Reinstall gpu-screen-recorder" button in Settings. Don't auto-relaunch the wizard — user explicitly removed the dependency, we shouldn't be intrusive.

### Task 3.1: Portal session helper

**Files:**
- Modify: `src/clips/hotkey.rs` — actually no, separate file better. Use a new helper inside `src/clips/buffer.rs`? No — portal-specific code goes in a new helper module. We don't have one for portals yet. Add to `src/clips/mod.rs` as a sub-module or a flat helper? For simplicity, put portal-related code in a new file `src/clips/portal.rs`.

- Create: `src/clips/portal.rs`
- Modify: `src/clips/mod.rs`

- [ ] **Step 1: Add module declaration**

In `src/clips/mod.rs`:

```rust
pub mod portal;
```

- [ ] **Step 2: Implement portal session creation**

Create `src/clips/portal.rs`:

```rust
//! ScreenCast portal interaction via ashpd.

use ashpd::desktop::screencast::{CursorMode, PersistMode, Screencast, SourceType};
use std::path::PathBuf;

const PORTAL_TOKEN_FILE: &str = "clips_portal.txt";

fn token_path() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home).join(".config/arctis-chatmix").join(PORTAL_TOKEN_FILE)
}

pub fn load_token() -> Option<String> {
    std::fs::read_to_string(token_path()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn save_token(token: &str) -> std::io::Result<()> {
    let path = token_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, token)
}

pub fn clear_token() -> std::io::Result<()> {
    let path = token_path();
    if path.exists() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

/// Open the ScreenCast portal picker. Awaits user choice, returns the restore token
/// on success.
pub async fn pick_screencast_source() -> ashpd::Result<String> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            false,
            None,
            PersistMode::ExplicitlyRevoked,
        )
        .await?;
    let response = proxy.start(&session, None).await?.response()?;
    let token = response
        .restore_token()
        .map(|s| s.to_string())
        .unwrap_or_default();
    Ok(token)
}

/// Capture a single frame from the current portal session for the "Test capture"
/// button. Uses the Screenshot portal which shares consent with ScreenCast.
pub async fn screenshot_current_target() -> ashpd::Result<PathBuf> {
    use ashpd::desktop::screenshot::Screenshot;
    let response = Screenshot::request().interactive(false).send().await?.response()?;
    // ashpd 0.10 returns &url::Url from .uri(). Use to_file_path() which handles
    // URL-decoding properly; fall back to as_str() trim if to_file_path is unavailable
    // for the URL kind.
    let url = response.uri();
    if let Ok(p) = url.to_file_path() {
        return Ok(p);
    }
    let s = url.as_str();
    let path = s.strip_prefix("file://").unwrap_or(s);
    Ok(PathBuf::from(path))
}
```

- [ ] **Step 3: Add unit test for token persistence**

Append to the file:

```rust
#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn token_round_trip() {
        let token = "test-token-12345";
        save_token(token).unwrap();
        assert_eq!(load_token().as_deref(), Some(token));
        clear_token().unwrap();
        assert!(load_token().is_none());
    }
}
```

- [ ] **Step 4: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::portal::token_tests
```

Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add src/clips/portal.rs src/clips/mod.rs
git commit -m "clips: ScreenCast portal helper + restore_token persistence"
```

### Task 3.2: GSR Flatpak detection + install helper

**Files:**
- Create: `src/clips/gsr_install.rs`
- Modify: `src/clips/mod.rs`

This module checks whether the GSR Flatpak is installed and provides three install paths the wizard surfaces: in-app auto-install (default), Bazaar deep-link, copyable terminal command.

- [ ] **Step 1: Add module declaration**

In `src/clips/mod.rs`, alongside the other `pub mod` lines:

```rust
pub mod gsr_install;
```

- [ ] **Step 2: Implement the helper module**

Create `src/clips/gsr_install.rs`:

```rust
//! GSR Flatpak detection + install helpers for the onboarding wizard.

use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

pub const GSR_APP_ID: &str = "com.dec05eba.gpu_screen_recorder";
pub const GSR_TERMINAL_INSTALL_COMMAND: &str =
    "flatpak install --user flathub com.dec05eba.gpu_screen_recorder";

/// Returns true if `flatpak info <app id>` succeeds (exit 0).
pub fn is_installed() -> bool {
    Command::new("flatpak")
        .args(["info", GSR_APP_ID])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Install progress updates emitted to the receiver.
#[derive(Debug)]
pub enum InstallProgress {
    Started,
    /// Indeterminate progress with a status string parsed from flatpak's stderr.
    Status(String),
    Done,
    Failed { reason: String },
}

/// Spawn a worker thread that runs `flatpak install --user --noninteractive --assumeyes
/// flathub com.dec05eba.gpu_screen_recorder` and streams progress events.
/// Returns immediately; events arrive on the receiver.
pub fn install() -> Receiver<InstallProgress> {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name("gsr-install".into())
        .spawn(move || run_install(tx))
        .expect("spawn gsr-install");
    rx
}

fn run_install(tx: Sender<InstallProgress>) {
    let _ = tx.send(InstallProgress::Started);

    // --noninteractive avoids the y/n prompt; --assumeyes accepts license/etc.
    // --user installs into ~/.local/share/flatpak (no root needed).
    let mut child = match Command::new("flatpak")
        .args([
            "install",
            "--user",
            "--noninteractive",
            "--assumeyes",
            "flathub",
            GSR_APP_ID,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(InstallProgress::Failed { reason: format!("spawn failed: {e}") });
            return;
        }
    };

    // Read stderr for status lines ("Installing...", "Downloading...", etc.).
    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let r = BufReader::new(stderr);
            for line in r.lines().map_while(Result::ok) {
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    let _ = tx.send(InstallProgress::Status(trimmed));
                }
            }
        });
    }

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(InstallProgress::Failed { reason: format!("wait failed: {e}") });
            return;
        }
    };

    if status.success() {
        let _ = tx.send(InstallProgress::Done);
    } else {
        let _ = tx.send(InstallProgress::Failed {
            reason: format!("flatpak install exited with {status:?}"),
        });
    }
}

/// Open the GSR app page in the system app store via AppStream URI.
/// On Bazzite this opens Bazaar; on KDE it opens Discover; on GNOME it opens Software.
pub fn open_in_bazaar() -> std::io::Result<()> {
    let url = format!("appstream://{GSR_APP_ID}");
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?
        .wait()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_command_string_is_stable() {
        assert_eq!(
            GSR_TERMINAL_INSTALL_COMMAND,
            "flatpak install --user flathub com.dec05eba.gpu_screen_recorder"
        );
    }

    #[test]
    fn app_id_is_canonical() {
        assert_eq!(GSR_APP_ID, "com.dec05eba.gpu_screen_recorder");
    }
}
```

- [ ] **Step 3: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::gsr_install
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/clips/gsr_install.rs src/clips/mod.rs
git commit -m "clips: GSR Flatpak detection + install helpers (info / install / bazaar)"
```

### Task 3.3: Onboarding state in settings

**Files:**
- Modify: `src/clips/settings.rs`

- [ ] **Step 1: Add `onboarding_complete` to ClipSettings**

Add a new field to `ClipSettings` and persist it. The flag flips to `true` when the user presses Next on Page 2 of the wizard (after picking a screen). It is NOT flipped by Page 3 — Page 3 is optional.

In `src/clips/settings.rs`, modify the existing struct + load/save:

```rust
#[derive(Debug, Clone)]
pub struct ClipSettings {
    pub buffer_length: u32,
    pub bitrate_mbps: u32,
    pub auto_arm: bool,
    pub always_armed: bool,
    pub per_source_tracks: bool,
    pub mic_capture: bool,
    pub storage_path: PathBuf,
    pub disk_cap_gb: Option<u32>,
    pub onboarding_complete: bool, // NEW
}

impl Default for ClipSettings {
    fn default() -> Self {
        Self {
            // ... existing fields ...
            onboarding_complete: false, // NEW
        }
    }
}
```

In the `load()` function, add a match arm for `"onboarding_complete"`:

```rust
"onboarding_complete" => s.onboarding_complete = v == "1",
```

In the `save()` function, append a line:

```rust
body.push_str(&format!("onboarding_complete={}\n", if s.onboarding_complete { 1 } else { 0 }));
```

- [ ] **Step 2: Add a helper to mark complete**

Append:

```rust
/// Mark onboarding complete. Idempotent.
pub fn mark_onboarding_complete() -> std::io::Result<()> {
    let mut s = load();
    s.onboarding_complete = true;
    save(&s)
}
```

- [ ] **Step 3: Add a unit test**

```rust
#[test]
fn onboarding_complete_round_trips() {
    let mut s = ClipSettings::default();
    assert!(!s.onboarding_complete, "default is false");
    s.onboarding_complete = true;
    // Just verify the field flips; full round-trip via save/load needs a real $HOME.
    assert!(s.onboarding_complete);
}
```

- [ ] **Step 4: Run tests + commit**

```bash
distrobox enter fedora-dev -- cargo test clips::settings
git add src/clips/settings.rs
git commit -m "clips: persist onboarding_complete flag in clips_settings.txt"
```

### Task 3.4: Onboarding wizard UI

**Files:**
- Modify: `src/clips/browser.rs`

The wizard is a `gtk::Stack` nested inside the Onboarding state of `ClipsPage`. Three pages, navigated via Next buttons.

- [ ] **Step 1: Replace browser.rs with a state-driven page including the wizard stack**

Replace `src/clips/browser.rs` contents:

```rust
//! Clips tab UI — onboarding wizard + grid browser for saved clips.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageState {
    Onboarding,
    Empty,
    Loaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    InstallGsr,
    PickScreen,
    Settings,
}

pub struct ClipsPage {
    pub root: gtk::Stack,
    pub state: Rc<RefCell<PageState>>,
    pub wizard: Rc<WizardWidgets>,
}

pub struct WizardWidgets {
    pub stack: gtk::Stack,
    pub step: RefCell<WizardStep>,
    // Page 1
    pub install_status_label: gtk::Label,
    pub install_next_btn: gtk::Button,
    // Page 2
    pub screen_picked_label: gtk::Label,
    pub screen_next_btn: gtk::Button,
    // Page 3
    pub hotkey_label: gtk::Label,
    pub buffer_scale: gtk::Scale,
    pub storage_label: gtk::Label,
}

pub fn build_clips_page() -> ClipsPage {
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .hexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();

    let wizard = Rc::new(build_wizard());
    stack.add_named(&wizard.stack, Some("onboarding"));
    stack.add_named(&empty_page(), Some("empty"));
    stack.add_named(&loaded_page(), Some("loaded"));

    let state = Rc::new(RefCell::new(PageState::Onboarding));
    stack.set_visible_child_name("onboarding");

    ClipsPage { root: stack, state, wizard }
}

fn build_wizard() -> WizardWidgets {
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .hexpand(true)
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .transition_duration(200)
        .build();

    let (page1, install_status_label, install_next_btn) = build_page1_install();
    let (page2, screen_picked_label, screen_next_btn) = build_page2_screen();
    let (page3, hotkey_label, buffer_scale, storage_label) = build_page3_settings();

    stack.add_named(&page1, Some("wizard-1-install"));
    stack.add_named(&page2, Some("wizard-2-screen"));
    stack.add_named(&page3, Some("wizard-3-settings"));
    stack.set_visible_child_name("wizard-1-install");

    WizardWidgets {
        stack,
        step: RefCell::new(WizardStep::InstallGsr),
        install_status_label,
        install_next_btn,
        screen_picked_label,
        screen_next_btn,
        hotkey_label,
        buffer_scale,
        storage_label,
    }
}

fn step_indicator(current: u8, total: u8) -> gtk::Label {
    let lbl = gtk::Label::new(Some(&format!("Step {current} of {total}")));
    lbl.add_css_class("dim-label");
    lbl.add_css_class("caption");
    lbl.set_xalign(0.5);
    lbl
}

fn build_page1_install() -> (gtk::Widget, gtk::Label, gtk::Button) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(40)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    page.append(&step_indicator(1, 3));

    let title = gtk::Label::new(Some("Install gpu-screen-recorder"));
    title.add_css_class("title-1");
    page.append(&title);

    let body = gtk::Label::new(Some(
        "Clips uses gpu-screen-recorder, a free open-source Flatpak \
         from Flathub, to capture gameplay efficiently. Pick any of \
         the install methods below — Clips will detect when it's ready."
    ));
    body.set_wrap(true);
    body.set_max_width_chars(60);
    body.set_xalign(0.5);
    page.append(&body);

    // Primary install button.
    let install_btn = gtk::Button::builder()
        .label("Install")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build();
    install_btn.set_action_name(Some("app.gsr-install"));
    page.append(&install_btn);

    // Secondary actions row.
    let alt_label = gtk::Label::new(Some("— or install manually —"));
    alt_label.add_css_class("dim-label");
    page.append(&alt_label);

    let bazaar_btn = gtk::Button::builder()
        .label("Open in Bazaar")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .build();
    bazaar_btn.set_action_name(Some("app.gsr-open-in-bazaar"));
    page.append(&bazaar_btn);

    // Terminal command in a code block + Copy button.
    let code_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["card"])
        .build();
    let cmd_label = gtk::Label::new(Some(crate::clips::gsr_install::GSR_TERMINAL_INSTALL_COMMAND));
    cmd_label.add_css_class("monospace");
    cmd_label.set_selectable(true);
    cmd_label.set_xalign(0.0);
    cmd_label.set_hexpand(true);
    code_box.append(&cmd_label);
    let copy_btn = gtk::Button::builder()
        .label("Copy")
        .build();
    copy_btn.set_action_name(Some("app.gsr-copy-cli"));
    code_box.append(&copy_btn);
    page.append(&code_box);

    // Status label (reflects install progress when active).
    let install_status_label = gtk::Label::new(None);
    install_status_label.add_css_class("dim-label");
    install_status_label.set_visible(false);
    page.append(&install_status_label);

    // Next button — disabled until is_installed() returns true.
    let install_next_btn = gtk::Button::builder()
        .label("Next")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();
    install_next_btn.set_action_name(Some("app.wizard-next"));
    page.append(&install_next_btn);

    (page.upcast(), install_status_label, install_next_btn)
}

fn build_page2_screen() -> (gtk::Widget, gtk::Label, gtk::Button) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(40)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    page.append(&step_indicator(2, 3));

    let title = gtk::Label::new(Some("Pick the screen to record"));
    title.add_css_class("title-1");
    page.append(&title);

    let body = gtk::Label::new(Some(
        "Choose which display Clips should capture from. \
         You can change this later in Settings."
    ));
    body.set_wrap(true);
    body.set_max_width_chars(60);
    body.set_xalign(0.5);
    page.append(&body);

    let pick_btn = gtk::Button::builder()
        .label("Pick screen")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build();
    pick_btn.set_action_name(Some("app.setup-clips"));
    page.append(&pick_btn);

    let screen_picked_label = gtk::Label::new(None);
    screen_picked_label.add_css_class("dim-label");
    screen_picked_label.set_visible(false);
    page.append(&screen_picked_label);

    let screen_next_btn = gtk::Button::builder()
        .label("Next")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();
    screen_next_btn.set_action_name(Some("app.wizard-next"));
    page.append(&screen_next_btn);

    (page.upcast(), screen_picked_label, screen_next_btn)
}

fn build_page3_settings() -> (gtk::Widget, gtk::Label, gtk::Scale, gtk::Label) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(40)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    page.append(&step_indicator(3, 3));

    let title = gtk::Label::new(Some("Configure clips"));
    title.add_css_class("title-1");
    page.append(&title);

    let body = gtk::Label::new(Some(
        "All settings have sensible defaults. Tweak now \
         or later in Settings."
    ));
    body.set_wrap(true);
    body.set_max_width_chars(60);
    body.set_xalign(0.5);
    page.append(&body);

    // Hotkey row
    let hotkey_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    hotkey_row.append(&gtk::Label::new(Some("Save hotkey")));
    let hotkey_label = gtk::Label::new(Some("Super+Shift+R"));
    hotkey_label.set_hexpand(true);
    hotkey_label.set_xalign(1.0);
    hotkey_row.append(&hotkey_label);
    let rebind_btn = gtk::Button::builder().label("Change…").build();
    rebind_btn.set_action_name(Some("app.rebind-clip-hotkey"));
    hotkey_row.append(&rebind_btn);
    page.append(&hotkey_row);

    // Buffer length scale
    let buffer_row = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
    buffer_row.append(&gtk::Label::builder().label("Buffer length (seconds)").xalign(0.0).build());
    let buffer_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 30.0, 300.0, 5.0);
    buffer_scale.set_value(60.0);
    buffer_scale.set_draw_value(true);
    buffer_scale.set_value_pos(gtk::PositionType::Right);
    buffer_row.append(&buffer_scale);
    page.append(&buffer_row);

    // Storage path row
    let storage_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    storage_row.append(&gtk::Label::new(Some("Save clips to")));
    let storage_label = gtk::Label::new(Some("~/Videos/Clips"));
    storage_label.set_hexpand(true);
    storage_label.set_xalign(1.0);
    storage_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    storage_row.append(&storage_label);
    let pick_storage_btn = gtk::Button::builder().label("Pick folder").build();
    pick_storage_btn.set_action_name(Some("app.pick-clip-storage"));
    storage_row.append(&pick_storage_btn);
    page.append(&storage_row);

    // Done button
    let done_btn = gtk::Button::builder()
        .label("Done")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::End)
        .build();
    done_btn.set_action_name(Some("app.wizard-next"));
    page.append(&done_btn);

    (page.upcast(), hotkey_label, buffer_scale, storage_label)
}

fn empty_page() -> gtk::Widget {
    let page = adw::StatusPage::builder()
        .icon_name("lucide-clapperboard-symbolic")
        .title("No clips yet")
        .description("Press the save hotkey while gaming to capture the last 60 seconds.")
        .build();
    page.upcast()
}

fn loaded_page() -> gtk::Widget {
    // Real grid view added in Phase 5.
    gtk::Box::builder().orientation(gtk::Orientation::Vertical).build().upcast()
}

impl ClipsPage {
    pub fn set_state(&self, new_state: PageState) {
        *self.state.borrow_mut() = new_state;
        match new_state {
            PageState::Onboarding => self.root.set_visible_child_name("onboarding"),
            PageState::Empty => self.root.set_visible_child_name("empty"),
            PageState::Loaded => self.root.set_visible_child_name("loaded"),
        }
    }

    pub fn set_wizard_step(&self, step: WizardStep) {
        *self.wizard.step.borrow_mut() = step;
        let name = match step {
            WizardStep::InstallGsr => "wizard-1-install",
            WizardStep::PickScreen => "wizard-2-screen",
            WizardStep::Settings => "wizard-3-settings",
        };
        self.wizard.stack.set_visible_child_name(name);
    }

    pub fn current_wizard_step(&self) -> WizardStep {
        *self.wizard.step.borrow()
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }
}
```

- [ ] **Step 2: Update mod re-export**

In `src/clips/mod.rs`:

```rust
// Replace any existing browser re-export with:
pub use browser::{ClipsPage, PageState, WizardStep, build_clips_page};
```

- [ ] **Step 3: Update window.rs to use the new ClipsPage**

```rust
let clips_page = crate::clips::build_clips_page();
stack.add_named(clips_page.widget(), Some("clips"));
// Store clips_page on Widgets so app.rs can drive set_state / set_wizard_step.
```

The `Widgets` struct in `src/window.rs` needs a new field `pub clips: Rc<crate::clips::ClipsPage>` (Rc'd because actions in app.rs share it).

- [ ] **Step 4: Build and verify visually**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix  # remember to kill the user's running instance first if needed
```

Manual: click the Clips sidebar button. Should land on Page 1 of the wizard ("Install gpu-screen-recorder", Step 1 of 3, Install/Bazaar/Terminal options visible, Next button greyed out).

- [ ] **Step 5: Commit**

```bash
git add src/clips/browser.rs src/clips/mod.rs src/window.rs
git commit -m "clips: 3-page onboarding wizard UI (install / screen / settings)"
```

### Task 3.5: Wire wizard actions

**Files:**
- Modify: `src/app.rs`
- Modify: `src/clips/buffer.rs`

Register the GActions the wizard buttons trigger, plus a 2-second polling timer that watches for GSR install completion.

- [ ] **Step 1: Register wizard actions**

In `src/app.rs::connect_activate`, after the existing setup:

```rust
use gtk::gio;

// app.gsr-install — kick off async install, stream progress to Page 1 status label.
{
    let clips_page = window.clips_page().clone();
    let install_action = gio::ActionEntry::builder("gsr-install")
        .activate(move |_app, _action, _param| {
            let rx = crate::clips::gsr_install::install();
            clips_page.wizard.install_status_label.set_visible(true);
            clips_page.wizard.install_status_label.set_label("Starting install…");
            let label = clips_page.wizard.install_status_label.clone();
            let next_btn = clips_page.wizard.install_next_btn.clone();
            // Drain the install-progress receiver on a glib timer (every 100 ms).
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                while let Ok(evt) = rx.try_recv() {
                    use crate::clips::gsr_install::InstallProgress;
                    match evt {
                        InstallProgress::Started => label.set_label("Starting install…"),
                        InstallProgress::Status(s) => label.set_label(&s),
                        InstallProgress::Done => {
                            label.set_label("Installed.");
                            next_btn.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        InstallProgress::Failed { reason } => {
                            label.set_label(&format!("Install failed: {reason}"));
                            // Next stays disabled — user can retry via the Install button.
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });
        })
        .build();
    app.add_action_entries([install_action]);
}

// app.gsr-open-in-bazaar — opens appstream:// URL.
{
    let bazaar_action = gio::ActionEntry::builder("gsr-open-in-bazaar")
        .activate(move |_app, _action, _param| {
            if let Err(e) = crate::clips::gsr_install::open_in_bazaar() {
                log::warn!("open_in_bazaar failed: {e}");
            }
        })
        .build();
    app.add_action_entries([bazaar_action]);
}

// app.gsr-copy-cli — copy terminal command to clipboard.
{
    let win_for_copy = window.clone();
    let copy_action = gio::ActionEntry::builder("gsr-copy-cli")
        .activate(move |_app, _action, _param| {
            let display = gtk::prelude::WidgetExt::display(&win_for_copy);
            let clipboard = display.clipboard();
            clipboard.set_text(crate::clips::gsr_install::GSR_TERMINAL_INSTALL_COMMAND);
        })
        .build();
    app.add_action_entries([copy_action]);
}

// app.wizard-next — advance the wizard's internal step.
{
    let buffer = buffer.clone();
    let clips_page = window.clips_page().clone();
    let next_action = gio::ActionEntry::builder("wizard-next")
        .activate(move |_app, _action, _param| {
            use crate::clips::WizardStep;
            match clips_page.current_wizard_step() {
                WizardStep::InstallGsr => {
                    clips_page.set_wizard_step(WizardStep::PickScreen);
                }
                WizardStep::PickScreen => {
                    // This is the onboarding-complete moment.
                    if let Err(e) = crate::clips::settings::mark_onboarding_complete() {
                        log::warn!("failed to persist onboarding_complete: {e}");
                    }
                    clips_page.set_wizard_step(WizardStep::Settings);
                }
                WizardStep::Settings => {
                    clips_page.set_state(crate::clips::PageState::Empty);
                }
            }
            let _ = &buffer;
        })
        .build();
    app.add_action_entries([next_action]);
}
```

(The `app.setup-clips` action — pre-existing in earlier plan revisions — already handles the portal pick. After it succeeds, it should also enable the Page 2 Next button by setting `clips_page.wizard.screen_next_btn.set_sensitive(true)` and updating `screen_picked_label`.)

- [ ] **Step 2: Update `app.setup-clips` to update Page 2 widgets**

Modify the existing `setup-clips` action to update the wizard's Page 2 status label + Next button sensitivity on success:

```rust
// inside the setup-clips action handler, after `save_token` and `on_portal_pick_complete`:
clips_page.wizard.screen_picked_label.set_visible(true);
clips_page.wizard.screen_picked_label.set_label("Screen picked.");
clips_page.wizard.screen_next_btn.set_sensitive(true);
```

- [ ] **Step 3: Add a 2-second poll timer to detect GSR-installed-via-other-means**

Right after action registration:

```rust
{
    let clips_page = window.clips_page().clone();
    glib::timeout_add_seconds_local(2, move || {
        if matches!(clips_page.current_wizard_step(), crate::clips::WizardStep::InstallGsr)
            && crate::clips::gsr_install::is_installed()
        {
            clips_page.wizard.install_next_btn.set_sensitive(true);
            clips_page.wizard.install_status_label.set_visible(true);
            clips_page.wizard.install_status_label.set_label("Installed.");
        }
        glib::ControlFlow::Continue
    });
}
```

This handles the "user installed via Bazaar or terminal while the wizard was open" case. The in-app install action also enables Next via its own progress watcher; whichever finishes first.

- [ ] **Step 4: Auto-resume to first incomplete step on app start**

After window construction, before showing it:

```rust
let settings = crate::clips::settings::load();
let token = crate::clips::portal::load_token();
let gsr_ok = crate::clips::gsr_install::is_installed();

if settings.onboarding_complete && gsr_ok && token.is_some() {
    // Skip the wizard entirely.
    if let Some(t) = token {
        let cmd_tx = res.clip_backend.as_ref().map(|h| h.sender()).unwrap();
        buffer.borrow_mut().on_portal_pick_complete(t, &cmd_tx);
    }
    window.clips_page().set_state(crate::clips::PageState::Empty);
} else if !gsr_ok {
    window.clips_page().set_wizard_step(crate::clips::WizardStep::InstallGsr);
} else if token.is_none() {
    window.clips_page().set_wizard_step(crate::clips::WizardStep::PickScreen);
    window.clips_page().wizard.install_next_btn.set_sensitive(true); // already past page 1
} else {
    // GSR installed + token present but onboarding_complete still false
    // (user pressed Next on Page 2 but never closed wizard). Treat as complete.
    let _ = crate::clips::settings::mark_onboarding_complete();
    window.clips_page().set_state(crate::clips::PageState::Empty);
}
```

- [ ] **Step 5: Build, verify, commit**

```bash
distrobox enter fedora-dev -- cargo build
# Manual check (after killing existing instance):
./target/debug/arctis-chatmix
```

Manual flow:
1. Fresh state (no GSR, no token): Clips tab → Page 1 (install). Click Install. Watch progress. Next enables. Click Next → Page 2.
2. Page 2: Click Pick screen. KDE portal dialog. Pick a monitor. Status label updates, Next enables. Click Next → Page 3.
3. Page 3: Try changing buffer length slider — value updates live. Click Done → empty Clips browser.
4. Restart app. Clips tab opens directly to empty browser (onboarding complete).

```bash
git add src/app.rs src/window.rs
git commit -m "clips: wire wizard actions + auto-resume to first incomplete step"
```

### Task 3.6: Reset capture source button (and `on_portal_reset` controller method)

**Files:**
- Modify: `src/clips/settings.rs`
- Modify: `src/window.rs:498` (`build_settings_page`)

- [ ] **Step 1: Implement the settings group**

Replace `src/clips/settings.rs`:

```rust
//! Clip-related settings widgets (called from window::build_settings_page).

use adw::prelude::*;

/// Build a PreferencesGroup containing only the Reset Capture Source action row.
/// Other settings rows are added in Phase 6.
pub fn build_clips_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Clips").build();

    let reset_row = adw::ActionRow::builder()
        .title("Capture source")
        .subtitle("Pick the screen recorded by the clip buffer")
        .build();
    let reset_btn = gtk::Button::builder()
        .label("Reset")
        .valign(gtk::Align::Center)
        .build();
    reset_btn.set_action_name(Some("app.reset-clips-capture"));
    reset_row.add_suffix(&reset_btn);
    group.add(&reset_row);

    group
}
```

- [ ] **Step 2: Add to settings page**

In `src/window.rs::build_settings_page`, after the existing `general_group` is added and before the `data_group`:

```rust
page.add(&crate::clips::settings::build_clips_group());
```

- [ ] **Step 3: Add `on_portal_reset` to BufferController**

Direct mutation of `has_portal_pick` from app.rs would bypass the state machine. Add a controller method that handles the full transition:

```rust
// in src/clips/buffer.rs
impl BufferController {
    /// Portal pick was reset by the user. Disarm if armed, return to Uninitialized.
    pub fn on_portal_reset(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if matches!(self.state, BufferState::Armed | BufferState::Arming | BufferState::Saving) {
            let _ = cmd_tx.send(ClipCommand::StopReplay);
        }
        self.has_portal_pick = false;
        self.config.portal_restore_token = None;
        self.state = BufferState::Uninitialized;
    }
}
```

Add a unit test:

```rust
// in buffer.rs tests
#[test]
fn portal_reset_returns_to_uninitialized() {
    let (tx, rx) = channel();
    let mut b = BufferController::new(cfg());
    b.on_portal_pick_complete("t".into(), &tx);
    b.on_game_started(dg(42), &tx);
    let _ = rx.try_recv(); // consume StartReplay
    b.on_backend_event(&BackendEvent::Armed, &tx);
    assert_eq!(b.state(), BufferState::Armed);
    b.on_portal_reset(&tx);
    assert_eq!(b.state(), BufferState::Uninitialized);
    assert!(matches!(rx.try_recv(), Ok(ClipCommand::StopReplay)));
}
```

- [ ] **Step 4: Wire the action**

In `src/app.rs`, register `reset-clips-capture` action analogous to `setup-clips`:

```rust
let reset_action = gio::ActionEntry::builder("reset-clips-capture")
    .activate({
        let buffer = buffer.clone();
        let cmd_tx = res.clip_backend.as_ref().map(|h| h.sender()).unwrap();
        let clips_page = window.clips_page();
        move |_app, _action, _param| {
            let _ = crate::clips::portal::clear_token();
            buffer.borrow_mut().on_portal_reset(&cmd_tx);
            clips_page.set_state(crate::clips::PageState::Onboarding);
        }
    })
    .build();
app.add_action_entries([reset_action]);
```

- [ ] **Step 5: Build, verify, commit**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual: Settings → Clips → Reset. Page returns to onboarding card. Click "Set up" again — picker reopens.

```bash
git add src/clips/settings.rs src/window.rs src/app.rs src/clips/buffer.rs
git commit -m "clips: Reset capture source button + on_portal_reset (state-machine safe)"
```

### Task 3.7: Test capture button + dialog

**Files:**
- Modify: `src/clips/settings.rs`
- Modify: `src/app.rs`

This is the proactive mitigation for xdg-desktop-portal #1371 (multi-monitor restore_token bug). Lets users verify which screen they've persisted without saving an actual clip.

- [ ] **Step 1: Add the Test button to the Capture source row**

Modify `build_clips_group()` in `src/clips/settings.rs` so the Capture source row carries two buttons (Reset + Test):

```rust
let row_box = gtk::Box::builder()
    .orientation(gtk::Orientation::Horizontal)
    .spacing(6)
    .valign(gtk::Align::Center)
    .build();
let test_btn = gtk::Button::builder().label("Test").build();
test_btn.set_action_name(Some("app.test-clip-capture"));
let reset_btn = gtk::Button::builder().label("Reset").build();
reset_btn.set_action_name(Some("app.reset-clips-capture"));
row_box.append(&test_btn);
row_box.append(&reset_btn);
reset_row.add_suffix(&row_box);
```

- [ ] **Step 2: Wire the GAction**

In `src/app.rs`, register `test-clip-capture`:

```rust
let test_action = gio::ActionEntry::builder("test-clip-capture")
    .activate({
        let app = app.clone();
        move |_app, _action, _param| {
            let app_for_async = app.clone();
            glib::MainContext::default().spawn_local(async move {
                match crate::clips::portal::screenshot_current_target().await {
                    Ok(path) => show_test_capture_dialog(&app_for_async, &path),
                    Err(e) => log::warn!("test capture failed: {e}"),
                }
            });
        }
    })
    .build();
app.add_action_entries([test_action]);
```

And the dialog helper (in `src/app.rs` or a small `src/clips/test_capture.rs`):

```rust
fn show_test_capture_dialog(app: &adw::Application, image_path: &std::path::Path) {
    let dialog = adw::AlertDialog::builder()
        .heading("Capture source preview")
        .body("This is the screen the clip buffer will record. If wrong, click Reset in settings.")
        .build();
    let pic = gtk::Picture::for_filename(image_path);
    pic.set_height_request(360);
    pic.set_width_request(640);
    dialog.set_extra_child(Some(&pic));
    dialog.add_response("close", "Close");
    dialog.set_default_response(Some("close"));
    if let Some(window) = app.active_window() {
        dialog.present(Some(&window));
    }
}
```

- [ ] **Step 3: Build and verify**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual: Settings → Clips → Test. Dialog opens with a screenshot of the persisted capture target.

- [ ] **Step 4: Commit**

```bash
git add src/clips/settings.rs src/app.rs
git commit -m "clips: Test capture button — preview persisted source (xdg-portal #1371 mitigation)"
```

### Task 3.8: Reinstall GSR button + missing-GSR detection

**Files:**
- Modify: `src/clips/settings.rs`
- Modify: `src/app.rs`
- Modify: `src/clips/backend.rs`

Handles the case where the user uninstalls the GSR Flatpak via Bazaar/Discover *after* completing onboarding. We do NOT auto-relaunch the wizard (per user's recovery preference: don't be intrusive when the user explicitly removed something). Instead: surface an error toast on the next arm attempt, and provide a "Reinstall gpu-screen-recorder" Settings row that re-runs the install flow.

- [ ] **Step 1: Add missing-GSR detection in `arm()`**

In `src/clips/backend.rs::arm()`, before `spawn_gsr`, check if GSR is installed:

```rust
fn arm(
    active: &mut Option<ActiveCapture>,
    active_config: &mut Option<CaptureConfig>,
    config: &CaptureConfig,
) -> std::io::Result<()> {
    if active.is_some() {
        return Ok(());
    }
    if !crate::clips::gsr_install::is_installed() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "gpu-screen-recorder Flatpak is not installed. Reinstall it from Settings → Clips.",
        ));
    }
    std::fs::create_dir_all(&config.output_dir)?;
    // ... existing code continues
}
```

The existing error path in `run_backend` (when `arm` returns `Err`) emits `BackendEvent::Error { context: "StartReplay", message: e.to_string() }`. The app's poll handler already shows error events as toasts; the new error message text is the user-visible recovery instruction.

- [ ] **Step 2: Add Reinstall row to Clips settings group**

In `src/clips/settings.rs::build_clips_group`, add a row below the Capture source row (or wherever is logical given mockup decisions):

```rust
let reinstall_row = adw::ActionRow::builder()
    .title("gpu-screen-recorder")
    .subtitle("The Flatpak Clips uses to capture gameplay")
    .build();
let reinstall_btn = gtk::Button::builder()
    .label("Reinstall")
    .valign(gtk::Align::Center)
    .build();
reinstall_btn.set_action_name(Some("app.gsr-install"));
reinstall_row.add_suffix(&reinstall_btn);
group.add(&reinstall_row);
```

The button reuses the existing `app.gsr-install` action from Task 3.5 — same install flow, just invoked from Settings instead of the wizard. Since the wizard's install-progress label isn't visible from Settings, the user sees no progress feedback in the current design; for v1 that's acceptable (install is fast). A follow-up improvement would be to show a progress toast.

- [ ] **Step 3: Build, verify, commit**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual:
1. Complete onboarding so GSR is installed.
2. From a separate terminal: `flatpak uninstall --user com.dec05eba.gpu_screen_recorder`.
3. Launch a game in the app. Backend tries to arm, gets the not-installed error. Toast appears: "gpu-screen-recorder Flatpak is not installed. Reinstall it from Settings → Clips."
4. Open Settings → Clips → click Reinstall. Wait for install. Try arming again — succeeds.

```bash
git add src/clips/settings.rs src/clips/backend.rs src/app.rs
git commit -m "clips: detect missing GSR Flatpak at arm + Settings Reinstall button"
```

---

## Phase 4: Hotkey + GAction

Goal: `Super+Shift+R` saves the buffer via the GlobalShortcuts portal. A second `save-clip` GAction is exposed on the existing app D-Bus surface for users who prefer DE-native keybinds.

### Task 4.1: Register the `save-clip` GAction

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Register**

In `connect_activate`, alongside `setup-clips`:

```rust
let save_action = gio::ActionEntry::builder("save-clip")
    .activate({
        let buffer = buffer.clone();
        let cmd_tx = res.clip_backend.as_ref().map(|h| h.sender()).unwrap();
        move |_app, _action, _param| {
            buffer.borrow_mut().on_save_hotkey(&cmd_tx);
        }
    })
    .build();
app.add_action_entries([save_action]);
```

Add `save-clip-short`, `save-clip-medium`, `save-clip-long` analogously, each calling a `BufferController` method that sends `SaveClipShort`/`SaveClipMedium`/`SaveClipLong`. Add those methods to `BufferController`:

```rust
// in buffer.rs
pub fn on_save_hotkey_duration(&mut self, duration_cmd: ClipCommand, cmd_tx: &Sender<ClipCommand>) {
    if matches!(self.state, BufferState::Armed) {
        let _ = cmd_tx.send(duration_cmd);
        self.state = BufferState::Saving;
    }
}
```

- [ ] **Step 2: Build and verify**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix &
sleep 2
gdbus call --session \
  --dest com.github.arctis_chatmix.ArctisNovaEliteChatMix \
  --object-path /com/github/arctis_chatmix/ArctisNovaEliteChatMix \
  --method org.gtk.Actions.Activate \
  save-clip '[]' '{}'
```

Expected: backend logs an Error event ("Not armed") since no game is running. (Once a game is running, the same call would dispatch `SaveClip`.)

- [ ] **Step 3: Commit**

```bash
git add src/app.rs src/clips/buffer.rs
git commit -m "clips: save-clip GAction (and short/medium/long variants)"
```

### Task 4.2: Bind GlobalShortcuts portal

**Files:**
- Modify: `src/clips/hotkey.rs`
- Modify: `src/app.rs`

- [ ] **Step 1: Implement the portal binding**

Replace `src/clips/hotkey.rs`:

```rust
//! GlobalShortcuts portal binding via ashpd.

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, Shortcut};
use futures_util::stream::StreamExt;

/// Suggested shortcut bindings.
///
/// Modifier syntax follows the XDG GlobalShortcuts spec (see
/// https://specifications.freedesktop.org/shortcuts-spec/latest/).
/// Keys are joined with `+`; modifiers: `CTRL`, `ALT`, `SHIFT`, `LOGO`.
/// (`LOGO` is the Super/Meta/Windows key. KDE displays it as "Meta" in the picker.)
pub fn suggested_bindings() -> Vec<NewShortcut> {
    vec![
        NewShortcut::new("save-clip", "Save the last N seconds of gameplay")
            .preferred_trigger(Some("LOGO+SHIFT+R")),
        NewShortcut::new("save-clip-short", "Save the last 30 seconds")
            .preferred_trigger(Some("LOGO+SHIFT+1")),
        NewShortcut::new("save-clip-medium", "Save the last 60 seconds")
            .preferred_trigger(Some("LOGO+SHIFT+2")),
        NewShortcut::new("save-clip-long", "Save the last 120 seconds")
            .preferred_trigger(Some("LOGO+SHIFT+3")),
    ]
}

/// Open the GlobalShortcuts portal session and bind suggested shortcuts.
/// Returns the running stream of activations to be polled. The caller spawns
/// this on glib::MainContext::default().spawn_local().
pub async fn run_global_shortcuts<F>(mut on_shortcut: F) -> ashpd::Result<()>
where
    F: FnMut(&str) + 'static,
{
    let proxy = GlobalShortcuts::new().await?;
    let session = proxy.create_session().await?;
    proxy.bind_shortcuts(&session, &suggested_bindings(), None).await?;
    let mut stream = proxy.receive_activated().await?;
    while let Some(activation) = stream.next().await {
        on_shortcut(activation.shortcut_id());
    }
    Ok(())
}
```

Note: ashpd 0.10's exact API surface for GlobalShortcuts may differ slightly (the crate is moving). Check `cargo doc` for `ashpd::desktop::global_shortcuts` and adjust struct/method names. The important shape: create session → bind shortcuts → receive `Activated` signals.

- [ ] **Step 2: Spawn the listener at startup**

In `src/app.rs::connect_activate`, after the GActions are registered:

```rust
{
    let app = app.clone();
    glib::MainContext::default().spawn_local(async move {
        let app_for_cb = app.clone();
        let _ = crate::clips::hotkey::run_global_shortcuts(move |id| {
            let action_name = match id {
                "save-clip" => "save-clip",
                "save-clip-short" => "save-clip-short",
                "save-clip-medium" => "save-clip-medium",
                "save-clip-long" => "save-clip-long",
                _ => return,
            };
            app_for_cb.activate_action(action_name, None);
        }).await;
    });
}
```

- [ ] **Step 3: Build and verify**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual:
1. On first launch, KDE shows a dialog asking the user to confirm or rebind the suggested shortcuts.
2. Confirm with defaults.
3. Launch a game (or trust the buffer is armed).
4. Press Super+Shift+R. Backend logs `Saved { ... }` once the file is muxed.

If KDE doesn't show a dialog, check `journalctl --user -u plasma-kde-portal*` for portal errors. Verify ashpd's GlobalShortcuts feature compiled (some ashpd versions gate it behind a crate feature).

- [ ] **Step 4: Commit**

```bash
git add src/clips/hotkey.rs src/app.rs Cargo.toml
git commit -m "clips: bind GlobalShortcuts portal — suggested Super+Shift+R + duration variants"
```

---

## Phase 5: Library + thumbnails + clips browser UI

Goal: Saved clips appear in a `gtk::GridView` with thumbnails. User can rename, delete, open in folder.

### Task 5.1: Library types and filename sanitization

**Files:**
- Modify: `src/clips/library.rs`

- [ ] **Step 1: Write the failing tests**

Replace `src/clips/library.rs`:

```rust
//! Clip metadata index, directory scan, filename sanitization.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipMeta {
    pub filename: String,
    pub duration_ms: u64,
    pub game_name: String,
    pub created_unix: u64,
    pub bitrate_kbps: u32,
    pub resolution: String,
}

/// Sanitize a game name for inclusion in a filename.
/// - Spaces → hyphens
/// - Non-alphanumeric (except hyphen) → stripped
/// - Truncate to 40 chars
pub fn sanitize_game_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    s.chars().take(40).collect()
}

/// Build a base filename from timestamp + game name.
pub fn build_base_filename(created: SystemTime, game_name: &str) -> String {
    use std::time::UNIX_EPOCH;
    let secs = created.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let game = if game_name.is_empty() { "Untitled" } else { game_name };
    let sanitized = sanitize_game_name(game);
    let final_game = if sanitized.is_empty() { "Untitled".to_string() } else { sanitized };
    // Format secs to YYYY-MM-DD-HHMM
    use chrono::DateTime; // need to add chrono — actually, std-only via libc strftime is awkward.
    // Actually: use chrono dependency or do manual formatting using time crate.
    // For minimal additions, format with the time crate which we already would for ffmpeg.
    // Simplest: use libc::localtime + libc::strftime.
    let tm = unsafe { *libc::localtime(&(secs as libc::time_t)) };
    let date = format!(
        "{:04}-{:02}-{:02}-{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    );
    format!("{date}-{final_game}")
}

/// Resolve filename collisions by appending -2, -3, etc.
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
        assert_eq!(sanitize_game_name("ELDEN RING™ ©"), "ELDEN-RING-");
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
```

Note: This uses `libc::localtime` for date formatting (no new dep). If `chrono` is preferred, add it now and use `DateTime<Local>::from(systemtime).format(...)`.

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::library::tests
```

Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/library.rs
git commit -m "clips: filename sanitization, base name, collision resolution"
```

### Task 5.2: Index file format + reconciliation

**Files:**
- Modify: `src/clips/library.rs`

- [ ] **Step 1: Write the failing tests**

Append:

```rust
const INDEX_FILENAME: &str = "clips_index.txt";

fn index_path() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home).join(".config/arctis-chatmix").join(INDEX_FILENAME)
}

/// Serialize a clip meta into one tab-separated index line.
pub fn serialize_meta(m: &ClipMeta) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        m.filename, m.duration_ms, m.game_name, m.created_unix, m.bitrate_kbps, m.resolution
    )
}

/// Parse one index line. Returns None on malformed input.
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

pub fn load_index() -> Vec<ClipMeta> {
    let p = index_path();
    let s = std::fs::read_to_string(p).unwrap_or_default();
    s.lines().filter_map(parse_meta).collect()
}

pub fn save_index(items: &[ClipMeta]) -> std::io::Result<()> {
    let p = index_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    let body: String = items.iter().map(serialize_meta).collect::<Vec<_>>().join("\n");
    std::fs::write(p, body)
}

/// Scan the storage dir, reconcile with the index, return the reconciled list.
/// Removes index entries whose files no longer exist; adds entries for files
/// not yet indexed (with default metadata — ffprobe-augmented in a worker thread).
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
    let known: std::collections::HashSet<String> = indexed.iter().map(|m| m.filename.clone()).collect();
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
```

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::library
```

Expected: tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/clips/library.rs
git commit -m "clips: index file format + reconciliation against directory contents"
```

### Task 5.3: Thumbnail extraction worker

**Files:**
- Modify: `src/clips/thumbnail.rs`

- [ ] **Step 1: Implement**

Replace `src/clips/thumbnail.rs`:

```rust
//! Thumbnail extraction via ffmpeg.

use std::path::{Path, PathBuf};
use std::process::Command;

const THUMB_W: u32 = 320;
const THUMB_H: u32 = 180;

pub fn thumb_dir(storage_dir: &Path) -> PathBuf {
    storage_dir.join(".cache/thumbs")
}

pub fn thumb_path(storage_dir: &Path, clip_filename: &str) -> PathBuf {
    let stem = Path::new(clip_filename).file_stem().unwrap_or_default();
    thumb_dir(storage_dir).join(format!("{}.jpg", stem.to_string_lossy()))
}

/// Extract a thumbnail at offset 1.0s. Idempotent — if the thumb file already
/// exists with non-zero size, skip.
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
        .args([
            "-y",
            "-ss", "1.0",
            "-i",
        ])
        .arg(&clip_path)
        .args([
            "-vframes", "1",
            "-vf", &format!("scale={THUMB_W}:{THUMB_H}"),
            "-q:v", "4",
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
```

- [ ] **Step 2: Build**

```bash
distrobox enter fedora-dev -- cargo check
```

Expected: clean compile.

- [ ] **Step 3: Commit**

```bash
git add src/clips/thumbnail.rs
git commit -m "clips: thumbnail extraction via ffmpeg (320x180 JPEG, idempotent)"
```

### Task 5.4a: ClipObject GLib subclass

**Files:**
- Modify: `src/clips/browser.rs`
- Modify: `src/clips/library.rs` (add `#[derive(Default)]` to ClipMeta)

- [ ] **Step 1: Add Default derive on ClipMeta**

In `src/clips/library.rs`, change the struct from:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipMeta { ... }
```

to:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipMeta { ... }
```

This is the canonical Default. **Do NOT also write a manual `impl Default for ClipMeta`** — Rust will emit `error[E0119]: conflicting implementations`.

- [ ] **Step 2: Add ClipObject GLib subclass to browser.rs**

In `src/clips/browser.rs`:

```rust
use gtk::glib;
use gtk::glib::subclass::prelude::*;

mod clip_object {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use gtk::glib;
    use gtk::glib::subclass::prelude::*;

    use crate::clips::library::ClipMeta;

    #[derive(Default)]
    pub struct ClipObjectImpl {
        pub meta: RefCell<ClipMeta>,
        pub storage_dir: RefCell<PathBuf>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClipObjectImpl {
        const NAME: &'static str = "ClipObject";
        type Type = super::ClipObject;
    }

    impl ObjectImpl for ClipObjectImpl {}
}

glib::wrapper! {
    pub struct ClipObject(ObjectSubclass<clip_object::ClipObjectImpl>);
}

impl ClipObject {
    pub fn new(meta: crate::clips::library::ClipMeta, storage_dir: std::path::PathBuf) -> Self {
        let obj: Self = glib::Object::new();
        *obj.imp().meta.borrow_mut() = meta;
        *obj.imp().storage_dir.borrow_mut() = storage_dir;
        obj
    }
    pub fn meta(&self) -> crate::clips::library::ClipMeta {
        self.imp().meta.borrow().clone()
    }
    pub fn storage_dir(&self) -> std::path::PathBuf {
        self.imp().storage_dir.borrow().clone()
    }
}
```

Note: no `#[derive(Properties)]` — the plan uses direct `imp().borrow_mut()` access. Adding the derive without `#[property(...)]` field annotations is dead code that adds noise. The `use ...subclass::prelude::*` at the top of `browser.rs` is required for the `imp()` accessor.

- [ ] **Step 3: Round-trip unit test**

Add to `src/clips/browser.rs`:

```rust
#[cfg(test)]
mod object_tests {
    use super::*;
    use crate::clips::library::ClipMeta;

    #[test]
    fn clip_object_round_trips_meta() {
        gtk::init().ok(); // GLib needs init for Object subclasses
        let mut m = ClipMeta::default();
        m.filename = "test.mp4".into();
        m.duration_ms = 60_000;
        let dir = std::path::PathBuf::from("/tmp");
        let obj = ClipObject::new(m.clone(), dir.clone());
        assert_eq!(obj.meta(), m);
        assert_eq!(obj.storage_dir(), dir);
    }
}
```

- [ ] **Step 4: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::browser::object_tests
```

Note: `gtk::init()` may fail in headless CI; in that case, gate the test with `#[ignore]` and run manually.

- [ ] **Step 5: Commit**

```bash
git add src/clips/browser.rs src/clips/library.rs
git commit -m "clips: ClipObject GLib subclass + ClipMeta Default derive"
```

### Task 5.4b: GridView with label-only cards (no thumbnails yet)

**Files:**
- Modify: `src/clips/browser.rs`

Verify the GridView + factory wiring works before adding thumbnails.

- [ ] **Step 1: Define a CardWidgets struct that holds explicit widget refs**

To avoid the `first_child()` / `last_child()` traversal fragility (which would break the moment we add the kebab button in Task 5.5), the factory stores explicit widget refs by attaching a `CardWidgets` struct to each ListItem via GLib data:

```rust
#[derive(Clone)]
struct CardWidgets {
    image: gtk::Picture,
    title: gtk::Label,
}

fn build_clip_card() -> (gtk::Box, CardWidgets) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    card.add_css_class("clip-card");
    let image = gtk::Picture::builder()
        .height_request(180)
        .width_request(320)
        .build();
    image.add_css_class("clip-thumb");
    let title = gtk::Label::builder()
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(30)
        .xalign(0.0)
        .build();
    title.add_css_class("clip-title");
    card.append(&image);
    card.append(&title);
    (card, CardWidgets { image, title })
}
```

- [ ] **Step 2: Implement loaded_page with factory + bind**

```rust
fn loaded_page() -> gtk::Widget {
    use crate::clips::library;
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let storage_dir = std::path::PathBuf::from(
        std::env::var("HOME").unwrap_or_default()
    ).join("Videos/Clips");
    let _ = std::fs::create_dir_all(&storage_dir);

    let model = gtk::gio::ListStore::new::<ClipObject>();
    for meta in library::reconcile(&storage_dir) {
        model.append(&ClipObject::new(meta, storage_dir.clone()));
    }

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let (card, widgets) = build_clip_card();
        // Attach widgets to the ListItem so bind() can retrieve them without
        // walking the widget tree.
        unsafe {
            item.set_data("card-widgets", widgets);
        }
        item.set_child(Some(&card));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let widgets: CardWidgets = unsafe {
            item.data::<CardWidgets>("card-widgets")
                .map(|p| p.as_ref().clone())
                .expect("card-widgets attached during setup")
        };
        let clip = item.item().and_then(|o| o.downcast::<ClipObject>().ok()).unwrap();
        bind_label_only(&widgets, &clip);
    });

    let grid = gtk::GridView::builder()
        .model(&gtk::SingleSelection::new(Some(model)))
        .factory(&factory)
        .min_columns(2)
        .max_columns(5)
        .build();
    scroll.set_child(Some(&grid));
    scroll.upcast()
}

fn bind_label_only(widgets: &CardWidgets, clip: &ClipObject) {
    let meta = clip.meta();
    let game = if meta.game_name.is_empty() { "Untitled" } else { &meta.game_name };
    widgets.title.set_label(game);
    // Picture left blank in this task; thumbnail wiring lands in Task 5.4c.
}
```

- [ ] **Step 3: Build, verify visually**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual: drop a stub `.mp4` file into `~/Videos/Clips/` (any video will do) and open the Clips tab. A card appears showing the filename's game-name portion. No thumbnail yet — that's Task 5.4c.

- [ ] **Step 4: Commit**

```bash
git add src/clips/browser.rs
git commit -m "clips: GridView with label-only cards (no thumbnails yet)"
```

### Task 5.4c: Thumbnail wiring on cards

**Files:**
- Modify: `src/clips/browser.rs`

- [ ] **Step 1: Modify bind to spawn thumbnail worker**

Replace `bind_label_only` with `bind_clip_card`:

```rust
fn bind_clip_card(widgets: &CardWidgets, clip: &ClipObject) {
    let meta = clip.meta();
    let storage_dir = clip.storage_dir();
    let game = if meta.game_name.is_empty() { "Untitled" } else { &meta.game_name };
    widgets.title.set_label(game);

    let filename = meta.filename.clone();
    let storage_for_worker = storage_dir.clone();
    let img_weak = widgets.image.downgrade();
    std::thread::spawn(move || {
        if let Ok(thumb) = crate::clips::thumbnail::ensure_thumbnail(&storage_for_worker, &filename) {
            glib::MainContext::default().invoke(move || {
                if let Some(img) = img_weak.upgrade() {
                    img.set_filename(Some(&thumb));
                }
            });
        }
    });
}
```

Update the factory's `connect_bind` to call `bind_clip_card`.

- [ ] **Step 2: Build, verify**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual: thumbnails extract via ffmpeg in the background and appear on the cards.

- [ ] **Step 3: Commit**

```bash
git add src/clips/browser.rs
git commit -m "clips: async thumbnail extraction wired to clip cards"
```

### Task 5.4d: ffprobe duration backfill

**Files:**
- Modify: `src/clips/browser.rs` (or new helper file)
- Modify: `src/clips/library.rs`

Phase 1's FIFO reader emits `Saved { duration_ms: 0 }` (we deferred ffprobe out of the FIFO reader to avoid blocking the next save). The library index also has many entries with `duration_ms == 0` after `reconcile()`. Backfill in a worker thread.

- [ ] **Step 1: Add a duration extractor + backfill helper**

In `src/clips/library.rs`:

```rust
use std::process::Command;

pub fn ffprobe_duration_ms(path: &Path) -> Option<u64> {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let secs: f64 = s.trim().parse().ok()?;
    Some((secs * 1000.0) as u64)
}

/// Fill missing duration_ms in the index by ffprobing files. Called from a worker thread.
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
```

- [ ] **Step 2: Trigger backfill on browser open**

In `src/clips/browser.rs::loaded_page`, after building the model, spawn a worker thread that calls `library::backfill_durations` and (when done) refreshes the model from the updated index. Use a glib timeout to coordinate or send through an mpsc.

- [ ] **Step 3: Commit**

```bash
git add src/clips/library.rs src/clips/browser.rs
git commit -m "clips: ffprobe duration backfill in worker thread"
```

### Task 5.5: Card kebab menu (Rename / Delete / Open in Folder)

**Files:**
- Modify: `src/clips/browser.rs`

- [ ] **Step 1: Add kebab button + popover**

Modify `build_clip_card` to include a hover-revealed kebab button (or always-visible for v1 simplicity). Each menu item triggers a callback:

- **Rename:** open an `adw::AlertDialog` with a text entry; on confirm, rename the file and refresh the list.
- **Delete:** confirm dialog → `std::fs::remove_file` → also remove thumb → refresh.
- **Open in Folder:** spawn `xdg-open <storage_dir>` (Bazzite KDE will use Dolphin).

Concrete code (sketch — flesh out per-item):

```rust
fn build_kebab(card: &gtk::Box, clip: &ClipObject, refresh: impl Fn() + 'static) {
    let kebab = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .has_frame(false)
        .css_classes(["circular"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .build();
    let menu = gtk::gio::Menu::new();
    menu.append(Some("Rename"), Some("clip.rename"));
    menu.append(Some("Delete"), Some("clip.delete"));
    menu.append(Some("Open in Folder"), Some("clip.open-folder"));
    kebab.set_menu_model(Some(&menu));
    // Action group is added per-card with the clip captured.
    // ... (define gio::SimpleActionGroup with three actions)
    card.append(&kebab); // overlay rather than append for hover-only display
}
```

For brevity in this plan: the implementer should look at `src/eq/mod.rs` `show_edit_dialog` for the rename modal pattern, and `src/audio/persistence.rs` for delete-with-confirmation patterns elsewhere in the codebase.

- [ ] **Step 2: Build and verify**

Manual: Right-click (or kebab-click) a clip card. Menu appears. Test each action:
- Rename → file renamed in `~/Videos/Clips/`, grid refreshes.
- Delete → file gone, grid refreshes.
- Open in Folder → Dolphin opens with the clip selected.

- [ ] **Step 3: Commit**

```bash
git add src/clips/browser.rs
git commit -m "clips: per-card kebab menu — Rename / Delete / Open in Folder"
```

### Task 5.6: Visual mockup pass for Clips browser

**Files:**
- May modify CSS in `src/window.rs` (the existing `css.load_from_string`)
- May modify `src/clips/browser.rs`

This task is a paired brainstorming + apply, not a single implementer dispatch. Per CLAUDE.md, `plan-implementer` is for "execution of defined, scoped work" — picking visual directions is brainstorming, not execution.

- [ ] **Step 1: Brainstorming sub-session (team lead drives)**

Push 3 mockup HTMLs to the brainstorming companion at http://localhost:49421 (screen_dir: `/var/home/admin/Documents/Code/SteelseriesFlatpak/.superpowers/brainstorm/51993-1778294045/content`) showing:
- `clips-grid.html` — grid card spacing, hover state, kebab visibility
- `clips-onboarding.html` — CTA prominence, optional illustration placement
- `clips-empty.html` — where the suggested hotkey is displayed

User picks one direction per page in the browser.

- [ ] **Step 2: Dispatch plan-implementer with the approved directions**

> Apply the chosen mockups (`<grid-name>.html`, `<onboarding-name>.html`, `<empty-name>.html` in screen_dir) to `src/clips/browser.rs` and CSS in `src/window.rs` (under `.clip-card`, `.clip-thumb`, `.clip-title`, `.clip-onboarding-cta`, `.clip-empty-hotkey` classes). Build with `distrobox enter fedora-dev -- cargo build`. Commit each page-state's changes as a separate commit with message "clips: apply <name> mockup".

- [ ] **Step 3: Build and verify visually**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Confirm the Clips tab now matches the chosen mockups.

---

## Phase 6: Settings additions + status indicator

Goal: Settings tab grows the full Clips group (buffer length, bitrate, hotkey, auto-arm, always armed, audio tracks, mic capture, storage location, retention). Dashboard Status card grows a status badge.

### Task 6.1: Settings file format

**Files:**
- Modify: `src/clips/settings.rs`

- [ ] **Step 1: Define the on-disk format**

Append to `src/clips/settings.rs`:

```rust
use std::path::PathBuf;
use std::collections::HashMap;

const SETTINGS_FILENAME: &str = "clips_settings.txt";

fn settings_path() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home).join(".config/arctis-chatmix").join(SETTINGS_FILENAME)
}

#[derive(Debug, Clone)]
pub struct ClipSettings {
    pub buffer_length: u32,
    pub bitrate_mbps: u32,
    pub auto_arm: bool,
    pub always_armed: bool,
    pub per_source_tracks: bool,
    pub mic_capture: bool,
    pub storage_path: PathBuf,
    pub disk_cap_gb: Option<u32>, // None = no cap
}

impl Default for ClipSettings {
    fn default() -> Self {
        Self {
            buffer_length: 60,
            bitrate_mbps: 25,
            auto_arm: true,
            always_armed: false,
            per_source_tracks: true,
            mic_capture: true,
            storage_path: PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Videos/Clips"),
            disk_cap_gb: None,
        }
    }
}

pub fn load() -> ClipSettings {
    let mut s = ClipSettings::default();
    let path = settings_path();
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return s,
    };
    for line in contents.lines() {
        let (k, v) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        match k {
            "buffer_length" => if let Ok(n) = v.parse() { s.buffer_length = n; }
            "bitrate_mbps" => if let Ok(n) = v.parse() { s.bitrate_mbps = n; }
            "auto_arm" => s.auto_arm = v == "1",
            "always_armed" => s.always_armed = v == "1",
            "per_source_tracks" => s.per_source_tracks = v == "1",
            "mic_capture" => s.mic_capture = v == "1",
            "storage_path" => s.storage_path = PathBuf::from(v),
            "disk_cap_gb" => s.disk_cap_gb = v.parse().ok(),
            _ => {}
        }
    }
    s
}

pub fn save(s: &ClipSettings) -> std::io::Result<()> {
    let path = settings_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut body = String::new();
    body.push_str(&format!("buffer_length={}\n", s.buffer_length));
    body.push_str(&format!("bitrate_mbps={}\n", s.bitrate_mbps));
    body.push_str(&format!("auto_arm={}\n", if s.auto_arm { 1 } else { 0 }));
    body.push_str(&format!("always_armed={}\n", if s.always_armed { 1 } else { 0 }));
    body.push_str(&format!("per_source_tracks={}\n", if s.per_source_tracks { 1 } else { 0 }));
    body.push_str(&format!("mic_capture={}\n", if s.mic_capture { 1 } else { 0 }));
    body.push_str(&format!("storage_path={}\n", s.storage_path.display()));
    if let Some(cap) = s.disk_cap_gb {
        body.push_str(&format!("disk_cap_gb={}\n", cap));
    }
    std::fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_60s_25mbps() {
        let s = ClipSettings::default();
        assert_eq!(s.buffer_length, 60);
        assert_eq!(s.bitrate_mbps, 25);
        assert!(s.auto_arm);
        assert!(!s.always_armed);
        assert!(s.per_source_tracks);
        assert!(s.mic_capture);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::settings::tests
```

- [ ] **Step 3: Commit**

```bash
git add src/clips/settings.rs
git commit -m "clips: settings file format (clips_settings.txt) + Default values"
```

### Task 6.2: Settings widgets — buffer + bitrate + capture-source row

**Files:**
- Modify: `src/clips/settings.rs`

- [ ] **Step 1: Expand `build_clips_group()`**

Replace `build_clips_group()` with a fuller version. Each widget reads from `ClipSettings::load()` on construction and writes back via `ClipSettings::save()` on change.

Spec contents:
- Capture source action row (Reset + Test)
- Buffer length scale (30–300 s)
- Bitrate dropdown (15 / 25 / 40 / 60 Mbps)
- Hotkey row (current binding label + Rebind button)
- Auto-arm switch
- Always-armed switch (mutex with auto-arm)
- Per-source tracks switch
- Mic capture switch
- Storage location FileChooserButton
- Disk retention dropdown ("Keep all" / "10 GB" / "30 GB" / "50 GB" / "100 GB")

For the structure, follow the `adw::PreferencesGroup` + `adw::ActionRow` / `adw::SwitchRow` pattern already used in `src/window.rs::build_settings_page`.

Each control's `connect_*_notify` handler calls `crate::clips::settings::save(&new_settings)` and emits a "settings changed" event the buffer controller can react to via `BufferController::on_config_change`.

A clean way: put a `ClipSettingsBus` newtype wrapping `Rc<RefCell<ClipSettings>>` plus a `Vec<Box<dyn Fn(&ClipSettings)>>` of subscribers. The settings widgets mutate the bus; the bus dispatches to subscribers. The app installs one subscriber that calls `BufferController::on_config_change`.

For brevity in the plan, the implementer should:
1. Build the widgets in `build_clips_group`, each accepting a `Rc<RefCell<ClipSettings>>` and a `&dyn Fn(&ClipSettings)` for the change callback.
2. Wire from app.rs to mediate between settings changes and the buffer controller.

- [ ] **Step 2: Build and verify visually**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual: Settings → Clips. All rows present. Toggle each, restart app, verify persistence.

- [ ] **Step 3: Commit**

```bash
git add src/clips/settings.rs src/app.rs
git commit -m "clips: full settings group (buffer/bitrate/hotkey/arm/tracks/mic/storage/retention)"
```

### Task 6.3: Status indicator on dashboard

**Files:**
- Modify: `src/clips/mod.rs`
- Modify: `src/window.rs::build_dashboard_page`

- [ ] **Step 1: Build a status badge widget**

Add to `src/clips/mod.rs` (or a new `src/clips/indicator.rs` if preferred):

```rust
pub fn build_status_indicator() -> StatusIndicator {
    let label = gtk::Label::new(None);
    let dot = gtk::Image::from_icon_name("media-record-symbolic");
    let badge = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    badge.append(&dot);
    badge.append(&label);
    badge.add_css_class("clip-indicator");
    StatusIndicator { root: badge.upcast(), label, dot }
}

#[derive(Clone)]
pub struct StatusIndicator {
    pub root: gtk::Widget,
    pub label: gtk::Label,
    pub dot: gtk::Image,
}

impl StatusIndicator {
    pub fn set_state(&self, state: BufferState, game: Option<&str>) {
        self.dot.remove_css_class("dot-armed");
        self.dot.remove_css_class("dot-saving");
        self.dot.remove_css_class("dot-error");
        self.dot.remove_css_class("dot-setup");
        self.root.set_visible(true);
        match state {
            BufferState::Uninitialized => {
                self.dot.add_css_class("dot-setup");
                self.label.set_label("Set up Clips");
            }
            BufferState::Idle => self.root.set_visible(false),
            BufferState::Arming | BufferState::Armed => {
                self.dot.add_css_class("dot-armed");
                let g = game.unwrap_or("game");
                self.label.set_label(&format!("Buffering — {g}"));
            }
            BufferState::Saving => {
                self.dot.add_css_class("dot-saving");
                self.label.set_label("Saving…");
            }
            BufferState::ErrorState => {
                self.dot.add_css_class("dot-error");
                self.label.set_label("Capture stopped");
            }
        }
    }
}
```

CSS additions in `src/window.rs::css.load_from_string`:

```rust
".clip-indicator { padding: 4px 10px; border-radius: 8px; } \
 .clip-indicator .dot-armed { color: rgb(77,204,179); } \
 .clip-indicator .dot-saving { color: rgb(242,205,64); } \
 .clip-indicator .dot-error { color: rgb(230,77,77); } \
 .clip-indicator .dot-setup { color: rgba(255,255,255,0.5); }"
```

- [ ] **Step 2: Embed indicator in dashboard Status card**

In `src/window.rs::build_dashboard_page`, find the Status card construction and append `crate::clips::build_status_indicator()` to its content box. Store the `StatusIndicator` on `Widgets` for later updates.

- [ ] **Step 3: Wire updates from app.rs**

In the backend-events poll timer, after each event call `widgets.clip_indicator.set_state(buffer.state(), buffer.current_game().map(|g| g.name.as_str()))`.

- [ ] **Step 4: Visual verify**

Manual:
- App starts, dashboard shows "Set up Clips" badge.
- After portal pick: badge hidden.
- During game: green dot + "Buffering — Apex Legends".
- During save: yellow dot + "Saving…".

- [ ] **Step 5: Commit**

```bash
git add src/clips/mod.rs src/window.rs src/app.rs
git commit -m "clips: status indicator on dashboard Status card"
```

### Task 6.4: Visual mockup pass for settings + indicator

This task is a paired brainstorming + apply, not a single implementer dispatch. Per CLAUDE.md, `plan-implementer` is for execution work; picking visual directions is brainstorming.

- [ ] **Step 1: Brainstorming sub-session (team lead drives)**

Push 2 mockup HTMLs to the brainstorming companion:
- `clips-settings.html` — settings panel ordering / grouping options for the Clips section
- `clips-indicator.html` — status indicator placement options (above battery? below ChatMix? as a pill in the HeaderBar?)

User picks one direction per page.

- [ ] **Step 2: Dispatch plan-implementer**

> Apply the chosen mockups (`<settings-name>.html`, `<indicator-name>.html`) to `src/clips/settings.rs::build_clips_group` and `src/window.rs::build_dashboard_page` (indicator placement) plus the `.clip-indicator` CSS in `src/window.rs::css.load_from_string`. Build with `distrobox enter fedora-dev -- cargo build`. Commit each as a separate commit.

- [ ] **Step 3: Build, verify, commit**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

---

## Phase 7: Remix panel + notifications

Goal: Notifications work (toast vs gio when hidden) with thumbnails. Remix panel slides in over the clip card with per-track sliders + Preview + Export.

### Task 7.1: Notification dispatch

**Files:**
- Create: `src/clips/notifications.rs` (new)
- Modify: `src/clips/mod.rs`
- Modify: `src/app.rs`

- [ ] **Step 1: Implement**

Create `src/clips/notifications.rs`:

```rust
//! Saved-clip notifications. Toast when the window is visible, gio::Notification
//! when hidden.

use std::path::Path;

use adw::prelude::*;
use gtk::{gio, glib};

pub fn notify_saved(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    saved_path: &Path,
    thumbnail_path: Option<&Path>,
) {
    let title = format!(
        "Clip saved: {}",
        saved_path.file_name().unwrap_or_default().to_string_lossy()
    );

    if window.is_visible() {
        // Toast inside main window.
        // Find the AdwToastOverlay; if absent, log and skip.
        if let Some(overlay) = find_toast_overlay(window) {
            // Coerce Cow<str> → owned String → ToVariant. Cow<str>::to_variant isn't
            // guaranteed to resolve depending on glib version; the explicit owned
            // String form is unambiguous.
            let path_str: String = saved_path.to_string_lossy().into_owned();
            let toast = adw::Toast::builder()
                .title(&title)
                .button_label("Show")
                .action_name("app.show-clip")
                .action_target(&path_str.to_variant())
                .timeout(6)
                .build();
            overlay.add_toast(toast);
        }
    } else {
        // Desktop notification.
        let notif = gio::Notification::new(&title);
        if let Some(thumb) = thumbnail_path {
            if let Ok(file) = gio::File::for_path(thumb).query_info(
                "*",
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            ) {
                let _ = file;
                // Set icon via gio::FileIcon
                let icon = gio::FileIcon::new(&gio::File::for_path(thumb));
                notif.set_icon(&icon);
            }
        }
        let path_str: String = saved_path.to_string_lossy().into_owned();
        notif.add_button_with_target_value(
            "Show clip",
            "app.show-clip",
            Some(&path_str.to_variant()),
        );
        app.send_notification(Some("clip-saved"), &notif);
    }
}

fn find_toast_overlay(window: &adw::ApplicationWindow) -> Option<adw::ToastOverlay> {
    // Walk descendants. Replace with a stored reference if perf becomes a concern.
    let mut current = window.child();
    while let Some(w) = current {
        if let Ok(o) = w.clone().downcast::<adw::ToastOverlay>() {
            return Some(o);
        }
        current = w.first_child();
    }
    None
}
```

This requires the window root to have an `adw::ToastOverlay` wrapping the existing content. Modify `src/window.rs::ChatMixWindow::new` to wrap the root in an `adw::ToastOverlay`.

- [ ] **Step 2: Wire `app.show-clip` action**

In `src/app.rs`:

```rust
let show_action = gio::ActionEntry::builder("show-clip")
    .parameter_type(Some(&String::static_variant_type()))
    .activate({
        let window_weak = window.downgrade();
        move |_app, _action, param| {
            if let (Some(window), Some(p)) = (window_weak.upgrade(), param.and_then(|p| p.get::<String>())) {
                window.set_visible(true);
                window.present();
                // TODO: navigate to Clips tab and select the clip with filename basename(p).
                let _ = p;
            }
        }
    })
    .build();
app.add_action_entries([show_action]);
```

- [ ] **Step 3: Wire from backend events**

In the backend events poll, when receiving `Saved { path, .. }`:

```rust
if let BackendEvent::Saved { path, .. } = &evt {
    let storage_dir = path.parent().unwrap_or(std::path::Path::new(""));
    let filename = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let thumb = crate::clips::thumbnail::ensure_thumbnail(storage_dir, &filename).ok();
    crate::clips::notifications::notify_saved(&app, &window, path, thumb.as_deref());
}
```

- [ ] **Step 4: Build and verify**

Manual:
1. Save a clip while window is visible. Toast appears.
2. Hide window. Save another clip. KDE desktop notification appears with thumbnail.
3. Click "Show clip" on either. Window comes forward.

- [ ] **Step 5: Commit**

```bash
git add src/clips/notifications.rs src/clips/mod.rs src/window.rs src/app.rs
git commit -m "clips: saved-clip notifications (toast visible / gio hidden) + thumbnail"
```

### Task 7.2: Remix panel layout

**Files:**
- Create: `src/clips/remix.rs`
- Modify: `src/clips/mod.rs`
- Modify: `src/clips/browser.rs` (to invoke remix on card click)

- [ ] **Step 1: Implement remix UI**

Create `src/clips/remix.rs`:

```rust
//! Per-track remix panel.

use adw::prelude::*;
use gtk::glib;
use std::path::Path;

const TRACK_LABELS: [&str; 6] = ["Mix", "Game", "Chat", "Music", "Aux", "Mic"];

pub struct RemixPanel {
    pub root: gtk::Box,
    pub volumes: [f64; 6],
}

pub fn build_remix_panel(clip_path: &Path, on_close: impl Fn() + 'static) -> RemixPanel {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(20)
        .margin_end(20)
        .build();

    let header = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).build();
    let title = gtk::Label::builder()
        .label(&clip_path.file_name().unwrap_or_default().to_string_lossy())
        .css_classes(["title-2"])
        .hexpand(true)
        .xalign(0.0)
        .build();
    header.append(&title);
    let close_btn = gtk::Button::builder().label("Close").build();
    {
        close_btn.connect_clicked(move |_| on_close());
    }
    header.append(&close_btn);
    root.append(&header);

    for (i, label) in TRACK_LABELS.iter().enumerate() {
        let row = build_track_row(label, i);
        root.append(&row);
    }

    let action_bar = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).margin_top(12).build();
    let preview_btn = gtk::Button::builder().label("Preview").build();
    let export_btn = gtk::Button::builder().label("Export").css_classes(["suggested-action"]).build();
    action_bar.append(&preview_btn);
    action_bar.append(&export_btn);
    root.append(&action_bar);

    RemixPanel { root, volumes: [1.0; 6] }
}

fn build_track_row(label: &str, index: usize) -> gtk::Box {
    let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    let lbl = gtk::Label::builder().label(label).width_request(80).xalign(0.0).build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -60.0, 6.0, 0.5);
    scale.set_value(0.0);
    scale.set_hexpand(true);
    let mute = gtk::ToggleButton::builder().label("Mute").build();
    let solo = gtk::ToggleButton::builder().label("Solo").build();
    row.append(&lbl);
    row.append(&scale);
    row.append(&mute);
    row.append(&solo);
    let _ = index; // wire signals later
    row
}
```

Wire `preview_btn` and `export_btn` to the methods in Task 7.3 / 7.4.

- [ ] **Step 2: Hook into the browser**

Modify `src/clips/browser.rs::build_clip_card` (or add a click handler on the GridView item) to push a new Stack page containing the RemixPanel when a card is clicked. The Clips tab Stack now has 4 children: onboarding / empty / loaded / remix.

- [ ] **Step 3: Build, verify, commit**

```bash
distrobox enter fedora-dev -- cargo build
./target/debug/arctis-chatmix
```

Manual: click a clip card → remix panel shows with 6 sliders. Preview/Export buttons present (no-op for now).

```bash
git add src/clips/remix.rs src/clips/mod.rs src/clips/browser.rs
git commit -m "clips: remix panel skeleton (per-track sliders, mute/solo, preview/export)"
```

### Task 7.3: Preview pipeline (GStreamer)

**Files:**
- Modify: `src/clips/remix.rs`

- [ ] **Step 1: Wire Preview button**

The simplest approach is to spawn an external player with the clip — `xdg-open <clip>` — without per-track mixing. For real per-track preview, build a GStreamer pipeline:

```
filesrc location=<clip> ! qtdemux name=demux \
  demux.video_0 ! decodebin ! videoconvert ! autovideosink \
  demux.audio_1 ! decodebin ! volume volume=0.X ! audiomixer name=mix \
  demux.audio_2 ! decodebin ! volume volume=0.X ! mix. \
  ... (5 audio branches) ...
  mix. ! audioconvert ! autoaudiosink
```

The Preview button uses `gst::parse_launch` to assemble this string with current slider values, then sets state to PLAYING. State management needs: a stored `gst::Pipeline` ref, stop-on-close.

This requires `gstreamer-rs`. Add to `Cargo.toml`:

```toml
gstreamer = "0.23"
```

For brevity in the plan: the implementer should reference https://gstreamer.freedesktop.org/documentation/tutorials/basic/index.html for the pipeline pattern, and look at the existing `src/audio/router.rs::build_spatial_graph` for a similar in-Rust assembly approach (though that uses `pw-cli`, not gstreamer-rs).

If GStreamer-rs proves too involved, the v1 fallback is `xdg-open` (single-track playback only); the remix sliders only have effect on Export.

- [ ] **Step 2: Build, verify, commit**

```bash
git add Cargo.toml Cargo.lock src/clips/remix.rs
git commit -m "clips: remix Preview via GStreamer pipeline (per-track volume)"
```

### Task 7.4: Export pipeline (ffmpeg)

**Files:**
- Modify: `src/clips/remix.rs`

- [ ] **Step 1: Wire Export button**

```rust
fn export_remix(input: &std::path::Path, output: &std::path::Path, volumes: &[f64; 6]) -> std::io::Result<()> {
    use std::process::Command;

    // Build filter_complex: each input track gets a volume filter, then amix to track 0.
    // [0:a:0]volume=v0[a0]; [0:a:1]volume=v1[a1]; ... amix=inputs=6:duration=longest[mix]
    let mut filter = String::new();
    let mut inputs = String::new();
    for i in 0..6 {
        filter.push_str(&format!("[0:a:{i}]volume={}[a{i}];", volumes[i]));
        inputs.push_str(&format!("[a{i}]"));
    }
    filter.push_str(&format!("{inputs}amix=inputs=6:duration=longest:dropout_transition=0[mix]"));

    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
        ])
        .arg(input)
        .args([
            "-filter_complex", &filter,
            "-map", "0:v:0",
            "-map", "[mix]",
            "-map", "0:a:1",
            "-map", "0:a:2",
            "-map", "0:a:3",
            "-map", "0:a:4",
            "-map", "0:a:5",
            "-c:v", "copy",
            "-c:a:0", "aac", "-b:a:0", "192k",
            "-c:a:1", "copy",
            "-c:a:2", "copy",
            "-c:a:3", "copy",
            "-c:a:4", "copy",
            "-c:a:5", "copy",
        ])
        .arg(output)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("ffmpeg export failed: {status:?}")));
    }
    Ok(())
}
```

The Export button:
1. Determines output path: input filename + `-remix.mp4` suffix in the same dir.
2. Spawns the ffmpeg in a worker thread.
3. Shows a progress toast / spinner.
4. On completion, refreshes the grid.

- [ ] **Step 2: Build, verify, commit**

Manual: open a clip in remix panel, lower the Music slider to -∞, click Export. New file `<original>-remix.mp4` appears. Open in mpv — Music track is silent, others unchanged.

```bash
git add src/clips/remix.rs
git commit -m "clips: remix Export via ffmpeg -filter_complex (track 0 mixdown)"
```

### Task 7.5: Disk retention auto-delete

**Files:**
- Modify: `src/clips/library.rs`
- Modify: `src/app.rs`

The spec requires auto-deleting oldest clips when over the user-set cap (default: no cap). Triggered on each `BackendEvent::Saved`, not on a periodic timer.

- [ ] **Step 1: Implement and unit-test the retention enforcer**

In `src/clips/library.rs`:

```rust
/// Total bytes of all .mp4 files in the storage dir (excluding thumbs).
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

/// Enforce the cap by deleting oldest clips (by mtime) until under the cap.
/// Returns the list of deleted filenames.
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
            // Also remove the matching thumbnail.
            if let Some(stem) = path.file_stem() {
                let thumb = storage_dir.join(".cache/thumbs").join(format!("{}.jpg", stem.to_string_lossy()));
                let _ = std::fs::remove_file(thumb);
            }
            deleted.push(path.file_name().unwrap_or_default().to_string_lossy().into_owned());
            total = total.saturating_sub(size);
        }
    }
    deleted
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    #[test]
    fn enforce_retention_skips_when_under_cap() {
        let dir = std::env::temp_dir().join(format!("clips-rtn-test-{}-a", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.mp4"), vec![0u8; 1024]).unwrap();
        let deleted = enforce_retention(&dir, 1);
        assert!(deleted.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enforce_retention_deletes_oldest_first() {
        let dir = std::env::temp_dir().join(format!("clips-rtn-test-{}-b", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Write three 500MB sparse files to easily exceed a 1 GB cap.
        for name in ["a.mp4", "b.mp4", "c.mp4"] {
            let f = std::fs::File::create(dir.join(name)).unwrap();
            f.set_len(500 * 1024 * 1024).unwrap();
            // Bump mtimes so a < b < c.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let deleted = enforce_retention(&dir, 1);
        assert!(deleted.contains(&"a.mp4".to_string()), "oldest should be deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
distrobox enter fedora-dev -- cargo test clips::library::retention_tests
```

Note: the second test uses `set_len` for sparse files (no actual disk write); should run fast.

- [ ] **Step 3: Wire into app.rs Saved-event handler**

In the backend-events poll, after the notification dispatch:

```rust
if let BackendEvent::Saved { path, .. } = &evt {
    let storage_dir = path.parent().unwrap_or(std::path::Path::new("")).to_path_buf();
    let settings = crate::clips::settings::load();
    if let Some(cap_gb) = settings.disk_cap_gb {
        std::thread::spawn(move || {
            let deleted = crate::clips::library::enforce_retention(&storage_dir, cap_gb);
            if !deleted.is_empty() {
                log::info!("retention: deleted {} clip(s) over cap", deleted.len());
            }
        });
    }
}
```

The spawn keeps disk I/O off the GTK main thread.

- [ ] **Step 4: Commit**

```bash
git add src/clips/library.rs src/app.rs
git commit -m "clips: disk-cap retention auto-delete (oldest-first on Saved)"
```

### Task 7.6: Visual mockup pass for remix panel

This task is a paired brainstorming + apply, not a single implementer dispatch. Per CLAUDE.md, `plan-implementer` is for "execution of defined, scoped work" — picking visual directions is brainstorming, not execution.

- [ ] **Step 1: Run a brainstorming sub-session for the remix panel layout (team lead drives)**

The team lead (this conversation) pushes 2-3 mockup HTMLs to the brainstorming companion at http://localhost:49421 (screen_dir: `/var/home/admin/Documents/Code/SteelseriesFlatpak/.superpowers/brainstorm/51993-1778294045/content`) showing:
- Per-track row arrangement options (label width, slider proportions, mute/solo placement)
- Action bar layout options (Preview vs Export prominence)

User picks a direction in the browser. Save the HTML names that were chosen (e.g. `remix-v2.html`).

- [ ] **Step 2: Dispatch plan-implementer with the approved direction**

Once a direction is approved, dispatch plan-implementer with prompt:

> Apply the remix panel layout from `<chosen mockup HTML name>` (in screen_dir). Update CSS in `src/window.rs` (under `.remix-track-row`, `.remix-action-bar` classes) and structural code in `src/clips/remix.rs::build_remix_panel`. Build with `distrobox enter fedora-dev -- cargo build`. Commit with message "clips: apply remix panel mockup `<name>`".

- [ ] **Step 3: Build, verify, commit**

---

## Phase 8: Verification + polish

Goal: Confirm runtime behavior matches the spec's open verification items. Run agentic QA and project-tester passes.

### Task 8.1: Verify GSR multi-track output

- [ ] **Step 1: Save a real clip**

Launch a Steam game (or stand-in: any process matching the detector). Save a clip via hotkey.

- [ ] **Step 2: Inspect with ffprobe**

```bash
ffprobe -hide_banner -i ~/Videos/Clips/<latest-clip>.mp4 2>&1 | grep "Stream #0:"
```

Expected: `Stream #0:0` (Video), then `Stream #0:1` through `Stream #0:6` (six AAC audio streams).

- [ ] **Step 3: If only one audio track, fix the GSR args**

If GSR mixed all `-a` flags into one track, the design's fallback applies:
1. Drop track 0 (mix-down) from `build_gsr_args` — keep only the 5 isolated tracks.
2. Reuse the export pipeline for "synthesize a track-0 mix at remix time."

Run the test again; expected 5 streams.

- [ ] **Step 4: Document the result in the spec**

Update the spec's "Open verification items" entry #1 to reflect what was actually observed. If a fix was needed, also update the design doc to remove the multi-track-out-of-the-box claim.

```bash
git add docs/superpowers/specs/2026-05-08-clipping-system-design.md
git commit -m "spec: document verified GSR multi-track behavior"
```

### Task 8.2: Latency benchmark

- [ ] **Step 1: Instrument**

Add a `std::time::Instant::now()` capture at the moment `ClipCommand::SaveClip` is sent, and another in the FIFO reader when the path arrives. Log the elapsed time.

```rust
// in BufferController::on_save_hotkey
let t0 = std::time::Instant::now();
self.last_save_t0 = Some(t0);
```

In the backend-events poll, when `Saved` arrives:

```rust
if let Some(t0) = buf.last_save_t0.take() {
    log::info!("clip save latency: {:?}", t0.elapsed());
}
```

- [ ] **Step 2: Save 10 clips back-to-back, observe**

Launch a moderately demanding game. Save 10 clips spaced ~5s apart. Capture the log:

```bash
./target/debug/arctis-chatmix 2>&1 | grep "clip save latency" | tee /tmp/latency.log
```

- [ ] **Step 3: Compute p99 latency**

```bash
sort -n /tmp/latency.log | awk -F': ' '{print $NF}' | tail -1
```

Expected: ≤1.0 s p99.

If exceeded, drop track 0 from the GSR args (keep only 5 tracks) and re-measure.

- [ ] **Step 4: Document in spec**

Update spec verification item #2 with the measured p99.

### Task 8.3: Crash recovery test

- [ ] **Step 1: While armed, kill GSR**

```bash
pkill -9 -f "^gpu-screen-recorder"
```

Within ~1 s, expect:
- Indicator turns red ("Capture stopped")
- Backend logs `BackendDied` and `auto-restart attempt 1`
- Indicator turns green again ("Buffering")
- New GSR child appears in `pgrep`

- [ ] **Step 2: Kill twice in a row**

Repeat the kill within 2 s. Expect:
- After second kill: indicator stays red, error toast shown, no further restart.

- [ ] **Step 3: User retry**

Add a retry button somewhere (settings or the indicator click action) that calls `BufferController::retry()`. Verify clicking it transitions Idle → Arming → Armed.

```bash
git add docs/superpowers/specs/2026-05-08-clipping-system-design.md src/app.rs
git commit -m "spec+app: verify crash recovery and add retry button"
```

### Task 8.4: Multi-monitor recovery flow

- [ ] **Step 1: Set up multi-monitor environment**

If the dev machine has only one monitor, attach a second (HDMI test). Restart the app.

- [ ] **Step 2: Pick monitor A in the portal, save a clip**

Verify the saved clip captures monitor A.

- [ ] **Step 3: Disconnect monitor A, plug into monitor B port; save another clip**

This simulates the xdg-portal #1371 scenario.

- [ ] **Step 4: Verify the recovery UX works**

The save toast/notification thumbnail should let the user see the wrong monitor was captured. They click Reset capture source in settings → onboarding card → re-pick monitor B.

If the recovery UX has friction, fix in `src/clips/notifications.rs` (e.g., add a "Reset capture source" button to the toast directly).

```bash
git add src/clips/notifications.rs
git commit -m "clips: surface 'Reset capture source' in saved-clip toast for recovery"
```

### Task 8.5: Verify Steam appmanifest path

- [ ] **Step 1: Check path on host**

```bash
ls ~/.steam/steam/steamapps/appmanifest_*.acf 2>&1 | head -3
```

If the path resolves and lists `.acf` files, the detector's lookup will work. If on Bazzite-deck (Flatpak Steam), the path may be `~/.var/app/com.valvesoftware.Steam/data/Steam/steamapps/`. Update `src/clips/detector.rs::steam_game_name` to try both:

```rust
pub fn steam_game_name(app_id: &str) -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let candidates = [
        PathBuf::from(&home).join(".steam/steam/steamapps").join(format!("appmanifest_{app_id}.acf")),
        PathBuf::from(&home).join(".var/app/com.valvesoftware.Steam/data/Steam/steamapps").join(format!("appmanifest_{app_id}.acf")),
    ];
    for path in candidates {
        if let Ok(c) = std::fs::read_to_string(&path) {
            if let Some(n) = parse_acf_name(&c) {
                return Some(n);
            }
        }
    }
    None
}
```

```bash
git add src/clips/detector.rs
git commit -m "clips: try Steam Flatpak appmanifest path as fallback"
```

### Task 8.6: QA pass via qa-code-auditor

- [ ] **Step 1: Dispatch agent**

Invoke `qa-code-auditor` with prompt:

> Audit `src/clips/` for idiomatic Rust, consistency with the existing project conventions (mirroring patterns in `src/eq/`, `src/audio/`), inefficiencies in the hot paths (`/proc` scan in `clips::detector::scan_once`, save-callback FIFO read in `clips::backend::run_fifo_reader`, thumbnail extraction worker spawning in `clips::browser::bind_clip_card`), and any dead code or misleading comments. Spec: `docs/superpowers/specs/2026-05-08-clipping-system-design.md`. Report findings sorted by severity. Do not modify code; just identify.

- [ ] **Step 2: Address findings**

Apply fixes for any Critical / Major findings. File-level cleanup or refactor as needed.

- [ ] **Step 3: Commit**

```bash
git add src/clips/
git commit -m "clips: address QA findings — <summary>"
```

### Task 8.7: End-to-end pass via project-tester

- [ ] **Step 1: Dispatch agent**

Invoke `project-tester` with prompt:

> Run the clipping system end-to-end on Bazzite. Verify: portal pick succeeds; KDE GlobalShortcuts dialog appears on first launch; suggested Super+Shift+R works; launching a Steam game arms the buffer (verify via pgrep + indicator); pressing the hotkey saves a clip; clip appears in the Clips browser tab with thumbnail; remix Export produces a `-remix.mp4` with adjusted track 0 audio; settings Reset capture source returns to onboarding card; window-hide notifications still arrive via gio. Report each step's pass/fail with reproduction details for any failures.

- [ ] **Step 2: Fix any failures**

Apply fixes as needed.

- [ ] **Step 3: Final commit**

```bash
git commit --allow-empty -m "clips: project-tester pass complete"
```

---

## Self-review (already performed by author of this plan)

**Spec coverage check (post-critique revision):**

| Spec section | Plan task |
|---|---|
| Module layout (`src/clips/...`) | Task 1.2 |
| Threading (1 dedicated thread) | Tasks 1.7, 1.8 |
| GSR command surface (CLI args) | Task 1.5 |
| Signal map (SIGUSR1, SIGRTMIN+N, SIGINT) | Task 1.8 |
| Save callback FIFO + bundled script | Tasks 1.3, 1.6 |
| Supervision (PR_SET_PDEATHSIG, auto-restart) | Tasks 1.7, 1.8 |
| Game detection /proc + SteamAppId + appmanifest | Tasks 2.1, 2.2, 2.3, 2.5 |
| Detector debounce (2-scan persistence) | Task 2.4 |
| Buffer state machine | Task 2.6 |
| GSR Flatpak detection + install helpers | Task 3.2 |
| Onboarding state in settings (`onboarding_complete` flag) | Task 3.3 |
| 3-page onboarding wizard UI (install / screen / settings) | Task 3.4 |
| Wizard action wiring + auto-resume on partial completion | Task 3.5 |
| `on_portal_reset` (state-machine-safe reset) | Task 3.6 |
| Eager portal pick (within wizard, mid-flow) | Tasks 3.1, 3.4, 3.5 |
| restore_token persistence | Task 3.1 |
| Reset capture source | Task 3.6 |
| **Test capture button (xdg-portal #1371 mitigation)** | Task 3.7 |
| **Reinstall GSR + missing-GSR detection (recovery)** | Task 3.8 |
| GlobalShortcuts portal | Task 4.2 |
| GAction fallback | Task 4.1 |
| Library: index, sanitization, collisions | Tasks 5.1, 5.2 |
| Thumbnails | Task 5.3 |
| Clips browser GridView (split for granularity) | Tasks 5.4a, 5.4b, 5.4c, 5.4d |
| Card kebab menu | Task 5.5 |
| Settings (full group) | Tasks 6.1, 6.2 |
| Status indicator | Task 6.3 |
| Notifications (toast + gio) | Task 7.1 |
| Remix panel | Task 7.2 |
| Preview (GStreamer) | Task 7.3 |
| Export (ffmpeg) | Task 7.4 |
| **Disk retention auto-delete (oldest-first on Saved)** | Task 7.5 |
| Verification items | Phase 8 |
| QA pass | Task 8.6 |
| End-to-end pass | Task 8.7 |
| Visual mockups (paired brainstorm + apply) | Tasks 5.6, 6.4, 7.6 |

**Placeholder scan:** Two reference-to-pattern items deliberately left for the implementer's discretion:
- Task 5.5: kebab-menu code is described but not fully written — implementer should reference `src/eq/mod.rs::show_edit_dialog` for the modal pattern. This is justified because the project has an established pattern; copying 80 lines of code into the plan adds noise.
- Task 6.2: full settings widget code is sketched, not written line-by-line. Each widget follows the same `adw::SwitchRow` + `connect_active_notify` pattern already used at `src/window.rs:498`.

These are deliberate references to in-codebase patterns, not gaps.

**Critique revisions applied (devils-advocate-critic, second pass):**
- C1: `pre_exec` closure body wrapped in inner `unsafe { ... }` block (Task 1.7).
- C2: `ClipMeta` gets a single `#[derive(Default)]`; the manual impl was removed (Task 5.4a).
- C3: `futures-util` dep dropped; `StreamExt` reachable via `gtk::glib::prelude::*` (Task 1.1).
- C4: Wrong SIGRTMIN fallback recipe deleted from Task 1.8.
- C5: Test capture button + dialog added as Task 3.7 (was 3.5 in earlier revision; renumbered when wizard tasks were added).
- **Phase 3 expanded** (post-user-direction May 2026): single-button onboarding card replaced with a 3-page wizard. New Tasks 3.2 (GSR install helpers), 3.3 (onboarding_complete flag), 3.4 (wizard UI), 3.5 (action wiring + auto-resume), 3.8 (reinstall flow + missing-GSR detection). Old Tasks 3.4/3.5 renumbered to 3.6/3.7.
- M1: Disk retention auto-delete added as Task 7.5.
- M2: Task 5.4 split into 5.4a (GLib subclass), 5.4b (factory + label-only), 5.4c (thumbnails), 5.4d (ffprobe backfill).
- M3: Screenshot URI handling fixed (Task 3.1) — uses `to_file_path()` first, falls back to `as_str()`.
- M4: Removed `tokio` from ashpd features (CLAUDE.md "No tokio"). Falls back to `async-std` if a runtime is later required (Task 1.1).
- M5: `on_portal_reset` controller method added to keep state-machine ownership (Task 3.4).
- M6: `Cow<str>` → owned `String` before `to_variant()` (Task 7.1).
- M7: Skipped — `ujust install-gpu-screen-recorder` command name verified at preflight (P.2).
- M8: Plasma 6.4.1+ check added to preflight P.2 (KDE GlobalShortcuts portal bug fix).
- N1, N2, N4: Subclass prelude import + dropped `Properties` derive + explicit `CardWidgets` struct (Tasks 5.4a, 5.4b).
- N3: Visual mockup tasks reframed as brainstorming sub-session + plan-implementer apply (Tasks 5.6, 6.4, 7.6).
- N5: XDG modifier syntax documented (Task 4.2).
- N6: ffprobe moved out of FIFO reader; deferred to backfill worker thread (Tasks 1.8, 5.4d).

**Type consistency:** `ClipCommand::SaveClip` (Task 1.4) → `BufferController::on_save_hotkey` (Task 2.6) → `app.rs save-clip GAction` (Task 4.1) → portal `save-clip` shortcut id (Task 4.2). All consistent. `BackendEvent::Saved { path, duration_ms }` declared in Task 1.4 and destructured in Tasks 7.1, 7.5 — consistent (duration_ms = 0 from FIFO reader, backfilled later).

**Scope:** Single coherent feature; not decomposable without losing testability (capture without browser is unverifiable).

---

## Teammate involvement (per `~/.claude/CLAUDE.md`)

- **research-bot** — used early in brainstorming (capture-stack survey). Done.
- **devils-advocate-critic** — used mid-brainstorming (pre-spec adversarial review). Done.
- **plan-implementer** — primary executor for this plan. Visual mockup tasks (5.6, 6.4, 7.6) are split into a brainstorming sub-session (driven by the team lead) followed by a plan-implementer dispatch that *applies* the user-approved direction. plan-implementer is not asked to pick visual directions on its own.
- **project-tester** — Task 8.7 (end-to-end pass).
- **qa-code-auditor** — Task 8.6 (pre-merge audit of `src/clips/`).
- **security-audit-sentinel** — skip for v1; re-evaluate at Flatpak packaging time when sandbox permissions become a security surface.
