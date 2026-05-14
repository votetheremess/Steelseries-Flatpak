# Routing / Volume / Clips fixes — design

**Date:** 2026-05-13
**Topic:** Four bug + UX fixes — app→sink persistence, mic hotplug, virtual-source volume persistence, Clips home-page section relocation/expansion
**Status:** Revised draft (revision 4 — addresses critic round-3 feedback; QA round-3 already passing at 91/100)
**Target branch:** `clipping-system` (worktree `/var/home/admin/Documents/Code/SteelseriesFlatpak-clipping/`)
**Authoritative bug list:** `knownbugs.txt` (issues 1–3) + user request (issue 4)

## Context

Four issues surfaced during live testing on the `clipping-system` branch. Three are audio-routing / persistence bugs reachable on both `main` and `clipping-system`; the fourth is a UI polish + expansion request for the clip status indicator on the home page. Per user direction all four land together on `clipping-system`.

### The four issues, as the user described them

1. **Tidal's sink assignment doesn't persist.** Open Tidal → it lands on `SteelSeries_Chat`. Move it to `SteelSeries_Music` via system sound settings. Close Tidal, reopen Tidal → it goes back to `SteelSeries_Chat`. Repeats forever.
2. **Mic source doesn't auto-switch on hotplug.** App starts while preferred mic (e.g. Alias Pro) is powered off → app falls back to whatever is available. Turn the preferred mic on later → app does not detect or switch. User has to manually re-select from the dropdown every time.
3. **`SteelSeries_Mic` (and likely `_Music` / `_Aux`) volume doesn't persist correctly.** User sets `SteelSeries_Mic` to 100% via system sound settings. Closes the app. Reopens. Volume comes up as something other than 100% — and the value is inconsistent across launches (47%, 83%, etc.).
4. **Clip status indicator placement is wrong + needs more controls.** Today the indicator sits as a third row inside the Status card, crowding it. User wants it relocated to its own full-width section below Status + Device, expanded with: "Save Clip Now" button, recording-duration display, current hotkey hint, and a pause / resume toggle. Plus a terminology pass — user-visible strings should say "recording" rather than "buffering".

### Why these have proven hard

Issues 1 and 3 share a structural root cause: **we only capture user-state at app shutdown.** `persistence::save_assignments` runs in `app.connect_shutdown`. Volume changes are never persisted at all on `clipping-system` today (see "Branch state vs. main" below). When the user changes state externally and then doesn't trigger our shutdown — or when a watched object (a sink-input) disappears before our shutdown — the saved state is stale or absent.

Issue 2 is similar in spirit: the mic source is chosen once during `AudioRouter::create` and never reconsidered. PipeWire's source list is dynamic; our model is static.

Issue 4 is a layout + content change, plus a domain-terminology change, plus a pause/resume mechanism that GSR replay mode does **not** natively support (see "Pause/resume design — what 'pause' means" below).

### Branch state vs. main (important)

The clipping-system branch **does not yet contain** the volume-persistence helpers that exist on `main` (`load_volumes`, `save_volume_entry`, `volumes_path`, the `VOLUMES_FILE` constant, the `init_pipeline` apply-volumes loop, and the `mixer.rs` slider save-on-change handler). The CLAUDE.md description of `volumes.txt` is forward-looking and reflects `main`. Implementer A must therefore **port these helpers from main** before extending them. The spec assumes this porting step is part of Implementer A's scope; see "Implementer A scope" below for the file list.

## Goals

- **Saved app→sink assignments always reflect the user's current routing,** whether the user moved a stream via system sound settings, via this app's future UI, or any other means.
- **The preferred mic source auto-attaches as soon as it appears,** without any manual user action, as long as the user has expressed a preference via the mixer dropdown previously — and tolerates USB re-enumeration and Bluetooth profile switches that change the node name but not the underlying device.
- **Virtual sink/source volumes survive app restarts**, including when the user changed them externally (system sound settings, `pactl`, etc.).
- **Clips section on the home page is its own card,** full-width across the dashboard, with an expanded set of controls; user-visible language is "recording" rather than "buffering" throughout the clip UI; pause behavior is honest about what it costs the user.

## Non-goals

- Reusing existing virtual sinks across launches instead of destroy/recreate. Current architecture deliberately destroys and recreates for clean state on startup. Issue 3 is solved by saving the right values, not by avoiding the recreate.
- Game and Chat volume persistence. These remain HID-dial-owned and out of scope for the volume save logic.
- Master (physical headset) volume persistence. WirePlumber owns this natively; we do not duplicate.
- Per-app routing through some new GUI surface. Routing remains exclusively system-sound-settings driven; we just track it more reliably.
- Renaming internal type names (`BufferController`, `buffer.rs`, etc.). Only user-visible strings change for the "recording" terminology pass.
- Auto-detect / auto-switch any non-preferred mic. Only the user's explicit saved preference triggers a switch on hotplug.
- "Resumable" pause that preserves an in-progress replay buffer across pause/resume. GSR's replay mode has no pause primitive (SIGUSR2 pause only applies to direct-record mode, per the GSR docs). Our pause stops the recorder, drops the buffer, and resumes from zero. UI copy must communicate this honestly.

## Score-gate rubric (calibration)

To make the user's "fix and re-review until ≥ 90%" workflow well-defined, the QA + critic reviewers score each phase against four dimensions, each weighted 25:

- **Correctness** — does the design / plan / implementation actually solve the user-reported bugs? Logic gaps, edge cases, race conditions count against.
- **Idiomatic fit** — does it follow the existing patterns in CLAUDE.md and the codebase (mpsc + glib::timeout_add_seconds_local, single-entry merge writes, persistent pw-cli session, etc.)?
- **Completeness** — error handling, edge cases, testing recipes, file impact, threading, terminology consistency.
- **Implementation guidance** — would two parallel implementers given this spec produce mergeable code without re-deriving design decisions?

A 90/100 means each dimension is at least ~22/25. Mixed scores (e.g. QA 91 / critic 88) trigger another revision pass focused on whichever dimension(s) the lower-scoring reviewer flagged. The next pass re-scores from scratch — there is no carry-over.

## Architecture

### Issues 1–3: one unified state-sync tick

Three of the four issues are different facets of the same structural gap: we don't observe-and-persist state continuously, only at startup and shutdown. The fix is to **extend the existing 2-second sink-input watcher in `app.rs`** (today it only calls `persistence::restore_new_streams`) into a broader "state sync tick" that does four things per cycle:

1. **Auto-route new streams** — existing behavior, retained: any new sink-input gets routed to its saved sink.
2. **Detect user-initiated stream moves** (Issue 1) — track `HashMap<u32, String>` mapping sink-input-id → last-known sink-name. If a stream's sink changes between polls and the new sink is one of ours, update the saved assignment. **Do not** drop the saved entry on a move *off* a managed sink (see "Decision: only persist moves *onto* managed sinks" below).
3. **Mic hotplug check** (Issue 2) — see "Issue 2 detailed design" for the stable-identifier match logic. Cached in the `AudioRouter`; no per-tick disk read.
4. **Virtual sink/source volume capture** (Issue 3) — query `get_sink_volume` / `get_source_volume` for `SteelSeries_Music`, `SteelSeries_Aux`, `SteelSeries_Mic`. If the value differs from what's in the in-memory `last_saved_volumes` cache, write through `persistence::save_volume_entry` and update the cache.

All four pieces share one `glib::timeout_add_seconds_local(2, ...)` callback (matching the existing `STREAM_WATCH_SECS = 2` constant in `app.rs`). No new threads, no new MPSC channels, no new IPC. The state-tracking maps live as `Rc<RefCell<...>>` captured by the closure.

#### Factoring: extract the tick body into a small module

To minimize `app.rs` co-editing between the two parallel implementers, the tick body lives in a new module — `src/audio/state_sync.rs` — exporting a single orchestrator function:

A `StateSyncState` struct holds the per-tick state. Implementer A may shape it as one struct or as separate maps — either is acceptable; the struct form is preferred for closure-capture cleanliness:

```rust
pub struct StateSyncState {
    pub tracked: HashMap<u32, String>,
    pub self_move_pre_sink: HashMap<u32, (String, Instant)>,
    pub last_saved_volumes: HashMap<String, u32>,
}

pub fn tick(state: &mut StateSyncState, router: &mut AudioRouter);
```

Internally `state_sync::tick` calls:
- `persistence::reconcile_stream_state(&mut state.tracked, &mut state.self_move_pre_sink)` — Issue 1. `reconcile_stream_state` lives in `persistence.rs` to keep `list_sink_inputs`/`list_sinks_by_id` private (`pub(crate)` not needed). Internal helper to that module.
- `router.check_mic_hotplug()` — Issue 2 (router calls its own internal helper using its cached `preferred_mic`).
- `state_sync::capture_virtual_volumes(&mut state.last_saved_volumes)` — Issue 3 (private function in this module).

`app.rs`'s `connect_activate` closure registers the timer with a one-liner closure that captures one `Rc<RefCell<StateSyncState>>` and the `Rc<RefCell<Option<AudioRouter>>>` already in scope, calling `state_sync::tick(...)`. Implementer A owns `state_sync.rs` in full; Implementer B does not touch it. Implementer B's only `app.rs` change is registering one new GAction (`pause-recording-toggle`), discussed below.

#### Shutdown capture (Issue 3, finale)

In `app.connect_shutdown`, immediately before dropping `AppResources` (which destroys the sinks via `VirtualSinks::drop`), iterate the three virtual targets, query each volume, and write through `persistence::save_volume_entry`. This catches the user's final state at quit time — important because the periodic poll only fires every 2 seconds, and a fast change-then-quit could miss the periodic window.

The existing `persistence::save_assignments()` call at shutdown **stays** but with one change to align with the new monotonic rule: its inner logic at `persistence.rs:109-115` currently `assignments.remove(&input.app_name)` when a stream's current sink is unmanaged. **That removal is removed.** Both paths (periodic tick + shutdown save) now agree: managed→managed updates the entry, anything else is silently ignored. This eliminates the "monotonic in tick, destructive at shutdown" inconsistency the critic flagged.

The shutdown call captures any sink-input that briefly existed within a single 2-second window (e.g. notification chimes), then quits cleanly.

### Issue 4: Clips section relocation + expansion + terminology

#### Pause/resume design — what "pause" means

GSR's replay mode has no native pause primitive (verified against the GSR docs at `git.dec05eba.com/gpu-screen-recorder/about/` — SIGUSR2 pause is "only applicable and useful when recording (not streaming nor replay)"). The spec therefore defines pause as **stop the recorder entirely**:

- Pause: send the GSR child SIGINT via the existing `disarm()` path, mark a new `user_paused: bool` on `BufferController`, and stop participating in auto-arm. The replay buffer is **lost**.
- Resume: clear `user_paused`, re-enter the normal auto-arm flow (which arms when a game is detected, or immediately if `always_armed` is on).

UI copy must communicate the cost honestly. The Pause button's tooltip reads **"Pause recording. The last N seconds of recording will be lost."** (substitute the actual configured length). This phrasing keeps the terminology pass coherent — no "buffer" word leaks into user-visible copy.

A new `BufferState` variant `Paused` is added, sitting between `Idle` and `Arming` in the conceptual lifecycle. Transitions:

| From state | Pause action | Resume action | Other transitions out of state |
|---|---|---|---|
| Uninitialized | (button hidden) | (button hidden) | → Idle on portal-pick complete |
| Idle | → Paused | (button shows Pause) | → Arming on game-detected (auto-arm) |
| Arming | (interpret as Pause: cancel arm, → Paused) | n/a | → Armed on backend ack; → ErrorState on backend death |
| Armed | → Paused (disarm, last N seconds lost) | n/a | → Saving on user save; → ErrorState on backend death |
| Saving | button disabled until Saved | n/a | → Armed on Saved event |
| ErrorState | (button hidden — retry button shown instead) | n/a | → Idle on user retry / portal reset |
| Paused | n/a | → Idle (auto-arm picks up from here) | → Uninitialized on portal-pick reset |

`auto_arm` and `always_armed` are honored *after* Resume, not during Paused. Pausing does not flip those flags; it just adds a gate that prevents them from acting. This means the user does not need to remember to re-enable always-armed after pausing.

The `restart_attempts` rolling-window counter (currently `MAX_RESTARTS_PER_WINDOW = 3` per 30 s) is **cleared** when entering Paused, matching the existing convention from `StartReplay` / `Stop`: user intent overrides the limiter.

#### New `ClipCommand` variants

Add to `src/clips/mod.rs`'s `ClipCommand` enum:

```rust
ClipCommand::PauseRecording,   // backend: enter user-paused, disarm
ClipCommand::ResumeRecording,  // backend: clear user-paused, drive normal arm path
```

Two variants instead of one `SetRecordingEnabled(bool)` keep the supervisor's match arms small and self-documenting. Both clear `restart_attempts` per above.

The supervisor's `run_backend` command loop gains two new arms. Match the existing convention used by `StartReplay` at `backend.rs:1063` and the existing Stop/save arms at `backend.rs:1077-1096` (both clear `restart_attempts` on entry):

```rust
Ok(ClipCommand::PauseRecording) => {
    restart_attempts.clear();
    if active.is_some() {
        disarm(&mut active);  // SIGINT via existing path
    }
    // BufferController transitions: caller side is responsible (see below)
}
Ok(ClipCommand::ResumeRecording) => {
    restart_attempts.clear();
    // Resume just re-enables auto-arm; the next tick of BufferController will
    // re-call StartReplay if conditions are met.
}
```

`BufferController` flips `user_paused` on the GTK main thread (the BufferController already lives there), and re-applies arm logic from its existing tick.

**Pause-while-Arming race:** if the user clicks Pause while the controller is in `Arming` (waiting for backend `Armed` ack), the backend may produce the `Armed` event after the `PauseRecording` command lands in its queue. The supervisor's `PauseRecording` handler always calls `disarm()` if `active.is_some()`, which sends SIGINT — so even if `Armed` ack happened a millisecond before, the disarm still fires and shuts down GSR. The `BufferController` flips to `Paused` state regardless of whether the intermediate `Armed` ack arrived. No special handling needed at the controller level; the existing `set_armed`/`set_arming` transitions are idempotent for re-entry.

#### Pause/Resume GAction handler sketch

Single GAction `app.pause-recording-toggle` (registered in `app.rs` adjacent to the existing `app.save-clip` cluster). The handler reads `BufferController.user_paused`, inverts it locally, sends the matching `ClipCommand`, and triggers an indicator refresh:

```rust
let toggle_action = gtk::gio::ActionEntry::builder("pause-recording-toggle")
    .activate({
        let buffer = buffer.clone();
        let cmd_tx = cmd_tx.clone();
        let widgets = widgets.clone();
        move |_app: &adw::Application, _action, _param| {
            let mut b = buffer.borrow_mut();
            if b.user_paused() {
                b.resume();
                let _ = cmd_tx.send(ClipCommand::ResumeRecording);
            } else {
                b.pause();
                let _ = cmd_tx.send(ClipCommand::PauseRecording);
            }
            widgets.refresh_clips_section();
        }
    })
    .build();
app.add_action_entries([toggle_action]);
```

The handler matches the existing `app.save-clip` shape (search for `gtk::gio::ActionEntry::builder("save-clip")` in `app.rs` for the in-tree reference). `widgets.refresh_clips_section()` is a new method added by Implementer B that re-reads the buffer state and updates label/button text.

#### Layout change in `src/window.rs`

- Remove the clip indicator from `build_status_card()`. Drop `clips_row` and `clips_indicator` fields from `StatusResult`. Drop the `clips_row` line from the row-height `SizeGroup`.
- Add `build_clips_section(...) -> (adw::PreferencesGroup, ClipsSectionWidgets)` returning a styled card titled **"Clips"** (per user choice).
- Attach the new section to the dashboard grid as row 1 spanning both columns: `grid.attach(&clips_section, 0, 1, 2, 1)`.
- `Widgets.clips_indicator` continues to live on the top-level `Widgets` struct but now sources from the new section.
- The dashboard `card_height` `SizeGroup` continues to group `status_card` and `device_card` — the new clips section is *outside* that group, so its width is just the grid's column-span result and its height is intrinsic.

#### Contents of the Clips section

Vertical `gtk::Box`, in order:

1. **Status indicator badge** — the existing `crate::clips::StatusIndicator` widget, unchanged in appearance (colored dot + game name + state label).
2. **Action row** — horizontal `gtk::Box` containing:
   - **"Save Clip Now" button** (primary, `add_css_class("suggested-action")`) — uses `set_action_name(Some("app.save-clip"))` to reuse the existing GAction. No new wiring.
   - **Pause / Resume toggle button** — `gtk::Button` whose label flips between "Pause Recording" and "Resume Recording" depending on `BufferController.user_paused`. Action name `app.pause-recording-toggle`. Tooltip text per the cost-disclosure rule above.
   - **Recording-duration label** — text reading e.g. "Recording last 60 seconds" (or "Recording last 5 minutes" for 300 s). Pulled from `ClipSettings.buffer_length` (u32, seconds). Format rule: `< 60 → "Recording last N seconds"`, `60 ≤ N < 3600 → "Recording last M minutes"` (where `M = N / 60` rounded), `≥ 3600 → "Recording last H hour(s)"`. Re-renders when `ClipSettings` changes.
   - **Hotkey hint label** — text e.g. "Hotkey: ALT+S". Pulled from a new `ClipSettings.save_hotkey_display` field (see below). Re-renders when the binding changes.

The action row uses `homogeneous = false` and `spacing = 12`. Suggested order left→right: Save | Pause/Resume | (flexible spacer) | Duration | Hotkey.

#### New persisted field: `save_hotkey_display`

`ClipSettings` gains:

```rust
pub save_hotkey_display: String,  // human-readable; e.g. "ALT+S". Default "ALT+S".
```

Persisted as `save_hotkey_display=...` in the existing `clips_settings.txt`. The value comes from the **portal's own `trigger_description` accessor** on the `Shortcut` response type returned by `org.freedesktop.portal.GlobalShortcuts.ListShortcuts` — the user-readable chord description the portal itself displays in its UI.

**ashpd 0.11.1 API (verified against upstream docs):**

```rust
// Registration uses NewShortcut (NOT `Shortcut::new`):
let shortcut = NewShortcut::new("save-clip", "Save the last N seconds of gameplay")
    .preferred_trigger(Some("ALT+s"));
proxy.bind_shortcuts(&session, &[shortcut], parent).await?;

// Read back the bound chord:
let req = proxy.list_shortcuts(&session).await?;
let resp = req.response()?;          // Response<ListShortcuts>
let shortcuts = resp.shortcuts();    // &[Shortcut]
let display = shortcuts.iter()
    .find(|s| s.id() == "save-clip")               // s.id() -> &str
    .map(|s| s.trigger_description().to_string()); // s.trigger_description() -> &str
```

Two ashpd types are distinct: `NewShortcut` (the request shape for `bind_shortcuts`) and `Shortcut` (the response shape from `list_shortcuts`). Don't confuse them. `trigger_description()` is a method, not a field, and returns `&str` — copy with `.to_string()` to persist.

Write sites for the field:

- **After every successful `proxy.bind_shortcuts(...).await` call** in `src/clips/hotkey.rs`: immediately call `proxy.list_shortcuts(&session).await`, run the snippet above, and persist via the existing `mark_*` pattern (mutate the shared `Rc<RefCell<ClipSettings>>` cell first, then call `settings::save(&cell.borrow())`). If the `list_shortcuts` call fails, leave the field unchanged and default to "ALT+S" on first read.
- This covers both onboarding-page-3 binding and the Settings → Clips "Reset Hotkey" flow because both paths go through `bind_shortcuts`.
- An optional polish — subscribe to the portal's **`ShortcutsChanged` signal** on the same session (which the existing `run_global_shortcuts` listener already holds open) and refresh `save_hotkey_display` when the user re-binds via the portal's own UI. This is filed under "deferred polish" in Risks below, not Bundle B's mandatory scope.

If the field is empty at read time, fall back to the literal string "ALT+S" (the documented default). We persist our own label rather than query the portal at display time because the portal session may not be live at startup before the user opens the Clips card.

Cleanup: existing stale comments at `src/clips/settings.rs:322` ("ashpd 0.10 has no `list_shortcuts` API") and `src/clips/hotkey.rs:153` ("ashpd 0.10 doesn't expose the (proposed) portal `ConfigureShortcuts` method directly") should be updated to reflect the actual dependency (0.11.1) and the newly-used API. Implementer B updates these in the same pass.

#### Terminology pass

Anywhere user-visible strings in the clip UI use the words "buffer" or "buffering" describing the user-facing state, replace with "recording". The pass is **enumerated** rather than greppy:

| File | String(s) to change |
|---|---|
| `src/clips/indicator.rs` | State labels ("Buffering" → "Recording"); tooltip strings ("Buffer armed" → "Recording armed"). Audit the full file — there are a handful of literals to rewrite. |
| `src/clips/settings.rs` | Enumerated literals (verified from current source): line 228 `"Pick the screen recorded by the clip buffer"` → `"Pick the screen recorded by the clipper"`; line 251 `"Seconds of gameplay kept in the replay buffer"` → `"Seconds of gameplay kept available for recording"`; line 378 `"Start buffering when a known game launches"` → `"Start recording when a known game launches"`; line 385 `"Buffer continuously, even outside games"` → `"Record continuously, even outside games"`. Also UI label "Buffer length" → "Recording length". |
| `src/clips/notifications.rs` | **Audit only — no buffer-strings present today**. Listed for completeness in case future work adds toasts mentioning the buffer. |
| `src/window.rs` | Any string added in this PR uses "recording". The new Clips section's labels use "Recording last N seconds" and "Hotkey: …". |
| `src/clips/browser.rs` | Audit the full file for any user-visible literals referencing "buffer" or "buffering". Most clip-browser strings refer to saved files, not the buffer, so the audit is expected to find few or no changes. |

Implementer B reads each listed file end-to-end and rewrites only the user-visible literal strings. Internal type names (`BufferController`, `BufferState`, `buffer.rs`, `buffer_length`), variable names, log lines, doc comments, and code comments stay. The "buffer" word in those internal contexts is fine — it describes the in-memory replay buffer accurately.

Verification: `grep -nE "\"[^\"]*[Bb]uffer(ing)?[^\"]*\"" src/clips/indicator.rs src/clips/settings.rs src/clips/notifications.rs src/window.rs src/clips/browser.rs` (word-bounded to literal strings only) should return only:
- Log-line strings (e.g. `log::info!("buffer: ...")`)
- Identifier references in `format!`-style strings that compose a log line
- Zero user-visible UI label / button / tooltip / toast string literals

The Pause tooltip ("Pause recording. The last N seconds of recording will be lost.") uses "recording" exclusively — no leak of the "buffer" term into user-facing copy.

## Detailed designs

### Issue 1 — User-move detection

#### Decision: only persist moves *onto* managed sinks

Today's `save_assignments` removes an entry when a stream's current sink is unmanaged. The proposed user-move detection extends that. **The new design drops the "remove on move-off-managed" rule** for two reasons:

- *A brief preview to a different physical speaker via system sound settings is a legitimate user action that should not destroy the saved assignment for the app.* The user expects Tidal → Music to persist across app restarts, even if they briefly auditioned Tidal on the laptop speakers.
- *App_name as a persistence key is brittle* (some apps don't set `application.name`; browsers report the same name across tabs; Discord changes name between voice and notification streams) — this is an existing issue, not caused by this spec, but the brittle key means we shouldn't aggressively *destroy* entries on transient observations.

The new rule is **monotonic**: a move *onto* a managed sink updates / creates the saved entry; a move *off* a managed sink is ignored (the entry stays). The user can still purge entries via the existing "Clear Config" button.

#### Race against pactl eventual consistency

`pactl move-sink-input` is generally synchronous, but the next call to `pactl list sink-inputs` can observe pre-move state for a brief window after a self-initiated move. The previous revision missed that **the pre-move sink is typically `SteelSeries_Game`** — the default sink set by `init_pipeline`, which is *managed*. So a "pre-move sink is unmanaged → safely ignored" argument doesn't hold: an Occupied branch reading `tracked = Music` (intended target) and `current_sink = Game` (stale pactl) would erroneously update the saved entry to Game, then flap back on the next tick when pactl catches up.

The fix is **timestamped post-move suppression**. Add a second map alongside `tracked`:

```rust
self_move_pre_sink: HashMap<u32, (String /*pre-move sink*/, Instant /*move time*/)>
```

Behavior:

- **Vacant branch, Ok(()) case:** insert `tracked[id] = target` *and* `self_move_pre_sink[id] = (observed_pre_move_sink, Instant::now())`.
- **Vacant branch, Err(...) case:** insert `tracked[id] = pre_move_sink`; do not touch `self_move_pre_sink`.
- **Occupied branch:** if `tracked[id] != current_sink`:
  - Look up `self_move_pre_sink[id]`. If `Some((pre_sink, t))` AND `pre_sink == current_sink` AND `t.elapsed() < SUPPRESSION_WINDOW (3s)`: this is eventual-consistency lag. Skip processing. Do **not** remove the entry — let the timestamp-based expiry handle it. This lets the suppression hold across multi-tick lag.
  - Else (no suppression entry, OR pre_sink ≠ current_sink, OR window expired): this is a real user move. Apply the "only persist onto managed sinks" rule. Remove the `self_move_pre_sink` entry if present.
- **Tick-end maintenance:** drop entries from `self_move_pre_sink` whose `t.elapsed() ≥ SUPPRESSION_WINDOW`.
- **Garbage collection:** at end-of-tick, drop entries from both maps for IDs not in `live_ids`.

`SUPPRESSION_WINDOW = Duration::from_millis(3000)` — slightly more than one tick period (2 s), tight enough that a real user counter-move within 3 s of our self-move loses by 2 ticks (acceptable in practice; the user would have to move twice within 3 seconds for this to surface, and even then the third tick captures their intent). Multi-tick pactl lag of up to ~3 s is silently tolerated.

#### User counter-move during the suppression window (edge case)

If the user manually moves a stream back to its pre-move sink during the suppression window, we cannot tell their action apart from pactl-lag. The third tick (after the window expires) will observe `tracked ≠ current_sink AND self_move_pre_sink entry expired` and persist the user's move correctly. **Net behavior:** the user's deliberate counter-move within 3 seconds of our auto-route is captured with ≤ 4 seconds of latency. This is documented as a known limitation; users moving streams twice within 3 seconds is uncommon enough that a tighter design is out of scope.

Sink-input ID reuse: PipeWire sink-input IDs can be reused after a long session. The garbage-collection pass at end-of-tick ensures that a reused ID always enters the tick as Vacant from the tick's perspective, which routes correctly via the saved entry for the new `app_name`.

#### First-tick seeding

Add `persistence::initial_tracked() -> HashMap<u32, String>` returning `(id → current_sink_name)` for every live sink-input at startup. `app.rs` calls it before installing the tick closure and seeds the closure's `tracked` map. First tick sees all existing streams as Occupied with identical sink → no-ops. Streams that appear after startup hit the Vacant branch correctly.

`init_pipeline::restore_assignments()` stays as-is — it runs once at startup, moves saved streams to their target sinks immediately, and the seeding then captures the *final* sink each stream landed on. The tick is additive, not replacing.

#### Helper signatures

```rust
// In src/audio/persistence.rs:

// New
pub fn initial_tracked() -> HashMap<u32, String>;  // id -> sink_name for live sink-inputs

// New — single-entry write, same shape as save_mixer_routing_entry
pub fn update_saved_assignment(app: &str, sink: &str);

// state_sync calls these in a tick — they internally call list_sink_inputs / list_sinks_by_id
pub fn reconcile_stream_state(tracked: &mut HashMap<u32, String>);

// Existing — UNCHANGED
pub fn save_assignments();
pub fn restore_assignments();
pub fn clear_saved() -> Result<(), String>;
```

`restore_new_streams` is renamed to `reconcile_stream_state` and gets the new signature. Existing callers in `app.rs` are updated by Implementer A.

#### Tick pseudocode (Issue 1 portion)

```text
fn reconcile_stream_state(tracked, self_move_pre_sink):
    // Re-read saved from disk each tick. Single-digit-KB file, page cache makes
    // this microseconds. Avoids needing a shared cache that must be invalidated
    // by Clear Config and other state-mutation paths.
    saved = read_saved()
    sinks_by_id = list_sinks_by_id()
    inputs = list_sink_inputs()
    live_ids = set()

    for input in inputs:
        live_ids.add(input.id)
        current_sink = sinks_by_id[input.sink_id]

        match tracked.entry(input.id):
            Vacant:
                if input.app_name == "pw-cli": continue
                if let Some(target) = saved.get(&input.app_name):
                    if current_sink != target:
                        let pre_move_sink = current_sink.to_string()
                        result = move_sink_input(input.id, target)
                        if result.is_ok():
                            tracked.insert(input.id, target.clone())
                            self_move_pre_sink.insert(input.id, (pre_move_sink, Instant::now()))
                        else:
                            tracked.insert(input.id, pre_move_sink)
                    else:
                        tracked.insert(input.id, target.clone())
                else:
                    tracked.insert(input.id, current_sink.to_string())

            Occupied(entry):
                if entry.get() != current_sink:
                    let suppress = match self_move_pre_sink.get(&input.id):
                        Some((pre, t)) if pre == current_sink && t.elapsed() < SUPPRESSION_WINDOW => true,
                        _ => false,
                    };
                    if suppress:
                        // Eventual-consistency stagger; keep the suppression
                        // entry — it will expire naturally.
                        continue;
                    if is_managed(current_sink):
                        update_saved_assignment(input.app_name, current_sink)
                        *entry.get_mut() = current_sink.to_string()
                        self_move_pre_sink.remove(&input.id)
                    // else: don't destroy saved entry on transient unmanaged move (monotonic rule)

    // Tick-end maintenance
    self_move_pre_sink.retain(|_, (_, t)| t.elapsed() < SUPPRESSION_WINDOW)
    tracked.retain(|id, _| live_ids.contains(id))
    self_move_pre_sink.retain(|id, _| live_ids.contains(id))
```

**Why re-read saved each tick rather than caching:** the prior revision proposed an in-memory `saved_cache` to avoid per-tick disk reads. Critic round-3 correctly observed that this creates a Clear-Config-cache-invalidation problem: the existing "Clear Saved Config" button at `src/window.rs:727-731` only deletes the file — if a tick-owned cache survives, the next tick re-routes from stale state and re-creates the file. Re-reading the file each tick eliminates this footgun. The saved-assignments file is single-digit-KB; on the OS's page cache it's a sub-millisecond op. The tradeoff is right.

### Issue 2 — Mic hotplug detection with stable identifier matching

#### Save node-name and stable-id chain at user-pick time

`mixer_routing.txt` today stores entries as `channel\tdevice_name\n`. The new design extends the `"mic"` entry to a tab-separated **5-field record**: `mic\t<node_name>\t<product_name>\t<bus_path>\t<bluez_address>\n`. Any field that's not available at pick time is written as the literal empty string between the tabs.

Why 5 fields and not a single fallback chain: critic round-3 correctly observed that picking `device.product.name` first is wrong for Bluetooth, where the MAC (`api.bluez5.address`) is intrinsically unique and stable across A2DP↔HSP profile switches. Two paired Sony WH-1000XM4s of the same model would collide on `product.name` but never on `api.bluez5.address`. Storing all three available properties lets the hotplug check try the most-specific identifier per device class:

**Hotplug match priority** (computed against each available source's properties):
1. **Exact node-name match.** Fast path; covers no-rename cases.
2. **`api.bluez5.address` match** (if both saved and available have a non-empty value). Strongest identifier for Bluetooth devices; never collides.
3. **`device.bus_path` match** (if both have a non-empty value). Stable per-USB-port; covers re-enumeration on the same port.
4. **`device.product.name` match** (if both have a non-empty value). Weakest; matches across USB-port changes for unique-model devices, but collides on duplicates.
5. **No match → no reroute.**

Backward-compatible: 2-field `mic` lines (the existing format) are treated as `<product_name>` = `<bus_path>` = `<bluez_address>` = empty (only step 1 applies). Old 3-field `mic` lines (revision-1 format with just `product_name`) parse with `<bus_path>` = `<bluez_address>` = empty. Forward-compatible: future identifiers can be appended without breaking old readers.

PipeWire-property stability matrix:

| Property | USB plug back into same port | USB plug into different port | BT profile switch (A2DP↔HSP) | Power cycle |
|---|---|---|---|---|
| `device.product.name` | usually stable | usually stable | varies — sometimes changes | usually stable |
| `device.bus_path` | stable | changes | n/a (BT has no bus path) | stable |
| `api.bluez5.address` | n/a | n/a | stable | stable |

#### Parser extension for list_physical_sources

`sinks::list_physical_sources()` is extended to also return the stable-id chain. New tuple shape:

```rust
Vec<(
    String,            // node_name (e.g. "alsa_input.usb-Vendor_Product-…")
    String,            // description (e.g. "Alias Pro Microphone")
    Option<String>,    // device.product.name
    Option<String>,    // device.bus_path
    Option<String>,    // api.bluez5.address
)>
```

The current `parse_device_list` at `src/audio/sinks.rs:184-228` only reads `Name:` and `Description:` lines. The extension must enter the `Properties:` block, recognize indentation (`\t<key> = "<value>"`), strip key prefixes, and unquote the value. Pseudocode for the parser extension:

```text
let mut in_properties = false
let mut current_props: HashMap<&str, String> = HashMap::new()

for line in output.lines():
    let trimmed = line.trim()
    let is_indented = line != trimmed && !trimmed.is_empty()  // any leading whitespace
    if line.starts_with("Source #") or line.starts_with("Sink #"):
        flush_current_record_into_results()
        in_properties = false
        current_props.clear()
    elif trimmed == "Properties:":
        in_properties = true
    elif trimmed.is_empty():
        in_properties = false   // blank line exits the block
    elif !is_indented:
        in_properties = false   // unindented line is a new top-level field
    elif in_properties:
        if let Some((key, value)) = trimmed.split_once(" = "):
            if key in ("device.product.name", "device.bus_path", "api.bluez5.address"):
                current_props.insert(key, value.trim_matches('"').to_string())
```

The existing `parse_device_list` at `src/audio/sinks.rs:184-228` uses a similar trim-then-strip-prefix pattern; mirror that style. `pactl list` indentation can be tabs OR spaces depending on the version; check `line != line.trim()` rather than `starts_with("\t")` to be version-robust.

Implementer A may use a state machine or simpler line-prefix approach — the spec gives the shape, the implementer picks the cleanest expression.

Consumers of `list_physical_sources` to update for the new tuple shape: `mixer.rs` (5 destructuring sites — find via `grep -n list_physical_sources src/`). Each site can ignore the new fields (`(name, desc, _, _, _)`) except the mic-source picker, which records the chain at save time.

When the user picks a mic from the dropdown, `reroute_mic` is called with the node name; `persistence::save_mixer_routing_mic(node_name, stable_id)` writes both fields. `stable_id` is the first-non-empty value from the chain above.

#### Match logic in hotplug check

```text
fn check_mic_hotplug(router, available_sources):
    let Some(saved) = router.preferred_mic.as_ref() else: return  // no preference
    // saved is a struct: { node_name, product_name, bus_path, bluez_address }

    if router.current_mic_source.as_deref() == Some(saved.node_name): return  // already on it

    // Step 1: exact node-name match
    if available_sources.iter().any(|s| s.node_name == saved.node_name):
        router.reroute_mic(saved.node_name.clone());
        return

    // Step 2: api.bluez5.address — strongest, BT-only
    if !saved.bluez_address.is_empty():
        if let Some(s) = available_sources.iter().find(|s|
            !s.bluez_address.is_empty() && s.bluez_address == saved.bluez_address
        ):
            router.reroute_mic(s.node_name.clone());
            return

    // Step 3: device.bus_path — stable per USB port
    if !saved.bus_path.is_empty():
        if let Some(s) = available_sources.iter().find(|s|
            !s.bus_path.is_empty() && s.bus_path == saved.bus_path
        ):
            router.reroute_mic(s.node_name.clone());
            return

    // Step 4: device.product.name — weakest, collides on duplicates
    if !saved.product_name.is_empty():
        if let Some(s) = available_sources.iter().find(|s|
            !s.product_name.is_empty() && s.product_name == saved.product_name
        ):
            router.reroute_mic(s.node_name.clone());
            return

    // No match — saved device not online; do nothing this tick
```

The `available_sources` shape is the 5-field tuple from the parser extension below; `router.preferred_mic` is the cached struct loaded once at `AudioRouter::create` from the new 5-field `mixer_routing.txt` mic line.

The preferred-mic cache lives on `AudioRouter` and is loaded once from `mixer_routing.txt` in `AudioRouter::create`. It's invalidated/updated on every `reroute_mic` call. The hotplug check reads the cache; **no per-tick disk read.**

#### Edge cases

- *User unplugs preferred mid-session.* The link to the preferred device breaks at the PipeWire layer. Our hotplug check doesn't currently auto-fall-back to a different device. In v1, the user manually picks a fallback via the dropdown; their pick updates the saved preference, so when the preferred device comes back, we'll switch back to *whatever the user last picked* — which may now be the fallback, not the original preferred. This is a known behavior of "last user pick wins"; documented as a limitation.
- *User has the dropdown open when hotplug fires.* The dropdown rebuilds its model on open via the existing `GestureClick` Capture-phase handler in `mixer.rs`. A reroute mid-display can cause a model swap; the user sees the dropdown's selection update to the new pick. Acceptable — same behavior as today when the user picks via dropdown.
- *Two devices with the same product name.* The first match wins. Out of scope to disambiguate further (PipeWire's node-name uniqueness already handles this at a lower layer).

### Issue 3 — Volume persistence (NEW on clipping-system)

#### Step 1: Port volume helpers from main

Implementer A copies / adapts from `/var/home/admin/Documents/Code/SteelseriesFlatpak/src/audio/persistence.rs` (the main branch's version of the file):

- The `VOLUMES_FILE` constant
- The `volumes_path()` helper
- `pub fn load_volumes() -> HashMap<String, u32>`
- `pub fn save_volume_entry(channel: &str, volume_percent: u32)`

And from main's `src/app.rs::init_pipeline`:

- The `for (name, pct) in persistence::load_volumes() { … }` apply-volumes loop, placed after `VirtualSinks::create()` and before `AudioRouter::create()`

And from main's `src/mixer.rs`:

- The `if name == sinks::MUSIC_SINK_NAME || name == sinks::AUX_SINK_NAME || name == sinks::MIC_SOURCE_NAME { persistence::save_volume_entry(name, pct); }` block inside the slider `connect_value_changed` handler

These are mechanical ports — same code, no behavior changes.

#### Step 2: Periodic capture in the state-sync tick

Maintain `last_saved_volumes: HashMap<String, u32>`, seeded from `persistence::load_volumes()` at startup. Each tick, for `[MUSIC_SINK_NAME, AUX_SINK_NAME]` call `get_sink_volume`; for `MIC_SOURCE_NAME` call `get_source_volume`. On change vs. the cache:

```rust
if last_saved_volumes.get(name).copied() != Some(vol) {
    persistence::save_volume_entry(name, vol);
    last_saved_volumes.insert(name.to_string(), vol);
}
```

#### Step 3: Shutdown capture

In `app.connect_shutdown`, before `drop(res)`:

```rust
for name in [MUSIC_SINK_NAME, AUX_SINK_NAME] {
    if let Ok(vol) = sinks::get_sink_volume(name) {
        persistence::save_volume_entry(name, vol);
    }
}
if let Ok(vol) = sinks::get_source_volume(MIC_SOURCE_NAME) {
    persistence::save_volume_entry(MIC_SOURCE_NAME, vol);
}
```

Idempotent with the periodic capture; running both is harmless.

#### Hypothesis for the "random values" symptom (testable)

Two hypotheses could explain the user's "random volume on relaunch" complaint:

- **H_A: WirePlumber state cache restoration.** PipeWire/WirePlumber's per-node state cache restores whatever value it last saw for `SteelSeries_Mic`. Since we never explicitly write a value at startup (volume helpers don't exist on the clipping branch), the recreated node lands on whatever WirePlumber remembered — which can drift run-to-run if multiple sessions have set different values.
- **H_B: `pactl set-source-volume` is silently ignored on `Audio/Source/Virtual` nodes** on certain PipeWire builds. Setting succeeds but the actual value doesn't change. Reading back returns whatever the node defaulted to.

**Pre-implementation discriminator (project-tester runs this BEFORE coding starts):**

```bash
# Discriminator: can we set the volume of SteelSeries_Mic at all?
distrobox enter fedora-dev -- bash -c '
  cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clipping && cargo build && cd -
'
./target/debug/arctis-chatmix &  # let virtual sinks come up
sleep 3
pactl set-source-volume SteelSeries_Mic 50%
sleep 0.2
pactl get-source-volume SteelSeries_Mic
# If this returns 50% → H_A is the candidate; save-and-restore design is correct.
# If this returns something else (default, or unchanged) → H_B is true; the fix
#   needs a different mechanism (e.g. pw-cli set-param on the adapter node directly,
#   or persisting volume via node.target props at create-node time).
```

If the discriminator reveals **H_A**, the spec's fix (port from main + periodic capture + shutdown capture) is correct as-designed.

If the discriminator reveals **H_B**, scope expansion required: the spec's "periodic re-apply" doesn't help because `pactl set-source-volume` is the broken layer in that universe. The fallback is to set the volume at node-creation time via `pw-cli`'s `Props` argument when creating the adapter node. This is a different code path from `pactl set-source-volume` and may bypass the silent-ignore bug. **If H_B is confirmed, Bundle A scope expands** to also extend `create_pw_node` in `src/audio/sinks.rs` to accept an optional initial volume and write it into the `Props` block at node creation. Project-tester flags this back to the team lead before Bundle A starts.

`project-tester` runs the discriminator *first*. The choice between fix-A (save-restore via pactl, this spec's current design) and fix-B (initial volume via `pw-cli` Props at creation, scope expansion) is determined by that test before Implementer A begins.

### Issue 4 — Clips section relocation + expansion + terminology

See "Architecture → Issue 4" for the layout, contents, pause-design, hotkey-hint persistence, and terminology pass.

## Data files / config impact

- `assignments.txt` — file shape unchanged. Written more frequently (per-tick on change, plus at shutdown).
- `mixer_routing.txt` — **`mic` line extended** to a 3-field tab-separated triple: `mic\t<node_name>\t<product_name>\n`. Lines with two fields parse as before with `<product_name>` empty (backward compatible).
- `volumes.txt` — **new on this branch** (ported from main). Format: `<pipewire_node_name>\t<volume_percent>\n` lines.
- `clips_settings.txt` — **new field `save_hotkey_display=<chord_label>`**. Default "ALT+S". Persisted at portal bind time.
- `eq_state.txt`, `eq_presets/*.txt`, autostart `.desktop` file — unchanged.

## Threading

No new threads. No new mpsc channels. All four issues live inside the existing GTK-main-thread polling tick. `AudioRouter::check_mic_hotplug` runs on the main thread and shells out to `pactl` / `pw-link` synchronously, matching the existing audio-module style.

`Rc<RefCell<...>>` borrow safety: the tick's `borrow_mut` on `router` is safe because all UI callbacks (dropdown handlers, mixer slider, etc.) complete-then-release before the next main-loop iteration. `glib::timeout_add_seconds_local` schedules the callback as a separate iteration; no risk of nested borrow under current architecture.

Tick latency budget: the unified tick makes 6 synchronous `pactl` calls (`list sink-inputs`, `list sinks short`, `list sources` verbose, `get-sink-volume Music`, `get-sink-volume Aux`, `get-source-volume Mic`). The dominant cost is the verbose `list sources` (parses Properties blocks). Conservative worst-case estimate: ~400 ms blocking every 2 s on slower hardware (Bluetooth-heavy session, busy WirePlumber). Acceptable for a desktop app at 2 Hz, but `project-tester` must run `time pactl list sources` and `time pactl list sink-inputs` on the target Bazzite hardware with a representative session (Bluetooth audio + a few apps + active virtual sinks) and report actual numbers before sign-off — see Testing section.

If actual measurements exceed 500 ms / 2 s on the user's hardware, the optimization is to cache the `list sources` result and refresh only on PipeWire object events (subscribe via `pw-cli -m`). Explicitly out of scope here; documented as a deferred polish in Risks.

## Error handling

- `pactl get-*-volume` failures: log at `warn` level, skip this tick's capture for the failing channel, retry next tick.
- Hotplug check failures (e.g. `pw-link` errors during reroute): `reroute_mic` already logs and persists user intent regardless of link success. Same applies in the hotplug path.
- User-move detection on a stream created and destroyed within 2 seconds: invisible to us. Acceptable — short-lived streams (notification sounds) are not what the user wants persisted.
- Filesystem write failures on `save_*_entry`: log at `warn` level, do not retry. The next change triggers another write attempt.
- Pause command issued while supervisor is mid-restart: `restart_attempts.clear()` runs first; the next tick of the supervisor sees a clean slate.

## Testing

### Existing tests stay green

- All existing unit tests (HID protocol, biquad, Sonar import, spatial state, mixer dead-code) continue passing.
- After Implementer A's changes: `cargo test` inside the distrobox passes. Implementer B's changes are mostly UI and don't break tests.

### New unit tests

- `persistence.rs`:
  - `tracked_map_user_move_to_managed_updates_saved` — simulate stream's sink changing from managed-A to managed-B; saved file gains/updates entry to B.
  - `tracked_map_user_move_to_unmanaged_keeps_saved` — stream moves to an unmanaged sink; saved entry is *retained* (per the new monotonic rule).
  - `tracked_map_garbage_collects_dead_ids` — after a poll where a tracked stream no longer appears in `list_sink_inputs`, its entry is removed.
  - `update_saved_assignment_preserves_other_entries` — single-entry write doesn't clobber unrelated entries.
  - `initial_tracked_seeds_existing_streams` — given fixture sink-input output, returns the right id→sink map.
- `router.rs`:
  - `check_mic_hotplug_noop_when_no_saved_preference` — empty `mixer_routing.txt` → no reroute call.
  - `check_mic_hotplug_noop_when_already_on_saved` — `current_mic_source == saved` → no reroute call.
  - `check_mic_hotplug_exact_node_match` — saved node X, current Y, X appears → reroute called with X.
  - `check_mic_hotplug_product_name_fallback` — saved node X with product P; current Y. X is gone but a new node Z with product P appears → reroute called with Z.
  - `check_mic_hotplug_no_match_without_product_id` — old 2-field mic line in saved (no product), node renamed, no exact match → no reroute (acceptable behavior on upgrade).
- `state_sync.rs`:
  - `tick_no_volume_changes_no_writes` — if all virtual volumes match cache, no `save_volume_entry` call.
  - `tick_volume_change_triggers_write_and_cache_update` — single change → single write, cache updated.
- `buffer.rs`:
  - `pause_from_armed_transitions_to_paused` — `BufferController` in Armed state, `pause()` → `Paused`, `disarm` called.
  - `resume_from_paused_to_idle` — `BufferController` in Paused, `resume()` → Idle, ready for auto-arm.
  - `pause_clears_restart_attempts` — pre-condition non-empty `restart_attempts`; pause clears it.

Tests use a thin `SinkInputProvider` / `SourceListProvider` trait or fn-pointer seam so `list_sink_inputs` / `list_physical_sources` can be mocked. Implementer A chooses the cleanest seam.

### Manual verification (project-tester runs after both implementers report)

- **Pre-flight: tick latency baseline.** On the user's Bazzite hardware, run `time pactl list sources` and `time pactl list sink-inputs` 5 times each with a representative session active (Bluetooth audio + a music app + Discord). Report the median wall-clock. If `list sources` median exceeds 300 ms, escalate to add a caching pass before sign-off.
- **Issue 1:** Launch a known sink-input source (e.g. `paplay --device=SteelSeries_Chat /usr/share/sounds/freedesktop/stereo/bell.oga` or open a music app). Move it to `SteelSeries_Music` via system sound settings. Within ~3 seconds, `cat ~/.config/arctis-chatmix/assignments.txt | grep <app>` shows `→ SteelSeries_Music`. Kill the stream, relaunch the app (or paplay), confirm it routes to Music. Move it briefly to the laptop speakers (unmanaged sink), wait 3 seconds, confirm `assignments.txt` still has the Music entry (monotonic rule).
- **Issue 2:** Save preferred mic = Alias Pro. Turn off Alias Pro. Quit and relaunch our app — Mixer dropdown shows the fallback. Turn Alias Pro on. Within ~3 seconds, dropdown selection and live link both reflect Alias Pro. Bonus: re-plug Alias Pro into a different USB port (which renames the node). Within ~3 seconds, the product-name fallback re-attaches.
- **Issue 3:** Set `SteelSeries_Mic` to 70% via system sound settings. Quit app. Relaunch. `pactl get-source-volume SteelSeries_Mic` returns 70%. Repeat with `SteelSeries_Music` set to 35% via our mixer slider — restart — confirm 35%. Then `pactl set-sink-volume SteelSeries_Aux 90%` — wait 3 seconds — quit — relaunch — confirm 90%.
- **Issue 4:** Open Home tab — confirm Clips card is row 1 (below Status + Device), full-width, contains indicator + Save button + Pause button + duration + hotkey labels. Click "Save Clip Now" during a game session — clip saved (toast appears). Click "Pause Recording" — indicator updates, GSR child is killed, buffer is gone (`ls ~/Videos/Clips/.arctis/`). Click "Resume Recording" — GSR restarts and re-arms. All UI strings say "recording" rather than "buffering" (verify via the enumerated file list).
- **Hotkey display:** Change the hotkey via portal → confirm Clips section's hotkey label updates after settings save.

## Risks

- **Write storm.** A rapidly-bouncing sink-input could trigger many small file writes. *Mitigation:* poll is 2 Hz; only writes on actual change vs. cache; modern filesystems coalesce. Not a real risk on this hardware.
- **Slider/poll race.** User drags slider → mixer.rs writes save_volume_entry → poll reads slightly stale volume → re-writes. *Mitigation:* `last_saved_volumes` cache is the source of truth for "did we already save?" — both paths update the cache.
- **WirePlumber restores volume before our `set_source_volume`.** *Mitigation:* explicit `set_source_volume` after `VirtualSinks::create` wins in practice (it's the last write). If not, periodic poll catches up within 2 seconds.
- **Pause/Resume race with supervisor restart.** *Mitigation:* `restart_attempts.clear()` is the first action in both Pause and Resume command arms.
- **Terminology change miss.** *Mitigation:* enumerated file list + line-by-line audit; verified via word-bounded grep listed in "Terminology pass."
- **`save_hotkey_display` drifts from actual binding.** If the user re-binds via the portal's own UI outside our wizard, the displayed chord stays stale until they next open Settings → Clips. *Mitigation in scope:* the `bind_shortcuts → list_shortcuts → persist trigger_description` flow runs every time the user re-binds via *our* wizard or settings flow. *Deferred polish:* subscribe to the portal's `ShortcutsChanged` signal on the live `GlobalShortcuts` session (already held open by `run_global_shortcuts` in `src/clips/hotkey.rs`) and refresh on signal. Not in Bundle B's mandatory scope but cheap to add later.
- **Product-name match collisions** (two devices with the same product name simultaneously online). First match wins; users with two physical mics of the same model will have to disambiguate manually. *Mitigation:* documented limitation; out of scope to dedupe via serial.
- **Brittle app_name persistence key** (existing issue, not introduced here). Some apps don't set `application.name`; browsers report the same name across tabs; Discord changes its name between voice and notification streams. *Mitigation:* none beyond what already ships; the new monotonic rule reduces the blast radius (we don't aggressively destroy entries on transient observations).

## Implementer A scope (Audio/Persistence Bundle — Issues 1+2+3)

Files (read-write):

- `src/audio/persistence.rs` — port volume helpers from main (`VOLUMES_FILE`, `volumes_path`, `load_volumes`, `save_volume_entry`); add `initial_tracked`, `update_saved_assignment`, rename `restore_new_streams` → `reconcile_stream_state` with new HashMap signature; delete the now-unused `initial_seen_ids` helper; remove the `assignments.remove` on unmanaged-sink path inside `save_assignments` to align with the new monotonic rule; extend mixer-routing reader/writer for the 5-field mic line via a dedicated `load_mixer_routing_mic() -> Option<MicPreference>` / `save_mixer_routing_mic(&MicPreference)` pair (homogeneous channel→sink map stays 2-field).
- `src/audio/router.rs` — add `current_mic_source: Option<String>` + `preferred_mic: Option<(String, String)>`; add `check_mic_hotplug` method; cache preferred-mic at construction.
- `src/audio/sinks.rs` — extend `list_physical_sources` to also return the stable-id chain (`device.product.name` / `device.bus_path` / `api.bluez5.address`); extend `parse_device_list` to traverse the Properties block.
- `src/audio/state_sync.rs` — **NEW**. Single public `pub fn tick(...)` that orchestrates the four jobs.
- `src/app.rs` — **three regions only**: (1) the state-sync timer block around the existing `restore_new_streams` watcher (where `glib::timeout_add_seconds_local(STREAM_WATCH_SECS, ...)` lives), (2) the `init_pipeline` function — add the ported apply-volumes loop after `VirtualSinks::create()` and before `AudioRouter::create()`, (3) the `connect_shutdown` handler — add the volume capture before `drop(res)`. No other `app.rs` edits.
- `src/mixer.rs` — port volume save-on-change block from main (slider's `connect_value_changed` writing through `save_volume_entry`); update the 5 destructuring sites for `list_physical_sources` to consume the new 5-tuple shape (`(name, desc, _, _, _)` for non-mic; pass through the chain for the mic dropdown).

Files (read-only, for context): `src/audio/sinks.rs`'s `ALL_SINKS`/`ALL_SOURCES`, the existing `restore_assignments` flow, the main branch's persistence.rs for the volume helpers to port.

## Implementer B scope (Clips UI Bundle — Issue 4)

Files (read-write):

- `src/window.rs` — remove `clips_row` from `build_status_card` + `StatusResult`; add `build_clips_section`; attach as grid row 1 spanning 2 columns; update `Widgets` + the dashboard SizeGroup wiring; rebuild the on-state-change refresh path so `set_state` updates the new section's widgets.
- `src/clips/mod.rs` — add `ClipCommand::PauseRecording`, `ClipCommand::ResumeRecording`.
- `src/clips/buffer.rs` — add `user_paused: bool` field; add `BufferState::Paused`; add `pause()` / `resume()` methods; update arm-gating logic to honor `user_paused`.
- `src/clips/backend.rs` — add match arms for the two new commands; clear `restart_attempts` and call `disarm` on Pause; trigger normal arm path on Resume.
- `src/clips/indicator.rs` — terminology pass on user-visible strings; possibly add a "Paused" state visual.
- `src/clips/settings.rs` — add `save_hotkey_display` field with load/save support; terminology pass on user-visible strings.
- `src/clips/notifications.rs` — terminology pass on user-visible strings.
- `src/app.rs` — register one new GAction: `app.pause-recording-toggle`. Place adjacent to the existing `app.save-clip` / `app.retry-clip-capture` actions (around the `app.add_action_entries(save_action)` block). **No other `app.rs` edits.** Implementer A's state-sync block is in a separate region of the file.
- `src/clips/hotkey.rs` — after `proxy.bind_shortcuts(...).await` returns Ok, call `proxy.list_shortcuts(&session).await`, find the shortcut whose `id == "save-clip"`, extract its `trigger_description` (the portal's own human-readable chord label), and persist into `ClipSettings.save_hotkey_display`. If the call fails, leave the field unchanged. See `ashpd::desktop::global_shortcuts::Shortcut::trigger_description` (returns `String`). This is the only `hotkey.rs` change.
- `src/clips/browser.rs` — terminology pass on any user-visible strings (audit by reading; most strings refer to saved clips, not the buffer, so the audit is expected to be short).

Files (read-only, for context): GSR docs, `state_sync.rs` (do not edit), `audio/persistence.rs` (do not edit beyond what Implementer A merges).

## Coordination point: `src/app.rs`

The single shared file between Implementers A and B is `src/app.rs`. Three edit regions exist for Implementer A and one for Implementer B. None overlap:

- **Implementer A region 1:** the existing `restore_new_streams` watcher block (around `glib::timeout_add_seconds_local(STREAM_WATCH_SECS, ...)` — approximately `app.rs:1392`).
- **Implementer A region 2:** `init_pipeline` function — add the apply-volumes loop after `VirtualSinks::create()` (approximately `app.rs:1502–1517`).
- **Implementer A region 3:** `connect_shutdown` handler — add the volume capture before `drop(res)` (approximately `app.rs:1464`).
- **Implementer B region 1:** the action-registration cluster — add the new `app.pause-recording-toggle` GAction adjacent to the existing `app.save-clip` block (approximately `app.rs:715–788`).

These four regions are mutually disjoint and at least ~200 lines apart in the current file. A merge of two diffs touching non-adjacent regions is mechanical. If the team-lead orchestrating the merge sees a conflict marker, the resolution is: take both edits verbatim — neither implementer may touch the other's regions.

## Open questions

None at revision-2 spec time. All previously-open questions resolved:

- **Pause backend mechanism:** decided — disarm + user_paused flag, buffer lost on pause.
- **Hotkey hint data source:** decided — new `save_hotkey_display` persisted field, written at bind time.
- **Mic hotplug stable identifier:** decided — `device.product.name` with fallback to exact node-name.
- **Move-off-managed rule:** decided — monotonic (don't destroy entries on transient moves).
- **First-tick seeding:** decided — `persistence::initial_tracked` populates from live sink-inputs.
- **`restore_new_streams` rename:** decided — `reconcile_stream_state`.
- **`save_assignments` at shutdown:** decided — keep with documented rationale.
- **`app.rs` ownership:** decided — line-region carve-up; `state_sync.rs` extraction reduces the shared surface.
- **`backend.rs` in Bundle B:** decided — explicitly included.
- **Pause button state machine:** tabulated above.
- **Score rubric:** defined in "Score-gate rubric" section.

## Teammate involvement

Per the user's directive on this work item:

1. **Spec phase (this document).** After commit:
   - `qa-code-auditor` reviews for technical soundness, completeness, idiomatic style, implementation guidance; returns numeric score 0–100 plus narrative against the rubric.
   - `devils-advocate-critic` reviews adversarially; returns numeric score 0–100 plus narrative against the rubric.
   - Run **in parallel**.
   - If either score < 90, fix and re-review (this loop).
   - Once both ≥ 90, the spec is auto-approved.
2. **Plan phase.** After auto-approval:
   - Main session writes the implementation plan using `superpowers:writing-plans`.
   - Plan inherits this spec's parallel-implementer split (2 implementers, Audio + Clips UI).
   - Plan is reviewed by `qa-code-auditor` + `devils-advocate-critic` in parallel; same 90% gate.
3. **Implementation phase.** After plan auto-approval:
   - Two `plan-implementer` instances dispatched **in parallel** in **isolated git worktrees** off `clipping-system`.
   - Each implementer returns its report to **`qa-code-auditor`** (the team-lead is the conduit but does not act on the reports until the QA synthesis is done).
   - `qa-code-auditor` synthesizes both reports into one comprehensive QA report; same 90% gate.
   - `devils-advocate-critic` writes the final adversarial report on the synthesized output; same 90% gate.
   - Final report returns to the team-lead.
4. **Verification phase.** `project-tester` runs the build + manual verifications outlined in the Testing section.
5. **`security-audit-sentinel`** — skipped. No auth, secrets, or public-endpoint surface in this work; only local PipeWire calls and `pactl`. Trivial change in attack surface relative to what already ships.
6. **`research-bot`** — only on-demand. Use context7 as the primary source for any doc lookup; only escalate to research-bot if context7 lacks the doc set needed.
