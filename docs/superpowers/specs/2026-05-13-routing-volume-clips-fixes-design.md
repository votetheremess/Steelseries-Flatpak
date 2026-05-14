# Routing / Volume / Clips fixes — design

**Date:** 2026-05-13
**Topic:** Four bug + UX fixes — app→sink persistence, mic hotplug, virtual-source volume persistence, Clips home-page section relocation/expansion
**Status:** Draft, awaiting QA + critic review
**Target branch:** `clipping-system` (worktree `/var/home/admin/Documents/Code/SteelseriesFlatpak-clipping/`)
**Authoritative bug list:** `knownbugs.txt` (issues 1–3) + new request (issue 4)

## Context

Four issues surfaced during live testing on the `clipping-system` branch. Three are audio-routing / persistence bugs reachable on both `main` and `clipping-system`; the fourth is a UI polish + expansion request for the clip status indicator on the home page. Per user direction all four land together on `clipping-system`.

### The four issues, as the user described them

1. **Tidal's sink assignment doesn't persist.** Open Tidal → it lands on `SteelSeries_Chat`. Move it to `SteelSeries_Music` via system sound settings. Close Tidal, reopen Tidal → it goes back to `SteelSeries_Chat`. Repeats forever.
2. **Mic source doesn't auto-switch on hotplug.** App starts while preferred mic (e.g. Alias Pro) is powered off → app falls back to whatever is available. Turn the preferred mic on later → app does not detect or switch. User has to manually re-select from the dropdown every time.
3. **`SteelSeries_Mic` (and likely `_Music` / `_Aux`) volume doesn't persist correctly.** User sets `SteelSeries_Mic` to 100% via system sound settings. Closes the app. Reopens. Volume comes up as something other than 100% — and the value is inconsistent across launches (47%, 83%, etc.).
4. **Clip status indicator placement is wrong + needs more controls.** Today the indicator sits as a third row inside the Status card, crowding it. User wants it relocated to its own full-width section below Status + Device, and expanded to include: "Save Clip Now" button, recording duration display, current hotkey hint, and a pause / resume toggle. Plus a terminology pass — user-visible strings should say "recording" rather than "buffering".

### Why these have proven hard

Issues 1 and 3 share a structural root cause: **we only capture user-state at app shutdown.** `persistence::save_assignments` runs in `app.connect_shutdown`. The volume save in `mixer.rs` runs on slider change but never observes external (system-sound-settings) changes. When the user changes state externally and then doesn't trigger our shutdown — or when a watched object (a sink-input) disappears before our shutdown — the saved state is stale.

Issue 2 is similar in spirit: the mic source is chosen once during `AudioRouter::create` and never reconsidered. PipeWire's source list is dynamic; our model is static.

Issue 4 is a layout + content change, plus a domain-terminology change, plus one new ClipCommand variant (pause/resume). Mechanically independent of 1–3 except for one possible touchpoint in `app.rs` to wire a new command.

## Goals

- **Saved app→sink assignments always reflect the user's current routing,** whether the user moved a stream via system sound settings, via this app's future UI, or any other means.
- **The preferred mic source auto-attaches as soon as it appears,** without any manual user action, as long as the user has expressed a preference via the mixer dropdown previously.
- **Virtual sink/source volumes survive app restarts**, including when the user changed them externally (system sound settings, `pactl`, etc.).
- **Clips section on the home page is its own card,** full-width across the dashboard, with an expanded set of controls; user-visible language is "recording" rather than "buffering" throughout the clip UI.

## Non-goals

- Reusing existing virtual sinks across launches instead of destroy/recreate. The current architecture deliberately destroys and recreates for clean state on startup. Issue 3 is solved by saving the right values, not by avoiding the recreate.
- Game and Chat volume persistence. These remain HID-dial-owned by design and are out of scope for the volume save logic.
- Master (physical headset) volume persistence. WirePlumber owns this natively; we do not duplicate.
- Per-app routing through some new GUI surface. Routing remains exclusively system-sound-settings driven; we just track it more reliably.
- Renaming internal types (`BufferController`, `buffer.rs`, etc.). Only user-visible strings change for the "recording" terminology pass.
- Auto-detect / auto-switch any non-preferred mic. Only the user's explicit saved preference triggers a switch on hotplug.

## Architecture

### Issues 1–3: one unified state-sync tick

Three of the four issues are different facets of the same structural gap: we don't observe-and-persist state continuously, only at startup and shutdown. The fix is to **extend the existing 2-second sink-input watcher in `app.rs`** (today it only calls `persistence::restore_new_streams`) into a broader "state sync tick" that does four things per cycle:

1. **Auto-route new streams** — existing behavior, unchanged: any new sink-input gets routed to its saved sink.
2. **Detect user-initiated stream moves** (Issue 1) — track `HashMap<u32, String>` mapping sink-input-id → last-known sink-name. If a stream's sink changes between polls and the new sink is one of ours, update the saved assignment. If it moves *off* a managed sink, drop the saved entry.
3. **Mic hotplug check** (Issue 2) — read the saved preferred mic from `mixer_routing.txt`. If it's set, differs from the currently linked mic, and appears in `list_physical_sources()`, call `router.reroute_mic(saved)`.
4. **Virtual sink/source volume capture** (Issue 3) — query `get_sink_volume` / `get_source_volume` for `SteelSeries_Music`, `SteelSeries_Aux`, `SteelSeries_Mic`. If the value differs from what's recorded in memory since the last write, call `persistence::save_volume_entry`.

All four pieces share one `glib::timeout_add_local` at 2 seconds. No new threads, no new MPSC channels, no new IPC. The state-tracking maps live as `Rc<RefCell<...>>` captured by the closure.

#### Shutdown capture (Issue 3, finale)

In `app.connect_shutdown`, just before dropping `AppResources` (which destroys the sinks via `VirtualSinks::drop`), iterate `[MUSIC_SINK_NAME, AUX_SINK_NAME, MIC_SOURCE_NAME]`, query each volume, and write to `volumes.txt`. This catches the user's final state at quit time — important because the periodic poll only fires every 2 seconds, and a fast change-then-quit could miss the periodic window.

### Issue 4: Clips section relocation + expansion + terminology

#### Layout change in `src/window.rs`

- Remove the clip indicator from `build_status_card()`. Drop `clips_row` and `clips_indicator` fields from `StatusResult`. Drop the `clips_row` line from the row-height `SizeGroup`.
- Add a new `build_clips_section() -> (adw::PreferencesGroup, ClipsSectionWidgets)` returning a styled card titled **"Clips"** (per user choice).
- Attach the new section to the dashboard grid as row 1 spanning both columns: `grid.attach(&clips_section, 0, 1, 2, 1)`.
- `Widgets.clips_indicator` continues to live on the top-level `Widgets` struct but now sources from the new section.

#### Contents of the Clips section

The card contains, in order:

1. **Status indicator badge** — the existing `crate::clips::StatusIndicator` widget, unchanged in appearance (colored dot + game name).
2. **"Save Clip Now" button** — primary button (`add_css_class("suggested-action")`) that sends the existing `ClipCommand::SaveClip` over the channel. Identical effect to the hotkey path.
3. **Recording-duration label** — text reading e.g. "Recording last 5 minutes". Pulled from `ClipSettings.buffer_duration_seconds` (or whatever the equivalent field is named in the existing settings) and rendered via a humanize helper. Re-renders when settings change.
4. **Hotkey hint label** — text e.g. "Hotkey: ALT+S". Pulled from `ClipSettings.save_clip_binding` (or equivalent). Re-renders when settings change.
5. **Pause / Resume toggle** — a `gtk::Button` that flips between "Pause Recording" and "Resume Recording" depending on the buffer state. Sends a new `ClipCommand` variant (named below).

Layout: vertical `gtk::Box`; the indicator sits at the top, the action button + duration + hotkey + pause toggle form a horizontal row underneath, with the action button left-aligned and the secondary info right-aligned.

#### New `ClipCommand` variant

Add a single new variant to the enum in `src/clips/mod.rs`:

```rust
ClipCommand::SetRecordingEnabled(bool),  // true = resume, false = pause
```

Single setter (not two separate Pause / Resume variants) keeps the toggle simple and lets the implementer use one match arm in the backend supervisor. The supervisor (`backend.rs`) interprets `false` by sending SIGSTOP-equivalent (or the appropriate GSR-level mechanism — pause may map to `flatpak kill` + restart with the same args under the existing supervisor; the implementer chooses the cleanest path). On `true`, the supervisor re-arms the buffer.

The pause/resume implementation must reuse the existing supervisor's restart-loop infrastructure (rolling-window deque, etc.) rather than ad-hoc process management.

#### Terminology pass

Anywhere in user-visible strings the word "buffer" or "buffering" appears in the clip UI, replace with "recording". Survey points:

- `src/clips/indicator.rs` — state labels ("Buffering" → "Recording")
- `src/clips/settings.rs` — preference labels, descriptions, tooltips
- `src/clips/mod.rs` — any user-facing string constants
- `src/clips/notifications.rs` — toast text
- `src/window.rs` — labels added in this work

Internal type names (`BufferController`, `buffer.rs`, etc.) and code comments stay as-is. The renaming is a UX/copy change, not a refactor.

## Detailed designs

### Issue 1 — User-move detection

Today, `persistence::restore_new_streams(seen_ids: &mut HashSet<u32>)` only acts on streams whose IDs it hasn't seen. The "seen" semantics are wrong for the new requirement — we want to keep observing streams forever, not just on first appearance.

**Approach:** change the signature from `seen_ids: &mut HashSet<u32>` to `tracked: &mut HashMap<u32, String>` where the value is the last-known sink name. Each poll:

```text
for input in list_sink_inputs():
    let current_sink = sinks_by_id[input.sink_id];

    match tracked.entry(input.id):
        Vacant => {
            // New stream — preserve the existing auto-route behavior
            if input.app_name == "pw-cli": skip
            if let Some(target) = saved.get(&input.app_name):
                if current_sink != target: move_sink_input(input.id, target)
                final_sink = target.clone()
            else:
                final_sink = current_sink.to_string()
            tracked.insert(input.id, final_sink)
        }
        Occupied(entry) => {
            // Existing stream — detect a user move
            if entry.get() != current_sink:
                if is_managed(current_sink):
                    update_saved_assignment(input.app_name, current_sink)
                else:
                    remove_saved_assignment(input.app_name)
                *entry.get_mut() = current_sink.to_string()
        }
```

Garbage collection: at the end of each poll, drop entries from `tracked` whose IDs no longer appear in `list_sink_inputs()`. Necessary so the map doesn't grow unbounded across long sessions.

Update helpers `update_saved_assignment(app, sink)` and `remove_saved_assignment(app)` work on the existing `assignments.txt` file via the same read-modify-write pattern as `save_mixer_routing_entry`. They write single entries, not the whole file each time.

**Why not poll-all and write-all every tick:** writes are cheap but produce noisy filesystem activity. Single-entry updates are clearer and avoid clobbering concurrent writes from `save_assignments` at shutdown. (We keep `save_assignments` for the case where the user changes state and quits before the next 2-second tick.)

### Issue 2 — Mic hotplug detection

Add to `AudioRouter`:

```rust
pub struct AudioRouter {
    eq_pipeline: EqPipeline,
    mic_linked: bool,
    current_mic_source: Option<String>,  // NEW
}
```

`current_mic_source` is set after every successful `link_mic_source` call (both in `create` and in `reroute_mic`). Public accessor `pub fn current_mic_source(&self) -> Option<&str>`.

New method on `AudioRouter`:

```rust
pub fn check_mic_hotplug(&mut self) {
    let saved = persistence::load_mixer_routing().get("mic").cloned();
    let Some(saved_source) = saved else { return };  // No preference recorded
    if self.current_mic_source.as_deref() == Some(saved_source.as_str()) {
        return;  // Already on it
    }
    let available = sinks::list_physical_sources();
    if !available.iter().any(|(name, _)| name == &saved_source) {
        return;  // Saved device isn't online
    }
    log::info!("Mic hotplug: switching to saved preference {saved_source}");
    self.reroute_mic(&saved_source);
}
```

Called from the state-sync tick.

**Why only on explicit preference:** if the user has never used the dropdown, there's no `"mic"` entry in `mixer_routing.txt`. We do not auto-save the startup-fallback choice, so this method early-returns. New users see no surprising behavior.

**Edge cases:**

- *User unplugs the preferred mic mid-session.* The link to the preferred device breaks at the PipeWire layer; we continue running with a broken link until the next poll observes the preferred device is gone — but our hotplug check doesn't currently auto-fall-back. In v1, the user manually picks a fallback via the dropdown; their pick updates the saved preference, so when the preferred device comes back, we'll obediently switch back to it (per user's explicit choice). Document this in code-comments.
- *User picks fallback mic Y while preferred mic X is offline; later X comes online.* Saved preference is now Y, not X. No switch. Correct.
- *Two preferred-mic-class devices online at once with the same name.* Out of scope; PipeWire's node-name uniqueness handles it at a lower layer.

### Issue 3 — Volume persistence

#### Periodic capture (in the state-sync tick)

Maintain `Rc<RefCell<HashMap<String, u32>>>` `last_saved_volumes`, initially populated from `persistence::load_volumes()`. Each tick:

```text
for name in [MUSIC_SINK_NAME, AUX_SINK_NAME]:
    if let Ok(vol) = sinks::get_sink_volume(name):
        if last_saved_volumes[name] != vol:
            persistence::save_volume_entry(name, vol)
            last_saved_volumes[name] = vol

if let Ok(vol) = sinks::get_source_volume(MIC_SOURCE_NAME):
    if last_saved_volumes[MIC_SOURCE_NAME] != vol:
        persistence::save_volume_entry(MIC_SOURCE_NAME, vol)
        last_saved_volumes[MIC_SOURCE_NAME] = vol
```

#### Shutdown capture

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

This runs unconditionally, even if the periodic capture already saw a recent change. Saving the same value twice is harmless.

#### Mixer-slider save retained

The existing `mixer.rs` slider-change handler that calls `save_volume_entry` stays. It's still the lowest-latency path when the user moves the slider in our UI; the periodic capture catches everything else.

#### Apply-on-startup ordering

`load_volumes()` runs in `init_pipeline` after `VirtualSinks::create()`. This already works. The only addition is that the saved file will now actually contain useful values that reflect external changes.

#### Why the user observes "random" values today

Most likely: WirePlumber's per-node state cache restores some value when our recreated `SteelSeries_Mic` appears with the same node name as the destroyed one. The restored value isn't truly random — it's whatever WirePlumber last cached. With the new capture-and-restore round-tripping the user's actual intended value, our explicit `set_source_volume` after `load_volumes` wins, and the "randomness" goes away.

If WirePlumber's restored value still wins for any reason, the periodic poll picks up the discrepancy within 2 seconds and immediately re-applies the saved value — but in practice, our `set_source_volume` call in `init_pipeline` is the last write and PipeWire honors it.

### Issue 4 — Clips section relocation + expansion + terminology

Already covered in **Architecture → Issue 4** above. No additional detailed-design notes needed beyond:

- Resize the dashboard `gtk::Grid` if needed to fit row 1 cleanly. The existing grid has `column_homogeneous(true)` so the section's `attach(..., col=0, row=1, w=2, h=1)` naturally spans both columns.
- The new section must participate in the dashboard's `adw::Clamp` so its width respects the dashboard max (1125 px today).
- Update `Widgets` struct in `window.rs` to carry the new section's widgets (button refs, label refs, pause-state ref) so external code can refresh them on `ClipSettings` change and on buffer state change.

## Data files / config impact

- `assignments.txt` — file shape unchanged. Will be written more frequently (per-tick, per-change, plus at shutdown). Atomic single-entry helpers wrap a `read_saved → modify → write` cycle, same pattern as `save_mixer_routing_entry`.
- `mixer_routing.txt` — unchanged.
- `volumes.txt` — unchanged file shape. Will reliably contain the right values.
- `eq_state.txt`, `eq_presets/*.txt`, autostart `.desktop` file — unchanged.

## Threading

No new threads. No new mpsc channels. All four issues live inside the existing GTK-main-thread polling tick. The `AudioRouter::check_mic_hotplug` method is called on the main thread; it shells out to `pactl` and `pw-link` synchronously like the rest of the audio code does today.

## Error handling

- `pactl get-*-volume` failures: log at `warn` level, skip this tick's capture for the failing channel, retry next tick. Don't pollute `volumes.txt` with a fallback.
- Hotplug check failures (e.g. `pw-link` errors during reroute): the existing `reroute_mic` already logs and persists user intent regardless of link success. Same applies in the hotplug path.
- User-move detection on a stream that was created and destroyed within 2 seconds: invisible to us. Acceptable — short-lived streams (notification sounds, e.g.) are not what the user wants persisted anyway.
- Filesystem write failures on `save_*_entry`: log at `warn` level, do not retry. The next change triggers another write attempt.

## Testing

### Existing tests stay green

- All 40 existing unit tests must continue passing.
- The 9 HID protocol tests and 10 biquad tests are unaffected by these changes; sanity check after merge.

### New unit tests

- `persistence.rs`:
  - `tracked_map_user_move_updates_saved` — simulate a stream's sink changing from managed-A to managed-B; the saved file gains/updates the entry to B.
  - `tracked_map_user_move_to_unmanaged_removes_saved` — stream moves to an unmanaged sink; saved entry is dropped.
  - `tracked_map_garbage_collects_dead_ids` — after a poll where a tracked stream no longer appears in `list_sink_inputs`, its entry is removed from the map.
  - `update_saved_assignment_preserves_other_entries` — single-entry write doesn't clobber unrelated entries.
- `router.rs`:
  - `check_mic_hotplug_noop_when_no_saved_preference` — empty `mixer_routing.txt` → no reroute call.
  - `check_mic_hotplug_noop_when_already_on_saved` — `current_mic_source == saved` → no reroute call.
  - `check_mic_hotplug_switches_when_saved_becomes_available` — saved == X, current == Y, X appears in `list_physical_sources` → reroute called with X.

These mock `list_sink_inputs` / `list_physical_sources` via a thin trait or test seam — implementer chooses.

### Manual verification

After implementer reports, project-tester runs the build + targeted verifications:

- Issue 1: launch Tidal (or `paplay --device=SteelSeries_Music /usr/share/sounds/...`-style stand-in), observe initial routing, move via system sound settings, kill the stream, relaunch, confirm new routing persists. Repeat with the move going *off* a managed sink to confirm forgetting.
- Issue 2: turn off Alias Pro, launch app, observe fallback. Turn on Alias Pro. Within ~3 seconds (one 2-second tick + slack), mixer dropdown selection and active link reflect Alias Pro.
- Issue 3: set `SteelSeries_Mic` to 70% via system sound settings. Quit app. Relaunch. Volume is 70%. Set `SteelSeries_Music` to 35% via our mixer slider — restart — confirm 35%. Set `SteelSeries_Aux` to 90% via `pactl set-sink-volume SteelSeries_Aux 90%` — wait 3 seconds — quit — relaunch — confirm 90%.
- Issue 4: open Home tab — confirm Clips card is below Status + Device, full-width, contains indicator + Save button + duration + hotkey + pause toggle. Click Save Clip Now during a game session — clip saved. Click Pause — indicator reflects paused state. Click Resume. All UI strings say "recording".

## Risks

- **Risk: write storm.** If a sink-input rapidly bounces between sinks (some apps do this on startup), the per-tick `save_volume_entry` could write the assignments file dozens of times per minute. **Mitigation:** the poll runs at 2 Hz and only writes on actual change; modern filesystems coalesce trivially. Not a real risk on the actual hardware.
- **Risk: race between mixer slider and periodic poll.** User drags slider → mixer.rs writes save → periodic poll reads slightly stale volume → re-writes the slightly stale value. **Mitigation:** the `last_saved_volumes` cache is the source of truth for "did we already save this value?" — both the slider write and the periodic-poll write update it. Slider handler updates the cache after `save_volume_entry`.
- **Risk: WirePlumber restores volume before our `set_source_volume`.** As noted in Issue 3 detailed design. **Mitigation:** explicit `set_source_volume` call after `VirtualSinks::create` already wins in practice. If it doesn't, the periodic poll catches up within 2 seconds.
- **Risk: Pause/Resume implementation interacts badly with the existing supervisor's rolling-window restart limiter.** The supervisor counts restarts to detect crash-loops; deliberately pausing/resuming the recorder shouldn't trip that. **Mitigation:** implementer must add a `paused` state to the supervisor that suppresses restart counting; this is a small state-machine extension, not a rewrite. Detailed in plan.
- **Risk: Terminology change misses one of the user-visible string sites.** A leftover "buffering" label looks unprofessional. **Mitigation:** include an explicit grep checklist in the implementation plan and verify post-implementation via `grep -rni 'buffer' src/clips/ src/window.rs | grep -vi 'buffercontroller\|buffer.rs'` to confirm only internal references remain.

## Open questions

None at spec write time. All user-preference questions have been resolved during brainstorming. Implementer-time decisions (e.g. exact GTK widget layout in the new Clips card, exact ClipCommand integration with the existing supervisor for pause) are technical choices left to the implementer.

## Teammate involvement

Per the user's directive on this work item, the workflow is:

1. **Spec phase (this document).** After commit:
   - `qa-code-auditor` reviews for technical soundness, completeness, idiomatic style; returns numeric score 0–100 plus narrative.
   - `devils-advocate-critic` reviews adversarially; returns numeric score 0–100 plus narrative.
   - Run in **parallel**.
   - If either score < 90, fix the spec and re-review.
   - Once both ≥ 90, the spec is auto-approved.
2. **Plan phase.** After auto-approval:
   - Main session writes the implementation plan using `superpowers:writing-plans`.
   - Plan determines the parallel-implementer split (per architecture, 2 implementers in 2 worktrees — see below).
   - Plan is reviewed by `qa-code-auditor` + `devils-advocate-critic` in parallel; same 90% gate.
3. **Implementation phase.** After plan auto-approval:
   - Two `plan-implementer` instances dispatched **in parallel** in **isolated git worktrees** off `clipping-system`:
     - Implementer A — Audio/Persistence Bundle (Issues 1+2+3). Files: `src/audio/persistence.rs`, `src/audio/router.rs`, `src/audio/sinks.rs`, `src/app.rs`.
     - Implementer B — Clips UI Bundle (Issue 4). Files: `src/window.rs`, `src/clips/mod.rs`, `src/clips/indicator.rs`, `src/clips/settings.rs`, `src/clips/buffer.rs`, `src/clips/notifications.rs`, plus the `app.rs` line that registers any new `ClipCommand` variant (single touchpoint to coordinate at merge time).
   - Each implementer returns its report to **`qa-code-auditor`** (not back to the team lead).
   - `qa-code-auditor` synthesizes both reports into one comprehensive QA report; same 90% gate.
   - `devils-advocate-critic` writes the final adversarial report on the synthesized output; same 90% gate.
   - Final report comes back to the team lead.
4. **Verification phase.** `project-tester` runs the build + manual verifications outlined in the Testing section.
5. **`security-audit-sentinel`** — skipped. No auth, secrets, or public-endpoint surface in this work; only local PipeWire calls and `pactl`. Trivial change in attack surface relative to what already ships.
6. **`research-bot`** — only on-demand. If the implementer hits an unknown around GSR pause semantics, ashpd, or PipeWire-volume-restore behavior, escalate via research-bot using context7 as the primary source.
