//! Dashboard clip-status badge.
//!
//! A small horizontal pill (dot + label) that sits inside the dashboard's
//! Status card and reflects the live `BufferController` state. Colors
//! come from CSS classes on the dot's `gtk::Image` (the icon is
//! `media-record-symbolic`, which the GTK fast-path symbolic parser
//! tints with `currentColor` from the surrounding CSS class).
//!
//! State → visual mapping:
//!
//! | BufferState              | Visible | Dot color | Label                  | Retry button |
//! |--------------------------|---------|-----------|------------------------|--------------|
//! | Uninitialized            | yes     | dim white | "Set up Clips"         | no           |
//! | Idle                     | no      | —         | —                      | no           |
//! | Arming / Armed           | yes     | green     | "Recording — <game>"   | no           |
//! | Saving                   | yes     | yellow    | "Saving…"              | no           |
//! | ErrorState               | yes     | red       | "Capture stopped"      | yes          |
//!
//! Idle hides the badge entirely so the dashboard isn't visually noisy
//! when the user isn't actively gaming. The other states are persistent
//! reminders that the buffer is doing something the user might care about.
//!
//! The Retry button is wired to the `app.retry-clip-capture` GAction
//! (registered in `app.rs`), which calls
//! `BufferController::retry(&cmd_tx)`. That moves the state machine from
//! `ErrorState → Idle` and runs `maybe_arm` so the buffer re-engages
//! when a game is detected.

use adw::prelude::*;

use crate::clips::BufferState;

/// Owns the badge widget tree and exposes a single `set_state` setter.
/// Cloned cheaply (all fields are GObject refs).
#[derive(Clone)]
pub struct StatusIndicator {
    pub root: gtk::Widget,
    label: gtk::Label,
    dot: gtk::Image,
    retry_btn: gtk::Button,
}

impl StatusIndicator {
    /// Update the badge to reflect a new `BufferState` and (when relevant)
    /// the currently-detected game's name. `game` is only consulted in
    /// the `Arming` / `Armed` states; it's safe to pass `None` otherwise.
    pub fn set_state(&self, state: BufferState, game: Option<&str>) {
        // Clear all dot color classes before re-applying — the alternative
        // (per-state diff) would be both more code and more bug-prone.
        for cls in ["dot-armed", "dot-saving", "dot-error", "dot-setup", "dot-paused"] {
            self.dot.remove_css_class(cls);
        }
        // Retry button is only relevant in ErrorState; hide elsewhere.
        self.retry_btn.set_visible(matches!(state, BufferState::ErrorState));
        self.root.set_visible(true);
        match state {
            BufferState::Uninitialized => {
                self.dot.add_css_class("dot-setup");
                self.label.set_label("Set up Clips");
            }
            BufferState::Idle => {
                self.root.set_visible(false);
            }
            BufferState::Arming | BufferState::Armed => {
                self.dot.add_css_class("dot-armed");
                let g = game.unwrap_or("game");
                self.label.set_label(&format!("Recording: {g}"));
            }
            BufferState::Saving => {
                self.dot.add_css_class("dot-saving");
                self.label.set_label("Saving…");
            }
            BufferState::ErrorState => {
                self.dot.add_css_class("dot-error");
                self.label.set_label("Capture stopped");
            }
            BufferState::Paused => {
                self.dot.add_css_class("dot-paused");
                self.label.set_label("Paused");
            }
        }
    }
}

/// Construct a fresh indicator. The badge starts visible in the
/// "Uninitialized" look so the dashboard reflects the buffer's actual
/// startup state until the first `set_state` call after the auto-resume
/// block runs.
pub fn build_status_indicator() -> StatusIndicator {
    let dot = gtk::Image::from_icon_name("media-record-symbolic");
    dot.set_pixel_size(12);
    dot.add_css_class("dot-setup");

    let label = gtk::Label::builder().label("Set up Clips").build();

    // Retry button: hidden by default. set_state flips it visible when
    // BufferState::ErrorState is reported. Wired to the
    // `app.retry-clip-capture` GAction so the click crosses cleanly into
    // app.rs's BufferController without the indicator needing a Sender
    // ref. The "flat" + caption styling keeps it visually subordinate to
    // the badge label so it doesn't shout.
    let retry_btn = gtk::Button::builder()
        .label("Retry")
        .visible(false)
        .css_classes(["flat", "caption"])
        .valign(gtk::Align::Center)
        .build();
    retry_btn.set_action_name(Some("app.retry-clip-capture"));

    let badge = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    badge.add_css_class("clip-indicator");
    badge.append(&dot);
    badge.append(&label);
    badge.append(&retry_btn);

    StatusIndicator {
        root: badge.upcast(),
        label,
        dot,
        retry_btn,
    }
}
