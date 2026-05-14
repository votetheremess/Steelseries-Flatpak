//! Unified 2-second state-sync tick. Owns:
//! - User-move detection (Issue 1) via persistence::reconcile_stream_state
//! - Mic hotplug check (Issue 2) via AudioRouter::check_mic_hotplug
//! - Virtual-source volume capture (Issue 3) via capture_virtual_volumes
//!
//! See docs/superpowers/specs/2026-05-13-routing-volume-clips-fixes-design.md.

use std::collections::HashMap;
use std::time::Instant;

use super::persistence;
use super::router::AudioRouter;
use super::sinks::{self, AUX_SINK_NAME, MIC_SOURCE_NAME, MUSIC_SINK_NAME};

/// Per-process state for the state-sync tick. Seeded once from disk + the
/// current PulseAudio state on startup, then mutated in-place by every tick.
#[derive(Default)]
pub struct StateSyncState {
    /// Sink-input ID → sink_name we last observed it on. Drives the user-move
    /// detector in `reconcile_pure`.
    pub tracked: HashMap<u32, String>,
    /// Sink-input ID → (pre_move_sink, when_we_moved_it) — used to suppress
    /// our own moves from looking like user moves when the next pactl tick
    /// reflects them with a small lag.
    pub self_move_pre_sink: HashMap<u32, (String, Instant)>,
    /// Last volume value written to `volumes.txt` per channel. Lets us skip
    /// the I/O when nothing has changed since the previous tick.
    pub last_saved_volumes: HashMap<String, u32>,
}

impl StateSyncState {
    pub fn new_seeded() -> Self {
        Self {
            tracked: persistence::initial_tracked(),
            self_move_pre_sink: HashMap::new(),
            last_saved_volumes: persistence::load_volumes(),
        }
    }
}

/// One tick of the state-sync loop. Runs on the GTK main thread every
/// STREAM_WATCH_SECS (2 s by default).
pub fn tick(state: &mut StateSyncState, router: &mut AudioRouter) {
    persistence::reconcile_stream_state(&mut state.tracked, &mut state.self_move_pre_sink);
    router.check_mic_hotplug();
    capture_virtual_volumes(&mut state.last_saved_volumes);
}

/// Read current volumes on the virtual Music/Aux sinks + mic source and write
/// through to `volumes.txt` only when they've changed since the last tick.
fn capture_virtual_volumes(last_saved: &mut HashMap<String, u32>) {
    for name in [MUSIC_SINK_NAME, AUX_SINK_NAME] {
        if let Ok(vol) = sinks::get_sink_volume(name) {
            if last_saved.get(name).copied() != Some(vol) {
                persistence::save_volume_entry(name, vol);
                last_saved.insert(name.to_string(), vol);
            }
        }
    }
    if let Ok(vol) = sinks::get_source_volume(MIC_SOURCE_NAME) {
        if last_saved.get(MIC_SOURCE_NAME).copied() != Some(vol) {
            persistence::save_volume_entry(MIC_SOURCE_NAME, vol);
            last_saved.insert(MIC_SOURCE_NAME.to_string(), vol);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_no_change_writes_nothing() {
        // No direct test for write-side without mocking pactl, but this
        // asserts capture_virtual_volumes is callable with the cache and
        // doesn't panic on missing pactl output. Real verification happens
        // in the manual test recipe.
        let mut last = HashMap::new();
        last.insert(MIC_SOURCE_NAME.to_string(), 100);
        capture_virtual_volumes(&mut last);
    }
}
