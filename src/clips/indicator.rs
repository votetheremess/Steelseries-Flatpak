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
//! | BufferState              | Visible | Dot color | Label                 |
//! |--------------------------|---------|-----------|-----------------------|
//! | Uninitialized            | yes     | dim white | "Set up Clips"        |
//! | Idle                     | no      | —         | —                     |
//! | Arming / Armed           | yes     | green     | "Buffering — <game>"  |
//! | Saving                   | yes     | yellow    | "Saving…"             |
//! | ErrorState               | yes     | red       | "Capture stopped"     |
//!
//! Idle hides the badge entirely so the dashboard isn't visually noisy
//! when the user isn't actively gaming. The other states are persistent
//! reminders that the buffer is doing something the user might care about.

use adw::prelude::*;

use crate::clips::BufferState;

/// Owns the badge widget tree and exposes a single `set_state` setter.
/// Cloned cheaply (all fields are GObject refs).
#[derive(Clone)]
pub struct StatusIndicator {
    pub root: gtk::Widget,
    label: gtk::Label,
    dot: gtk::Image,
}

impl StatusIndicator {
    /// Update the badge to reflect a new `BufferState` and (when relevant)
    /// the currently-detected game's name. `game` is only consulted in
    /// the `Arming` / `Armed` states; it's safe to pass `None` otherwise.
    pub fn set_state(&self, state: BufferState, game: Option<&str>) {
        // Clear all dot color classes before re-applying — the alternative
        // (per-state diff) would be both more code and more bug-prone.
        for cls in ["dot-armed", "dot-saving", "dot-error", "dot-setup"] {
            self.dot.remove_css_class(cls);
        }
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

/// Construct a fresh indicator. The badge starts visible in the
/// "Uninitialized" look so the dashboard reflects the buffer's actual
/// startup state until the first `set_state` call after the auto-resume
/// block runs.
pub fn build_status_indicator() -> StatusIndicator {
    let dot = gtk::Image::from_icon_name("media-record-symbolic");
    dot.set_pixel_size(12);
    dot.add_css_class("dot-setup");

    let label = gtk::Label::builder().label("Set up Clips").build();

    let badge = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    badge.add_css_class("clip-indicator");
    badge.append(&dot);
    badge.append(&label);

    StatusIndicator { root: badge.upcast(), label, dot }
}
