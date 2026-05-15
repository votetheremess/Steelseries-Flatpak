//! Per-track remix panel.
//!
//! Builds the UI shown when the user picks "Remix" from a clip-card kebab
//! menu. Saved clips embed up to 6 audio tracks (track 0 is GSR's mix-down,
//! tracks 1–5 are per-source: Game / Chat / Music / Aux / Mic). The remix
//! panel exposes one row per track with a volume slider (in dB), a Mute
//! toggle, and a Solo toggle, plus a Preview / Export action bar.
//!
//! Slider semantics:
//!   * Volume slider reads in dB (-60..+6, default 0). The Export pipeline
//!     converts this to a linear gain via 10^(db/20) before passing to the
//!     ffmpeg `volume` filter. Muting forces 0; if any row is soloed, only
//!     soloed rows are audible (others are forced to 0).
//!
//! Preview pipeline: opens the original clip in the user's default video
//! player via `xdg-open`. Slider values do NOT apply to the preview — only
//! to Export. See the Preview button comment for the GStreamer alternative
//! we deferred.
//!
//! Export pipeline: spawns `ffmpeg -filter_complex` in a worker thread,
//! mixing all 6 input tracks into a fresh track 0 while preserving the
//! original per-source tracks unchanged. Output is `<stem>-remix.mp4` in
//! the same directory.

use std::cell::Cell;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;

use adw::prelude::*;
use gtk::glib;

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
/// Export-button click reads from it.
pub struct RemixPanel {
    pub root: gtk::Box,
    /// Live mix state. The Export click handler reads from this; kept on
    /// the public struct so future API extensions (e.g. preset save/load)
    /// can introspect or mutate the mix without going through the widget
    /// tree. Currently no external reader, but cheaper to leave in place
    /// than to remove and re-add later.
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
///   * `on_exported` — fires on the GTK main thread after a successful
///     Export. Caller should refresh the grid model so the new
///     `*-remix.mp4` shows up. The exported file's path is passed for
///     any post-export side effects (e.g. notifications).
pub fn build_remix_panel(
    clip_path: &Path,
    on_close: impl Fn() + 'static,
    on_exported: impl Fn(PathBuf) + 'static,
) -> RemixPanel {
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
    let action_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(12)
        .halign(gtk::Align::End)
        .build();

    // Preview button — opens the original clip in the user's default
    // video player via `xdg-open`. The slider values are NOT applied to
    // this preview; the tooltip explains that Export is the only place
    // that respects the mix.
    //
    // V1 simplification: real per-track preview requires a GStreamer
    // pipeline (qtdemux ! 6× decodebin ! 6× volume ! audiomixer !
    // autoaudiosink) assembled via gst::parse_launch with current slider
    // values. That adds gstreamer-rs as a heavyweight dep (multi-MB
    // transitive tree) and ~50–100 lines of pipeline assembly code with
    // nontrivial pad-add / decodebin async timing — too much weight for
    // a v1 nice-to-have. See `docs/superpowers/plans/2026-05-08-
    // clipping-system.md` Task 7.3 for the canonical pipeline string;
    // a future polish pass can replace this fallback in place.
    let preview_btn = gtk::Button::builder()
        .label("Preview")
        .tooltip_text(
            "Opens the original clip in your default video player. \
             Slider adjustments apply to Export only.",
        )
        .build();
    {
        let path_for_preview = clip_path.to_path_buf();
        preview_btn.connect_clicked(move |_| {
            if let Err(e) = Command::new("xdg-open")
                .arg(&path_for_preview)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                log::warn!(
                    "remix preview: xdg-open failed for {}: {e}",
                    path_for_preview.display()
                );
            }
        });
    }

    // Export button — spawns ffmpeg in a worker thread (10–30 s for a
    // 60 s clip on a modern desktop), drains the result via mpsc +
    // glib::timeout_add_local, and on success hands the new path to
    // `on_exported` so the caller can refresh the grid. The button label
    // flips to "Exporting…" while running and disables itself; both Cell
    // guard + sensitive flip protect against double-click.
    let export_btn = gtk::Button::builder()
        .label("Export")
        .css_classes(["suggested-action"])
        .build();
    {
        let path_for_export = clip_path.to_path_buf();
        let state_for_export = state.clone();
        let btn_weak: glib::SendWeakRef<gtk::Button> = export_btn.downgrade().into();
        // Box `on_exported` once into an Rc so each timer-tick clone is
        // cheap and the closure stays !Send-friendly. (`MainContext::
        // invoke` would require Send, so we use the existing project
        // pattern of a Send-mpsc + glib::timeout_add_local drain.)
        let on_exported = Rc::new(on_exported);
        // Re-entrancy guard: if the user double-clicks Export, the
        // second click is a no-op until the first ffmpeg has finished.
        // The primary defence is `set_sensitive(false)` below, but
        // `Cell<bool>` protects against any signal-routing edge case
        // (e.g. a queued click delivered after our sensitivity flip but
        // before ffmpeg returns).
        let running = Rc::new(Cell::new(false));

        export_btn.connect_clicked(move |btn| {
            if running.get() {
                return;
            }
            running.set(true);
            btn.set_sensitive(false);
            btn.set_label("Exporting…");

            let states_snapshot = *state_for_export.borrow();
            let output_path = remix_output_path(&path_for_export);

            let (tx, rx) = mpsc::channel::<std::io::Result<PathBuf>>();
            let input_for_thread = path_for_export.clone();
            let output_for_thread = output_path.clone();
            std::thread::spawn(move || {
                let result = export_remix(
                    &input_for_thread,
                    &output_for_thread,
                    &states_snapshot,
                )
                .map(|()| output_for_thread);
                let _ = tx.send(result);
            });

            // Drain on the main thread. Restoring the button label /
            // sensitivity is the priority — the on_exported callback
            // is best-effort (it should never fail in practice, but if
            // the GridView model has been swapped out from under us
            // we don't want a stuck "Exporting…" button).
            let on_exported_for_drain = on_exported.clone();
            let btn_weak_for_drain = btn_weak.clone();
            let running_for_drain = running.clone();
            glib::timeout_add_local(
                std::time::Duration::from_millis(100),
                move || match rx.try_recv() {
                    Ok(Ok(out_path)) => {
                        if let Some(b) = btn_weak_for_drain.upgrade() {
                            b.set_label("Export");
                            b.set_sensitive(true);
                        }
                        running_for_drain.set(false);
                        on_exported_for_drain(out_path);
                        glib::ControlFlow::Break
                    }
                    Ok(Err(e)) => {
                        log::warn!("remix export failed: {e}");
                        if let Some(b) = btn_weak_for_drain.upgrade() {
                            b.set_label("Export");
                            b.set_sensitive(true);
                        }
                        running_for_drain.set(false);
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        log::warn!("remix export: worker disconnected");
                        if let Some(b) = btn_weak_for_drain.upgrade() {
                            b.set_label("Export");
                            b.set_sensitive(true);
                        }
                        running_for_drain.set(false);
                        glib::ControlFlow::Break
                    }
                },
            );
        });
    }

    action_bar.append(&preview_btn);
    action_bar.append(&export_btn);
    root.append(&action_bar);

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
fn remix_output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".to_string());
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}-remix.mp4"))
}

/// Spawn ffmpeg with -filter_complex to mix all 6 input audio tracks into
/// a fresh track 0, preserving original tracks 1–5 unchanged. Per-track
/// volumes / mute / solo are baked into the filter_complex graph.
///
/// The dB→linear conversion happens here (not in the slider handler) so
/// the UI stays in user-friendly dB units throughout. See `linear_gain_for`
/// for the mute/solo rules.
///
/// Track count assumption: we always pass 6 inputs to amix. If the source
/// file has fewer (e.g. GSR was configured without per-source tracks),
/// ffmpeg's amix prints a warning and skips the missing tracks. The Phase 8
/// verification item will tighten this with an ffprobe step if it turns
/// out to bite users.
///
/// Argument quoting: `-filter_complex` takes a single string argument.
/// We pass it as `args(["-filter_complex", &filter_str])` — Command's
/// args API doesn't go through a shell, so we don't need to escape
/// internal quotes / semicolons / spaces.
fn export_remix(
    input: &Path,
    output: &Path,
    states: &[TrackState; 6],
) -> std::io::Result<()> {
    // Build filter_complex: each input track gets a volume filter, then
    // amix sums them into [mix].
    //
    // `[0:a:0]volume=V0[a0]; [0:a:1]volume=V1[a1]; … [a0][a1]…amix=…`
    let mut filter = String::new();
    let mut inputs = String::new();
    for i in 0..6 {
        let gain = linear_gain_for(states, i);
        // ffmpeg's `volume` filter accepts a non-negative float; format
        // with 6 decimal digits to preserve quiet-end precision (e.g.
        // -40 dB → 0.01 — a 2-digit format would round it).
        filter.push_str(&format!("[0:a:{i}]volume={gain:.6}[a{i}];"));
        inputs.push_str(&format!("[a{i}]"));
    }
    filter.push_str(&format!(
        "{inputs}amix=inputs=6:duration=longest:dropout_transition=0[mix]"
    ));

    let status = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(input)
        .args([
            "-filter_complex",
            &filter,
            "-map",
            "0:v:0",
            "-map",
            "[mix]",
            "-map",
            "0:a:1",
            "-map",
            "0:a:2",
            "-map",
            "0:a:3",
            "-map",
            "0:a:4",
            "-map",
            "0:a:5",
            "-c:v",
            "copy",
            "-c:a:0",
            "aac",
            "-b:a:0",
            "192k",
            "-c:a:1",
            "copy",
            "-c:a:2",
            "copy",
            "-c:a:3",
            "copy",
            "-c:a:4",
            "copy",
            "-c:a:5",
            "copy",
        ])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "ffmpeg export failed: {status:?}"
        )));
    }
    Ok(())
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
