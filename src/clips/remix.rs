//! Per-track remix panel.
//!
//! Builds the UI shown when the user picks "Remix" from a clip-card kebab
//! menu. Saved clips embed up to 6 audio tracks (track 0 is GSR's mix-down,
//! tracks 1–5 are per-source: Game / Chat / Music / Aux / Mic). The remix
//! panel exposes one row per track with a volume slider (in dB), a Mute
//! toggle, and a Solo toggle, plus a Preview / Export action bar.
//!
//! Slider semantics (informational — interpretation lives in the Export
//! pipeline, Task 7.4):
//!   * Volume slider reads in dB (-60..+6, default 0). The Export pipeline
//!     converts this to a linear gain via 10^(db/20) before passing to the
//!     ffmpeg `volume` filter. Muting forces 0; if any row is soloed, only
//!     soloed rows are audible (others are forced to 0).
//!
//! Preview / Export wiring lands in Tasks 7.3 / 7.4 — at the skeleton stage
//! the buttons are present but unhandled, and the on_exported callback
//! sits on the panel constructor for forward compatibility.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;

/// Display labels for each of the 6 audio tracks in a saved clip. Index 0
/// is the GSR-produced mix-down; indices 1..5 are the per-source tracks.
const TRACK_LABELS: [&str; 6] = ["Mix", "Game", "Chat", "Music", "Aux", "Mic"];

/// Slider lower bound in dB. Below this we clamp linear gain to 0 so the
/// filter graph doesn't produce numerical noise from a barely-audible scale.
const DB_MIN: f64 = -60.0;
/// Slider upper bound in dB. +6 dB is roughly 2x linear, enough headroom
/// for boosting a quiet voice track without driving the mix into clipping.
const DB_MAX: f64 = 6.0;
/// Threshold below which we treat a track as effectively muted in the
/// linear-gain conversion. -50 dB is ~0.003 linear — inaudible against
/// any other track and not worth the floating-point precision noise.
///
/// Used by `linear_gain_for` (consumed by Task 7.4 Export). Marked
/// dead-code-allowed at the skeleton stage so the panel-only commit
/// (Task 7.2) builds without warning churn.
#[allow(dead_code)]
const DB_SILENT_THRESHOLD: f64 = -50.0;

/// Per-track UI state, shared between the slider/toggle widgets and the
/// Export-button click handler so we can read the live mix at export time
/// without walking the widget tree.
#[derive(Debug, Clone, Copy, Default)]
struct TrackState {
    volume_db: f64,
    mute: bool,
    solo: bool,
}

/// Computed final linear gain for a single track, accounting for mute/solo
/// rules across the whole row set:
///   * If the row is muted, gain is 0.
///   * If any row is soloed and this row isn't, gain is 0.
///   * Otherwise the dB value is converted to linear via 10^(db/20),
///     clamped to 0 below DB_SILENT_THRESHOLD.
///
/// Skeleton-stage dead-code-allowed: the production caller is the Export
/// pipeline (Task 7.4); the function is exercised by tests in the
/// meantime.
#[allow(dead_code)]
fn linear_gain_for(states: &[TrackState; 6], idx: usize) -> f64 {
    let any_solo = states.iter().any(|s| s.solo);
    let s = states[idx];
    if s.mute {
        return 0.0;
    }
    if any_solo && !s.solo {
        return 0.0;
    }
    if s.volume_db <= DB_SILENT_THRESHOLD {
        return 0.0;
    }
    10f64.powf(s.volume_db / 20.0)
}

/// The remix panel.
///
/// `root` is the top-level box that callers append into the page-level
/// Stack's `remix` slot. `state` is the live per-track mix; signal handlers
/// inside the panel update it on every slider/toggle change, and the
/// Export pipeline (Task 7.4) reads from it on click.
pub struct RemixPanel {
    pub root: gtk::Box,
    /// Live mix state. Tasks 7.3 / 7.4 read from this on Preview / Export
    /// click; the field is kept on the public struct so future API
    /// extensions (e.g. preset save/load) can introspect or mutate the
    /// mix without going through the widget tree.
    #[allow(dead_code)]
    state: Rc<RefCell<[TrackState; 6]>>,
}

/// Build a fresh remix panel for `clip_path`. Returns the panel's root
/// widget (to be inserted into the parent Stack) plus the shared track
/// state.
///
/// Callbacks:
///   * `on_close` — fires when the user clicks the panel's Close button.
///     Caller should pop the stack back to the loaded grid.
///   * `on_exported` — fires after a successful Export (Task 7.4). Caller
///     should refresh the grid model so the new `*-remix.mp4` shows up.
///     Currently parked: at the skeleton stage the Export button is
///     unhandled, so the callback never runs. Kept on the API so the
///     Task 7.4 wiring doesn't require re-threading callsites.
pub fn build_remix_panel(
    clip_path: &Path,
    on_close: impl Fn() + 'static,
    on_exported: impl Fn(PathBuf) + 'static,
) -> RemixPanel {
    // The on_exported callback is wired up in Task 7.4; at the skeleton
    // stage the Export click is a no-op. Suppress the unused-variable
    // warning explicitly so the compiler doesn't need the `_` prefix
    // (which would change the public API name later).
    let _ = on_exported;
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(20)
        .margin_end(20)
        .build();

    let state: Rc<RefCell<[TrackState; 6]>> = Rc::new(RefCell::new([TrackState::default(); 6]));

    // ---- Header: title + Close button -----------------------------------
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    // The label builder takes `impl Into<GString>`. `Cow<'_, str>` doesn't
    // satisfy that bound on this glib version, so we materialize an owned
    // `String` first (this matches the same pattern used in
    // `clips::notifications`).
    let title_text: String = clip_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let title = gtk::Label::builder()
        .label(&title_text)
        .css_classes(["title-2"])
        .hexpand(true)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    header.append(&title);
    let close_btn = gtk::Button::builder().label("Close").build();
    close_btn.connect_clicked(move |_| on_close());
    header.append(&close_btn);
    root.append(&header);

    // ---- Per-track rows -------------------------------------------------
    for (idx, label) in TRACK_LABELS.iter().enumerate() {
        let row = build_track_row(label, idx, state.clone());
        root.append(&row);
    }

    // ---- Action bar -----------------------------------------------------
    // Buttons are present at the skeleton stage but unhandled. Tasks 7.3
    // and 7.4 wire them up.
    let action_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(12)
        .halign(gtk::Align::End)
        .build();
    let preview_btn = gtk::Button::builder().label("Preview").build();
    let export_btn = gtk::Button::builder()
        .label("Export")
        .css_classes(["suggested-action"])
        .build();
    action_bar.append(&preview_btn);
    action_bar.append(&export_btn);
    root.append(&action_bar);
    let _ = clip_path; // referenced by Tasks 7.3 / 7.4 wiring.

    RemixPanel { root, state }
}

/// Build one track row: label + volume scale + Mute toggle + Solo toggle.
/// Each widget's signal handler updates the shared `[TrackState; 6]` so
/// the Export-button click can read the live mix without re-querying the
/// widget hierarchy.
fn build_track_row(label: &str, idx: usize, state: Rc<RefCell<[TrackState; 6]>>) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    let lbl = gtk::Label::builder()
        .label(label)
        .width_request(80)
        .xalign(0.0)
        .build();
    row.append(&lbl);

    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, DB_MIN, DB_MAX, 0.5);
    scale.set_value(0.0);
    scale.set_hexpand(true);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Right);
    {
        let state_for_scale = state.clone();
        scale.connect_value_changed(move |s| {
            state_for_scale.borrow_mut()[idx].volume_db = s.value();
        });
    }
    row.append(&scale);

    let mute = gtk::ToggleButton::builder().label("Mute").build();
    {
        let state_for_mute = state.clone();
        mute.connect_toggled(move |btn| {
            state_for_mute.borrow_mut()[idx].mute = btn.is_active();
        });
    }
    row.append(&mute);

    let solo = gtk::ToggleButton::builder().label("Solo").build();
    {
        let state_for_solo = state.clone();
        solo.connect_toggled(move |btn| {
            state_for_solo.borrow_mut()[idx].solo = btn.is_active();
        });
    }
    row.append(&solo);

    row
}

/// Compute the remix output path: `<stem>-remix.mp4` in the same directory
/// as the input. `with_extension` and `with_file_name` get clobbered when
/// the original stem already contains dots (e.g. `cs2.2026-05-09.mp4`),
/// so we work directly with the file_stem.
///
/// Used by the Export pipeline (Task 7.4); kept here in the skeleton so
/// it's exercised by tests immediately rather than landing untested.
#[allow(dead_code)]
fn remix_output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".to_string());
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}-remix.mp4"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_gain_at_zero_db_is_one() {
        let states = [TrackState::default(); 6];
        for i in 0..6 {
            assert!((linear_gain_for(&states, i) - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn mute_forces_zero_gain() {
        let mut states = [TrackState::default(); 6];
        states[2].volume_db = 0.0;
        states[2].mute = true;
        assert_eq!(linear_gain_for(&states, 2), 0.0);
    }

    #[test]
    fn solo_silences_other_tracks() {
        let mut states = [TrackState::default(); 6];
        states[1].solo = true;
        // Soloed row plays at its dB value (default 0 → 1.0).
        assert!((linear_gain_for(&states, 1) - 1.0).abs() < 1e-9);
        // Non-soloed rows are forced to 0.
        for i in [0, 2, 3, 4, 5] {
            assert_eq!(linear_gain_for(&states, i), 0.0);
        }
    }

    #[test]
    fn below_silent_threshold_clamps_to_zero() {
        let mut states = [TrackState::default(); 6];
        states[0].volume_db = -60.0;
        assert_eq!(linear_gain_for(&states, 0), 0.0);
        states[0].volume_db = -55.0;
        assert_eq!(linear_gain_for(&states, 0), 0.0);
    }

    #[test]
    fn plus_six_db_is_roughly_two_linear() {
        let mut states = [TrackState::default(); 6];
        states[0].volume_db = 6.0;
        let g = linear_gain_for(&states, 0);
        // 10^(6/20) = 10^0.3 ≈ 1.995
        assert!((g - 1.995).abs() < 0.01, "expected ~1.995, got {g}");
    }

    #[test]
    fn mute_overrides_solo() {
        // If a track is both muted and soloed, mute wins (volume 0).
        let mut states = [TrackState::default(); 6];
        states[3].solo = true;
        states[3].mute = true;
        assert_eq!(linear_gain_for(&states, 3), 0.0);
    }

    #[test]
    fn remix_output_path_appends_remix_suffix() {
        let input = Path::new("/tmp/clips/match-1.mp4");
        let out = remix_output_path(input);
        assert_eq!(out, Path::new("/tmp/clips/match-1-remix.mp4"));
    }

    #[test]
    fn remix_output_path_preserves_dotted_stems() {
        // file_stem() strips only the last extension, so a name like
        // `cs2.2026-05-09.mp4` should become `cs2.2026-05-09-remix.mp4`,
        // not lose a segment.
        let input = Path::new("/tmp/clips/cs2.2026-05-09.mp4");
        let out = remix_output_path(input);
        assert_eq!(out, Path::new("/tmp/clips/cs2.2026-05-09-remix.mp4"));
    }
}
