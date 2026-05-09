//! Saved-clip notifications. Toast when the window is visible, gio::Notification
//! when hidden.
//!
//! Wired from `app.rs`'s BackendEvent poll: when the backend reports a `Saved`
//! event we look up the saved file's basename, kick off a thumbnail extraction
//! on a worker thread (ffmpeg shell-out, ~50–100 ms — too slow for the GTK main
//! thread), and post the result back here. This module does not block.
//!
//! Both the toast button and the desktop-notification button target the
//! `app.show-clip` GAction with the saved clip's path as a `Variant<String>`.
//! The action handler in `app.rs` presents the window and switches to the
//! Clips tab.

use std::path::Path;

use adw::prelude::*;
use gtk::gio;

/// Dispatch a "clip saved" notification to the user.
///
/// - If the window is visible, an `adw::Toast` is shown inside the window's
///   `adw::ToastOverlay` (6 s timeout).
/// - If the window is hidden, a `gio::Notification` is sent via
///   `app.send_notification("clip-saved", …)` so the desktop notification
///   daemon (KDE, GNOME, …) renders it. The fixed `"clip-saved"` ID means
///   rapid back-to-back saves coalesce — the latest replaces the older one
///   instead of stacking up.
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
        // Toast inside main window. Walk the widget tree to find the
        // ToastOverlay we wrapped the root in (in `window.rs`). If for some
        // reason the overlay is missing (e.g. window structure changed and
        // this code wasn't updated), log and skip — better than crashing.
        if let Some(overlay) = find_toast_overlay(window) {
            // Owned String + to_variant() yields an unambiguous Variant<String>.
            // Cow::to_variant doesn't always resolve cleanly across glib
            // versions, so this explicit form is safest.
            let path_str: String = saved_path.to_string_lossy().into_owned();
            let toast = adw::Toast::builder()
                .title(&title)
                .button_label("Show")
                .action_name("app.show-clip")
                .action_target(&path_str.to_variant())
                .timeout(6)
                .build();
            overlay.add_toast(toast);
        } else {
            log::warn!("notify_saved: no AdwToastOverlay under the window");
        }
    } else {
        // Desktop notification. KDE / GNOME / etc. pick this up via XDG.
        let notif = gio::Notification::new(&title);
        if let Some(thumb) = thumbnail_path {
            // Only set the icon if the thumbnail is actually readable; an
            // unreadable path silently breaks notification rendering on some
            // daemons. query_info is the cheapest probe that confirms the
            // file exists + is accessible.
            if gio::File::for_path(thumb)
                .query_info(
                    "*",
                    gio::FileQueryInfoFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .is_ok()
            {
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
        // Reusing the "clip-saved" ID makes a fresh save replace the prior
        // notification rather than piling them up. Users mashing the hotkey
        // shouldn't get a stack of 5 toasts on the desktop.
        app.send_notification(Some("clip-saved"), &notif);
    }
}

/// Walk descendants of the window looking for the AdwToastOverlay.
///
/// We wrap the window root in a ToastOverlay in `window.rs`, but the structure
/// is `Window → ToastOverlay → root: Box → …`, so a single `child()` call
/// finds it. The loop is defensive: if the wrapping ever changes (e.g.
/// adding an `adw::ToolbarView` between the window and the overlay), this
/// still finds it without code edits.
fn find_toast_overlay(window: &adw::ApplicationWindow) -> Option<adw::ToastOverlay> {
    let mut current = window.child();
    while let Some(w) = current {
        if let Ok(o) = w.clone().downcast::<adw::ToastOverlay>() {
            return Some(o);
        }
        current = w.first_child();
    }
    None
}
