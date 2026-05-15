//! GlobalShortcuts portal binding via ashpd.
//!
//! Talks to `org.freedesktop.portal.GlobalShortcuts` so users can press a
//! desktop-wide key chord to save a clip without bringing the app to the
//! foreground. KDE Plasma 6 (the primary target) shows a portal-mediated
//! dialog the first time we bind, lets the user accept the suggested
//! triggers or pick their own, and persists the binding under the user's
//! shortcut settings (System Settings → Shortcuts → Global Shortcuts).
//!
//! The session lifetime is the duration of `run_global_shortcuts` —
//! callers spawn this on `glib::MainContext::default().spawn_local(...)`
//! at app startup so the future runs forever and the bindings stay alive.

use std::cell::RefCell;
use std::rc::Rc;

use ashpd::WindowIdentifier;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use gtk::prelude::IsA;

use crate::clips::settings::ClipSettings;

/// Suggested shortcut bindings.
///
/// Modifier syntax follows the XDG GlobalShortcuts spec (see
/// https://specifications.freedesktop.org/shortcuts-spec/latest/).
/// Keys are joined with `+`; modifiers: `CTRL`, `ALT`, `SHIFT`, `LOGO`.
/// (`LOGO` is the Super/Meta/Windows key. KDE displays it as "Meta" in the
/// shortcuts picker — that's a UI-side translation; the wire format is LOGO.)
pub fn suggested_bindings() -> Vec<NewShortcut> {
    vec![
        NewShortcut::new("save-clip", "Save the last N seconds of gameplay")
            .preferred_trigger(Some("ALT+S")),
        NewShortcut::new("save-clip-short", "Save the last 30 seconds")
            .preferred_trigger(Some("LOGO+SHIFT+1")),
        NewShortcut::new("save-clip-medium", "Save the last 60 seconds")
            .preferred_trigger(Some("LOGO+SHIFT+2")),
        NewShortcut::new("save-clip-long", "Save the last 120 seconds")
            .preferred_trigger(Some("LOGO+SHIFT+3")),
    ]
}

/// Open the GlobalShortcuts portal session and bind suggested shortcuts.
/// Drives the activation stream forever, invoking `on_shortcut(id)` each
/// time the user presses a bound chord. The caller spawns this on the
/// glib main context so it shares the `!Send` GTK thread.
///
/// On first launch, KDE shows a portal dialog asking the user to confirm
/// or rebind the suggested chords. Subsequent launches re-bind the same
/// shortcuts silently as long as the user accepted them once.
///
/// `parent` is the window the portal can use to derive the calling app's
/// identity via xdg-foreign. KDE's xdg-desktop-portal-kde requires this
/// when running OUTSIDE a Flatpak sandbox: passing `None` triggers
/// `org.freedesktop.portal.Error.NotAllowed: An app id is required` and
/// the listener exits before any shortcut can fire. Inside a Flatpak
/// (final shipping target), either form works because the portal reads
/// the app id from the sandbox metadata. Pass `Some(&window)` whenever
/// possible; `None` is accepted but expected to fail on host-installed
/// builds.
///
/// `settings_cell` is the shared in-memory settings cell. When provided,
/// `list_shortcuts` is called after a successful bind and the portal's
/// reported `trigger_description` for the `save-clip` chord is persisted
/// to `save_hotkey_display`, so the dashboard Clips section can label the
/// hotkey hint without a portal round-trip on every refresh. Callers that
/// don't care about the display string (or don't have a settings cell
/// handy) pass `None`.
///
/// Returns when the portal session ends (which usually means the portal
/// daemon died — caller may want to log or surface this).
pub async fn run_global_shortcuts<F>(
    parent: Option<&impl IsA<gtk::Native>>,
    mut on_shortcut: F,
    settings_cell: Option<Rc<RefCell<ClipSettings>>>,
) -> ashpd::Result<()>
where
    F: FnMut(&str) + 'static,
{
    let proxy = GlobalShortcuts::new().await?;
    let session = proxy.create_session().await?;
    let identifier = if let Some(p) = parent {
        WindowIdentifier::from_native(p).await
    } else {
        None
    };
    proxy
        .bind_shortcuts(&session, &suggested_bindings(), identifier.as_ref())
        .await?;

    // Read back the portal's display string for the bound `save-clip` chord
    // and persist. ashpd 0.11.1's `list_shortcuts` takes a single session
    // argument; ListShortcutsOptions is private to the crate.
    persist_save_hotkey_display(&proxy, &session, settings_cell.as_ref()).await;

    let mut stream = proxy.receive_activated().await?;
    while let Some(activation) = stream.next().await {
        on_shortcut(activation.shortcut_id());
    }
    Ok(())
}

/// Re-open the binding dialog by creating a fresh session and re-binding
/// the suggested shortcuts. ashpd 0.11.1 exposes `list_shortcuts`;
/// `ConfigureShortcuts` is still a portal-level API not available. The
/// practical workaround for "open the picker again" is to drive a fresh
/// bind: KDE's portal redisplays its picker when the shortcut IDs are
/// already taken or when the user has not yet confirmed them, giving the
/// user a path to change the chord.
///
/// `parent` is the window the portal dialog should be modal to. When
/// running OUTSIDE a Flatpak sandbox, the portal needs the parent window
/// identifier to derive the calling app's identity (it can't read
/// `/proc/<pid>/root/.flatpak-info` because the file doesn't exist for
/// host-installed binaries). Passing `None` triggers
/// `org.freedesktop.portal.Error.NotAllowed: An app id is required` on
/// KDE's xdg-desktop-portal-kde when the caller is not Flatpak-packaged.
/// Inside a Flatpak (final shipping target), either form works because
/// the portal reads the app id from the sandbox metadata.
///
/// This is fire-and-forget — the existing `run_global_shortcuts` listener
/// from the original session keeps running and continues receiving
/// activations. If the user picks a different chord, the portal updates
/// the binding under the same session. The new chord's display string is
/// re-read here via `list_shortcuts` and pushed to `settings_cell` so the
/// dashboard hint updates without waiting for an app restart.
pub async fn rebind_shortcuts(
    parent: Option<&impl IsA<gtk::Native>>,
    settings_cell: Option<Rc<RefCell<ClipSettings>>>,
) -> ashpd::Result<()> {
    let proxy = GlobalShortcuts::new().await?;
    let session = proxy.create_session().await?;
    let identifier = if let Some(p) = parent {
        WindowIdentifier::from_native(p).await
    } else {
        None
    };
    proxy
        .bind_shortcuts(&session, &suggested_bindings(), identifier.as_ref())
        .await?;
    persist_save_hotkey_display(&proxy, &session, settings_cell.as_ref()).await;
    Ok(())
}

/// Read the portal's `list_shortcuts` for the `save-clip` chord and persist
/// the user-visible trigger description to `ClipSettings::save_hotkey_display`.
/// Errors are logged at warn level; a failed read leaves the existing setting
/// in place rather than overwriting with a guess.
async fn persist_save_hotkey_display(
    proxy: &GlobalShortcuts<'_>,
    session: &ashpd::desktop::Session<'_, GlobalShortcuts<'_>>,
    settings_cell: Option<&Rc<RefCell<ClipSettings>>>,
) {
    let Some(cell) = settings_cell else { return };
    match proxy.list_shortcuts(session).await {
        Ok(req) => match req.response() {
            Ok(resp) => {
                if let Some(s) = resp.shortcuts().iter().find(|s| s.id() == "save-clip") {
                    let display = s.trigger_description().to_string();
                    cell.borrow_mut().save_hotkey_display = display;
                    if let Err(e) = crate::clips::settings::save(&cell.borrow()) {
                        log::warn!("save_hotkey_display persist failed: {e}");
                    }
                }
            }
            Err(e) => log::warn!("list_shortcuts response decode failed: {e}"),
        },
        Err(e) => log::warn!("list_shortcuts call failed: {e}"),
    }
}
