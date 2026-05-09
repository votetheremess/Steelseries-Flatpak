# Clipping system — design

**Date:** 2026-05-08
**Topic:** Replay-buffer clip capture, hotkey-save, in-app browser
**Status:** Approved design, pre-implementation
**Target platform:** Bazzite (KDE Plasma 6.x), Flatpak distribution as eventual packaging

## Context

The existing Clips sidebar tab is a placeholder (`src/window.rs:238` — sidebar button currently `set_sensitive(false)`, page is `build_placeholder_page("Clips", ..., "Coming soon")`). This spec replaces the placeholder with a working SteelSeries-Moments-style clip capture feature: continuously buffer recent gameplay video+audio, hotkey to save the last N seconds, browse and manage saved clips in the existing sidebar tab.

The implementation drives `gpu-screen-recorder` (GSR) as a child process — the de-facto Linux replay-buffer recorder, with first-class hardware encode (NVENC / VAAPI / Vulkan video) and PipeWire ScreenCast portal support. Audio multi-track is GSR's first-class behavior (multiple `-a` flags = separate AAC tracks in MP4). Hotkeys come from `org.freedesktop.portal.GlobalShortcuts` via `ashpd`. Game detection is a process scan (gamemoded is intentionally absent on Bazzite). The Clips tab presents saved clips as a grid of thumbnails with per-track remix capability.

## Goals

- **Always-ready capture** while a game is detected; hotkey-save the last N seconds with sub-1-second perceived latency.
- **Per-source audio tracks** — game / chat / music / aux / mic preserved separately for post-edit remixing. This is a Moments-beating feature; SteelSeries doesn't offer it.
- **First-class in-app library** — grid browser with thumbnails, rename, delete, open-in-folder, in-app remix.
- **Idiomatic to the existing codebase** — same `std::thread` + `mpsc` + `glib::timeout_add_local` patterns used by HID, EQ pipeline, tray.
- **Bazzite-first, Flatpak-ready** — direct host-binary calls today; Flatpak port uses `flatpak-spawn --host gpu-screen-recorder ...` with no other architectural change.

## Non-goals (v1)

- HDR clip capture. ScreenCast portal converts HDR→SDR; native HDR pass-through requires compositor changes that aren't shipped on KDE 6.x.
- In-app trim / cut. Open the file in an external editor. (Remix the audio mix is in scope; trim isn't.)
- Cloud upload, sharing integrations.
- X11 support. Wayland-only.
- Distros other than Bazzite. The design works on any KDE 6 + PipeWire + GSR system, but only Bazzite is targeted for v1.

## Architecture

### Module layout

New flat directory `src/clips/` parallel to `src/eq/`, `src/audio/`, `src/hid/`:

```
src/clips/
├── mod.rs        — build_clips_page(), public types, mpsc wiring, status badge widget
├── backend.rs    — GsrBackend: drives gpu-screen-recorder child process
├── detector.rs   — GameDetector: /proc scan + SteamAppId lookup
├── buffer.rs     — BufferController: state machine
├── hotkey.rs     — GlobalShortcuts portal binding + GAction wiring
├── library.rs    — clip metadata index, file scanning
├── thumbnail.rs  — async thumbnail extraction via ffmpeg child
├── browser.rs    — Clips tab grid UI
└── settings.rs   — clip-settings widgets, called from window::build_settings_page
```

No `trait CaptureBackend` abstraction. Driving GSR via signal IPC and a future GStreamer-based implementation are different enough to make a shared trait either too thin to justify or too generic to be useful. When/if the GStreamer port happens, it replaces `backend.rs` as a port, not a swap.

### Threading model

The existing app has four threads (GTK main, HID listener, tray, EQ pipeline) communicating via `std::sync::mpsc` channels polled by `glib::timeout_add_local` on the GTK main thread. This feature adds **one new dedicated thread** plus one boring stdout-reader thread:

| Thread | Owner | Role |
|---|---|---|
| `clip-backend` | `backend.rs` | Owns the long-lived `gpu-screen-recorder` child process. Reads its stdout for save-callback notifications. Receives commands via mpsc. Mirrors the existing `eq-pipeline` thread pattern. |
| `gdbus-monitor-reader` (boring) | `detector.rs` (only if gamemoded happens to be present) | Drains stdout of `gdbus monitor --session --dest org.freedesktop.GameMode`. Mirrors the existing `pw-cli-reader` thread. Optional — Bazzite users won't hit this path. |

Game detection's primary signal is a `glib::timeout_add_local` 2-second `/proc` scan on the GTK main thread (mirrors the existing `restore_new_streams` 2-second new-stream watcher pattern). `ashpd` portal calls run on the GTK main thread via `glib::MainContext::default().spawn_local()` using `ashpd = { version = "0.10", features = ["gtk4"] }`. No new async runtime.

### Channels

All cross-thread messaging is `std::sync::mpsc`, polled by glib timers on the GTK main thread.

```
GTK main ── ClipCommand ──> clip-backend
GTK main <── BackendEvent ── clip-backend     (poll 100ms)
GTK main <── DetectorEvent ── detector tick   (in-thread; no channel needed)
GTK main <── HotkeyEvent ── ashpd spawn_local (in-thread; no channel needed)
```

`ClipCommand` variants: `StartReplay { config }`, `StopReplay`, `SaveClip { duration }`, `Reconfigure { config }`, `Shutdown`.
`BackendEvent` variants: `Armed`, `Disarmed`, `Saved { path, duration_ms }`, `BackendDied { reason }`, `Error { context, message }`.

## Capture backend

### GSR child process

Launched once per arming session. Single process per active capture. Killed and respawned only on `Reconfigure` (e.g. user changes buffer length or capture target). Uses `prctl(PR_SET_PDEATHSIG, SIGTERM)` so the child dies if our app dies.

CLI invocation skeleton (concrete flags resolved at runtime from settings + portal pick):

```
gpu-screen-recorder \
  -w portal \
  -restore-portal-session yes \
  -r <buffer_seconds> \
  -bm cbr \
  -q <bitrate_mbps> \
  -k h264 \
  -ac aac \
  -f 60 \
  -o <output_directory> \
  -sc <save_callback_script> \
  -restart-replay-on-save yes \
  -a "device:<sink_mix>.monitor" \
  -a "device:SteelSeries_Game.monitor" \
  -a "device:SteelSeries_Chat.monitor" \
  -a "device:SteelSeries_Music.monitor" \
  -a "device:SteelSeries_Aux.monitor" \
  -a "device:SteelSeries_Mic.monitor"
```

`-bm cbr` is mandatory — GSR's default `auto → qp` (constant quality) makes RAM usage unbounded under high-motion scenes. CBR keeps the in-RAM ring buffer at a predictable `buffer_seconds × bitrate_mbps / 8` MB.

### Control plane (signals)

Per the GSR upstream documentation, replay-mode signals are:

| Signal | Effect |
|---|---|
| `SIGUSR1` | Save replay (the buffer's full content, then continue) |
| `SIGRTMIN+1..6` | Save replay with a specific shorter duration (30s / 60s / 120s / 300s / 600s / 1800s respectively) |
| `SIGINT` | Stop without saving (replay mode) |

We use `SIGUSR1` for "save the configured length" and optionally surface `SIGRTMIN+1..3` as user-bindable secondary hotkeys ("Save 30s", "Save 60s", "Save 120s") — multi-duration saves with no buffer-length fight.

Pause/resume is **not** supported in replay mode (`SIGUSR2` is documented as "not for streaming/replay"). The buffer arms when a game is detected and disarms when it stops. To force-disarm, send `SIGINT` and join the child.

### Save callback

`-sc <path>` invokes a script after the replay file is muxed and on disk. We pass a tiny shell wrapper that writes `$1` (the saved file path) to a FIFO our backend thread reads:

```sh
#!/bin/sh
echo "$1" > "$ARCTIS_CHATMIX_SAVE_FIFO"
```

The wrapper is extracted from `include_bytes!`-bundled content into `~/.cache/arctis-chatmix/clips/save_callback.sh` on first arm (idempotent, mirroring how Lucide icons and HRIR WAVs are bundled). The FIFO lives at `~/.cache/arctis-chatmix/clips/save.fifo`. The backend thread reads lines from the FIFO and emits `BackendEvent::Saved { path }` for each.

### Supervision

The backend thread holds the `Child` handle and runs a non-blocking `try_wait()` after every command-channel poll. On unexpected exit (non-zero status + we didn't request shutdown), emit `BackendEvent::BackendDied { reason }` and attempt one auto-restart with the same config. Second consecutive death surfaces a persistent error toast with a "Retry" button. The status indicator turns red.

## Game detection

Primary signal is a 2-second `/proc` scan on the GTK main thread. For each PID, read `/proc/<pid>/comm`, `/proc/<pid>/cmdline`, and `/proc/<pid>/environ`. Matchers:

| Pattern | Action | Game name source |
|---|---|---|
| cmdline contains `reaper SteamLaunch AppId=<id>` | Steam game | Look up in `~/.steam/steam/steamapps/appmanifest_<id>.acf` (`"name" "..."` field). Fall back to `comm`. |
| `environ` contains `SteamAppId=<id>` | Steam game (alternate launcher path) | Same lookup as above. |
| cmdline contains `lutris-wrapper` | Lutris game | Parse the wrapped game name from cmdline arg position. |
| `comm` is `heroic` (within a child process tree spawned for play) | Heroic game | Walk to leaf of the gamescope/wine-server tree, use leaf `comm`. |
| `comm` is `gamescope` | Generic gamescope session | Use the longest-running child's `comm` as a fallback name. |
| `comm` is `mangohud` | Game with MangoHud overlay | Walk to leaf, use leaf `comm`. |

`gamemoded` D-Bus is **opportunistic, not required**. If the daemon happens to be running (some users install it manually despite Bazzite's recommendation), subscribe to `RegisterGame` / `UnregisterGame` via `gdbus monitor` shell-out — its game-name payload is more reliable than the scan when available. Do not depend on it.

A debounce: a detected game must persist across two consecutive scans before arming (4 s minimum), and must be absent across two scans before disarming. This prevents flap on game-launcher-spawning-game-spawning-game patterns.

Settings provides:
- **Auto-arm** toggle (default: on) — disables detection entirely; user manually arms via the Clips tab.
- **Always armed** toggle (default: off) — overrides detection; buffer runs whenever the app is up.

## Buffer state machine

```
       ┌────────────────────────────────────────────┐
       │                                            │
       ▼                                            │
   Uninitialized                                    │
       │                                            │
       │ portal pick complete                       │
       ▼                                            │
     Idle ◄─────────────── disarm ─────────────┐    │
       │                                       │    │
       │ game detected                         │    │
       ▼                                       │    │
    Arming ─── GSR launch fail ───► ErrorState─┘    │
       │                                            │
       │ GSR ready                                  │
       ▼                                            │
    Armed ◄──┐                                      │
       │     │                                      │
       │     └── SIGUSR1 result ◄─── Saving ◄───────┤
       │                                            │
       │ save hotkey pressed                        │
       ▼                                            │
    Saving                                          │
       │                                            │
       │ unrecoverable backend death                │
       ▼                                            │
   ErrorState ─── user retry ──────────────────────┘
```

`Uninitialized` is the pre-portal-pick state. Transition to `Idle` only after a successful ScreenCast portal pick is persisted.

## Capture target — portal pick flow

**Eager pick at first launch, not lazy.** Portal dialogs in the middle of a fullscreen game are an unacceptable UX hazard.

On every app start, check for the persisted ScreenCast `restore_token` in `~/.config/arctis-chatmix/clips_portal.txt`. If absent:

- The Clips tab shows an onboarding card: "**Set up clip capture** — Pick the screen you want to record. You can change this later in Settings." with a single button.
- Until pick is complete, auto-arm is suppressed and the indicator shows "Set up needed".
- Game detection still runs in the background but doesn't trigger arming.

If present:

- Validate the token by attempting a session restore. On failure (token expired, monitor unplugged), drop back to onboarding state.
- On success, transition to `Idle`.

Settings has:
- **Reset capture source** button — invalidates the token, returns to onboarding.
- **Test capture** button — captures a single frame from the current target via `org.freedesktop.portal.Screenshot` (which shares the user's persisted source consent with the ScreenCast portal session), displays it in a dialog. Lets the user verify their saved choice without recording a clip.

### Multi-monitor recovery (xdg-desktop-portal #1371)

A documented upstream bug means `PersistMode::Persistent` may silently restore the *wrong* monitor on multi-monitor systems. Mitigations:

- **Every save toast/notification embeds a thumbnail** of frame 1 from the saved clip. The user sees at a glance which screen got captured.
- **Reset capture source** is one click away in Settings.
- **Test capture** is the proactive verification path.

## Audio plumbing

GSR's multi-track behavior is verified: multiple `-a` flags produce separate AAC tracks in the MP4 container. The output structure:

| Track | Source |
|---|---|
| 0 | Headset physical sink monitor (the post-mix output that the user actually hears) |
| 1 | `SteelSeries_Game.monitor` |
| 2 | `SteelSeries_Chat.monitor` |
| 3 | `SteelSeries_Music.monitor` |
| 4 | `SteelSeries_Aux.monitor` |
| 5 | `SteelSeries_Mic.monitor` |

Track 0 ensures Discord embed / casual playback works without thinking. Tracks 1–5 enable the in-app remix panel and external editor workflows.

Per-source mic capture (track 5) follows the same headset-mic source the existing `src/audio/router.rs` manages — when the user has switched to a different physical mic via the mixer dropdown, that's what gets captured.

The headset physical sink monitor (track 0) is discovered at clip-arm time via `pactl list sources short | grep ".monitor" | grep -v SteelSeries` and selecting the entry whose name matches the headset sink registered in `AppResources::headset_sink`.

### Latency target

p99 hotkey-to-toast latency: **1.0 second** for a 60s 1080p60 H.264 + 6×AAC clip. Benchmark on Bazzite reference hardware before locking the multi-track default. If the budget is exceeded, the fallback is dropping track 0 (the mix-down) and exposing only the 5 isolated tracks — the remix panel can synthesize a track-0-equivalent on export.

## Hotkey + IPC

Two paths to the save action, both ending in the same code:

1. **GlobalShortcuts portal** via `ashpd` 0.10 `gtk4` feature. Suggested binding `Super+Shift+R`. On first launch, ashpd's `BindShortcuts` triggers the KDE picker dialog where the user confirms or rebinds. Subsequent launches are silent. Settings provides a "Rebind clip hotkey" button that re-invokes `BindShortcuts`.

2. **GAction-based DE keybind** for users with broken portals or who prefer DE-native config. Register `app.add_action("save-clip", ...)` on the existing `adw::Application`. `GApplication` auto-exposes actions via `org.gtk.Actions` on the application's well-known D-Bus name (`com.github.arctis_chatmix.ArctisNovaEliteChatMix`). Users can then bind any DE keyboard shortcut to:

   ```
   dbus-send --session \
     --dest=com.github.arctis_chatmix.ArctisNovaEliteChatMix \
     /com/github/arctis_chatmix/ArctisNovaEliteChatMix \
     org.gtk.Actions.Activate string:save-clip array:string: array:string:
   ```

   Settings shows this exact command in a copyable text field.

Both paths emit `BackendEvent::SaveClip` (or call `buffer.save_now()` directly), routed identically.

## Storage

### Layout

```
~/Videos/Clips/                                    (default, configurable)
├── 2026-05-08-1934-Apex-Legends.mp4
├── 2026-05-08-2102-Counter-Strike-2.mp4
├── 2026-05-09-0145-Untitled.mp4
└── .cache/
    └── thumbs/
        ├── 2026-05-08-1934-Apex-Legends.jpg
        └── ...
```

Thumbnails are 320×180 JPEGs extracted via `ffmpeg -ss 1.0 -i <clip> -vframes 1 -q:v 4 <thumb.jpg>` (offset 1s in case of black-frame intro). Generation is async on a worker thread, missing thumbnails show a generic `lucide-clapperboard-symbolic` placeholder.

### Index

`~/.config/arctis-chatmix/clips_index.txt` — tab-separated lines: `<filename>\t<duration_ms>\t<game_name>\t<created_unix>\t<bitrate_kbps>\t<resolution>`. Treated as a cache, not authoritative — the canonical source of truth is the directory contents. On Clips-tab open, scan the directory and reconcile with the index (add new entries for unindexed files via ffprobe; remove entries for files that no longer exist).

### Filename

`<YYYY-MM-DD>-<HHMM>-<game-name-or-Untitled>.mp4`. Game name is sanitized: spaces → hyphens, non-alphanumeric stripped, max 40 chars. Collisions append `-2`, `-3`, etc.

### Disk management

Settings provides:
- **Clip retention** dropdown: "Keep all" (default) / "Cap at 10 GB" / "Cap at 30 GB" / "Cap at 50 GB" / "Cap at 100 GB".
- **Show disk usage** card on the Clips tab — running total, with the cap if set.

When over the cap, oldest clips are auto-deleted on next clip save (not on a periodic timer — keeps the I/O bursty and visible).

## UI components

Visual mockups (browser layout, settings page sections, status indicator placement, remix panel) are deferred to the implementation phase and built via the brainstorming visual companion by `plan-implementer`. The spec describes behavior; the mockup pass nails layout.

### Clips tab (`browser.rs`)

Replaces the existing placeholder. Layout from top to bottom:

1. **Header row.** "Clips" title, right-aligned controls: "Show in Folder" button, search/filter Entry, sort dropdown (Newest / Oldest / Largest / Game).
2. **Onboarding card** — only when `restore_token` missing (pre-portal-pick). Shown above the grid. Single CTA button: "Set up clip capture".
3. **Disk-usage strip** — small label showing "X clips, Y GB / Z GB".
4. **Grid of clip cards.** `gtk::GridView` with `gtk::ListItemFactory`. Each card: thumbnail (320×180), title (game + timestamp), duration badge, hover reveals a kebab menu (Rename / Delete / Open in Folder / Remix). Click anywhere else on the card opens the Remix panel inline (slides in from the right or pushes a new Stack page — exact transition decided at mockup time).
5. **Empty state** — when no clips: a centered StatusPage with "No clips yet" text, the suggested hotkey shown prominently, and a faded preview of what the grid will look like.

### Settings additions (`settings.rs`)

A new `adw::PreferencesGroup` titled "Clips" inserted into the existing `build_settings_page` (`src/window.rs:498`), between the autostart group and the quit row:

- **Capture source** — current target shown as label, "Reset" and "Test capture" buttons.
- **Buffer length** — `gtk::Scale` 30–300 s, default 60.
- **Bitrate** — dropdown (15 / 25 / 40 / 60 Mbps) keyed to the user's display resolution.
- **Hotkey** — current portal binding shown as label, "Rebind" button, separator, GAction copyable command shown as monospace label with a Copy button.
- **Auto-arm** — switch.
- **Always armed** — switch (mutually exclusive with Auto-arm: enabling it greys out Auto-arm).
- **Audio tracks** — switch "Per-source tracks" (default on). When off, only track 0 (mix-down) is captured.
- **Mic capture** — switch (default on). Independent of per-source toggle.
- **Storage location** — `gtk::FileChooserButton` (default `~/Videos/Clips`).
- **Disk retention** — dropdown.

### Status indicator

A small badge widget added to the existing dashboard Status card (the same card that holds battery + ChatMix in `src/window.rs::build_dashboard_page`). Three states:

| State | Visual | Tooltip |
|---|---|---|
| Set up needed | Dim badge with "Set up Clips" link | Click to open Clips tab onboarding |
| Idle | Hidden | — |
| Armed (game detected) | Pulsing green dot + "Buffering — `<game>`" | Last N seconds available; press hotkey to save |
| Saving | Yellow dot + "Saving…" | Brief, ~1 s |
| Error | Red dot + "Capture stopped" | Click to see error toast |

### Remix panel

In-app post-edit view shown when a clip card is opened. Per-track:

- Track label (Game / Chat / Music / Aux / Mic / Mix)
- Volume slider (-∞ to +6 dB, default 0 dB)
- Mute / Solo buttons
- Mini waveform preview (post-implementation polish — `gtk::DrawingArea` with downsampled peaks)

Bottom action bar: **Preview** (plays the clip with the current mix via a temporary GStreamer pipeline that applies per-track volume and feeds an audio sink) / **Export** (renders a new MP4 with track 0 = remixed mix-down + tracks 1-5 unchanged). Export uses ffmpeg `-filter_complex` with per-input `volume=N` filters and `amix` to produce the new track 0; tracks 1-5 copied via `-c:a copy`. The exported file gets a `-remix` suffix to keep the original.

## Notifications policy

Two paths depending on window visibility:

| Window state | Notification mechanism |
|---|---|
| Visible | `adw::Toast` in the main window (libadwaita native) |
| Hidden (window-close-hides) | `org.freedesktop.Notifications` via `gio::Notification` — desktop-native popup that doesn't depend on a visible app window |

Saves always include a clip thumbnail (the frame extracted by `thumbnail.rs` after save). The notification action button "Show clip" focuses the window and selects the new clip in the browser.

## Configuration & state

New files:

- `~/.config/arctis-chatmix/clips_settings.txt` — line-oriented `key=value`. Settings keys: `buffer_length`, `bitrate_mbps`, `auto_arm`, `always_armed`, `per_source_tracks`, `mic_capture`, `storage_path`, `disk_cap_gb`, `hotkey_binding` (just the suggested string; portal owns the actual binding).
- `~/.config/arctis-chatmix/clips_portal.txt` — single line: the persisted ScreenCast `restore_token`.
- `~/.config/arctis-chatmix/clips_index.txt` — clip metadata index (described above).
- `~/.cache/arctis-chatmix/clips/save_callback.sh` — extracted bundled callback script.
- `~/.cache/arctis-chatmix/clips/save.fifo` — IPC FIFO from callback to backend thread.
- `~/.cache/arctis-chatmix/clips/thumbs/` (mirror of `<storage_path>/.cache/thumbs/` for legacy / migration cases).

## Lifecycle integration

The existing app behaviors interact with clipping:

- **`--hidden` autostart.** App starts hidden, no window. Clips feature still arms/disarms via game detection. First-run portal pick is deferred until the user opens the window — the indicator shows "Set up needed" via tray tooltip if reachable, otherwise the onboarding card waits.
- **Window close → hide.** Buffer keeps running. Notifications switch to `gio::Notification`.
- **Quit (explicit).** `ClipCommand::Shutdown` sent first; backend thread sends `SIGINT` to GSR, joins, exits. Then app quits normally.
- **Single-instance reactivation** (existing `connect_activate` behavior). No interaction needed; clipping state lives in the long-running process.
- **Tray icon.** Tray's existing left-click "Show window" works as-is. Right-click menu gains "Save clip now" entry that triggers the same action as the hotkey.

## Performance budget

Per Bazzite reference hardware (modern AMD or NVIDIA gaming desktop, 16+ GB RAM):

| Metric | Budget | Notes |
|---|---|---|
| GPU encoder utilization (1080p60 H.264) | 8–15% | NVENC / VAAPI hardware path |
| RAM (60s buffer @ 25 Mbps CBR) | ~190 MB | `buffer × bitrate / 8`, plus ~30 MB GSR overhead |
| CPU (audio capture + mux) | 3–8% of one core | 6 AAC encoders run on CPU |
| Hotkey-to-toast p99 latency | ≤1.0 s | Benchmarked before lock |
| Disk write per save (60s @ 25 Mbps) | ~190 MB | Sustained ~190 MB/s for ~1 s |

If the multi-track AAC encode pushes p99 latency above 1.0 s, fall back to dropping track 0 (5-track output, remix synthesizes mix on export).

## Edge cases

| Case | Behavior |
|---|---|
| User saves before buffer is full (game just started, only 20s of footage available) | Save the partial buffer. Notification reads "Saved 20s clip". |
| GSR child crashes silently mid-buffer | Backend thread detects via `try_wait()`, indicator turns red, one auto-restart attempted. Second crash → persistent toast with Retry. |
| Capture target monitor unplugged mid-session | GSR exits with portal-error stderr line. Backend thread emits `BackendDied`. Indicator → red. User must reset capture source. |
| Multi-monitor user got the wrong screen on save (xdg-portal #1371) | Notification thumbnail shows it. User clicks "Reset capture source" in Settings. |
| Disk full during save | GSR exits with EIO. Backend emits error. Toast: "Couldn't save — disk full". |
| User starts game, clipping not yet set up | Indicator shows "Set up needed" linking to Clips tab. Game runs unaffected. |
| Storage path doesn't exist | Auto-create on first save. If creation fails, toast error. |
| Concurrent save hotkeys (two presses within 1s) | Debounce: 2-second cooldown after a save signal sent. Second press shows brief toast "Already saving". |
| App crashes with GSR running | `prctl(PR_SET_PDEATHSIG, SIGTERM)` ensures GSR dies with us. No orphan. |
| User enables Always-armed but no portal pick | Onboarding card still wins. Always-armed is a no-op until pick exists. |

## Open verification items

These need to be confirmed in practice during implementation; the design assumes them but each carries some risk:

1. **GSR multi-track output structure on Bazzite.** Assumed: 6 separate AAC tracks per `-a` flag. Verified per upstream docs but worth a `ffprobe` check on the first real save. If it ships as one mixed track, fall back to a hybrid approach (GSR for video + ffmpeg for audio) before locking.
2. **Hotkey-to-toast p99 latency.** Assumed ≤1.0 s. Benchmark on the user's hardware. If exceeded, drop track 0.
3. **`-restore-portal-session yes` behavior on KDE 6.x.** Assumed: silent restore on subsequent launches. KDE 6.5+ has bug-fixed this path; verify.
4. **xdg-desktop-portal #1371 still open on Bazzite's KDE.** If KDE has fixed it independently of upstream, the recovery UI (thumbnail in toast, Test capture button) becomes a smaller concern.
5. **Steam appmanifest path.** Assumed `~/.steam/steam/steamapps/`. On Bazzite-deck (Flatpak Steam) the path may differ — check before locking.

Each of these should be a brief integration test the implementer runs before claiming the feature done.

## Future Flatpak strategy

When packaging as Flatpak:

- Manifest depends on `org.freedesktop.Sdk` (default).
- Permissions: `--socket=pipewire`, `--device=dri` (VAAPI), `--device=all` (NVIDIA NVENC — narrowed if Bazzite's NVIDIA Flatpak overlay supports it explicitly), `--filesystem=xdg-videos:create`, `--talk-name=org.freedesktop.portal.*`.
- GSR is **not bundled.** The Flatpak calls `flatpak-spawn --host gpu-screen-recorder ...` to use the host binary. Same arguments. Same control plane (signals work across `flatpak-spawn`). This avoids GPL-3.0 derivative-work concerns and lets Bazzite's GSR install be the source of truth.
- The save-callback FIFO needs a path the host script can write to that the sandbox can read. Use `/run/user/<uid>/arctis-chatmix-save.fifo` (host-accessible via `--filesystem=xdg-run`).
- Steam appmanifest reading needs `--filesystem=~/.steam:ro`.

This is a small, well-scoped follow-up — not a rewrite. The architecture above is Flatpak-ready by design.

## License declaration (out-of-band)

The project's own license is currently undeclared (no `LICENSE` file, `Cargo.toml` lacks `license = ...`). Driving GPL-3.0 GSR via signal IPC + reading its stdout is arms-length subprocess interaction and not a derivative work, so v1 development is unaffected. The Flatpak distribution path resolves cleanly because the binaries remain separate (`flatpak-spawn --host`). A license declaration before public distribution is recommended but out of scope for this design.

## Testing strategy

Unit-testable in colocated `#[cfg(test)]` modules (matches project convention):

- `detector.rs` — process-cmdline parsing, SteamAppId extraction, appmanifest parsing, debounce logic. Mock `/proc` reads via a `ProcReader` trait.
- `library.rs` — index serialization round-trip, reconciliation logic (file vs. index drift), filename generation + sanitization + collision handling.
- `buffer.rs` — state machine transitions, debounce timing, supervision logic with a fake `Backend`.

Integration / manual:

- Save callback FIFO round-trip (write from shell, read in test process).
- GSR launch + portal pick + arm + save end-to-end with a real game window.
- Multi-monitor monitor-restore behavior (the xdg-portal #1371 path).
- Crash injection: `kill -9 <gsr-pid>` while armed, verify auto-restart.

## Teammate involvement

Per the global "consider teammates at the end of every plan" instruction:

- **`research-bot`** — **Used.** Ran early in the design phase to survey the Linux capture stack (GSR vs portal+GStreamer vs OBS vs wf-recorder), Wayland hotkey constraints, audio tap patterns. Output informed the backend-choice decision and the audio tap path.

- **`devils-advocate-critic`** — **Used.** Ran mid-design to attack the in-progress decisions before the spec was written. Surfaced four critical findings (gamemoded broken on Bazzite; signal map wrong; lazy portal picker mid-game; multi-monitor portal restore bug) and several material concerns (CBR vs QP default, trait-abstraction fiction, latency target unset, multi-track UX gap, supervision missing, hidden-state notifications). All findings were addressed in this spec.

- **`plan-implementer`** — **Use post-spec.** Will execute the implementation plan that `superpowers:writing-plans` produces from this spec. Visual mockups for the Clips browser, Settings page additions, status indicator placement, and remix panel will be generated by `plan-implementer` using the brainstorming visual companion (browser at the URL the user already has open) before coding the corresponding widgets.

- **`project-tester`** — **Use during implementation.** Each integration item from the "Open verification items" list is a real execution test (multi-track output structure via ffprobe, latency benchmark, multi-monitor recovery flow, GSR crash auto-restart). Project-tester runs these against the real build on the user's host before the feature is marked done.

- **`qa-code-auditor`** — **Use before merge.** Pre-merge QA pass on `src/clips/` for idiomatic Rust, consistency with the existing module conventions (matching the patterns in `src/eq/`, `src/audio/`), and for any inefficiencies in the hot paths (process scan, save callback, thumbnail extraction).

- **`security-audit-sentinel`** — **Skip.** No new auth surface, no secret handling, no public endpoints. The IPC paths (signals, FIFO, D-Bus action) are local-user-only. The portal-mediated screen capture is by design user-consented. Re-evaluate when packaging as Flatpak (sandbox permissions become a security surface).

