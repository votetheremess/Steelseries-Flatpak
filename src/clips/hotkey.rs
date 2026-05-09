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

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;

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
            .preferred_trigger(Some("LOGO+SHIFT+R")),
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
/// Returns when the portal session ends (which usually means the portal
/// daemon died — caller may want to log or surface this).
pub async fn run_global_shortcuts<F>(mut on_shortcut: F) -> ashpd::Result<()>
where
    F: FnMut(&str) + 'static,
{
    let proxy = GlobalShortcuts::new().await?;
    let session = proxy.create_session().await?;
    // Third arg is Option<&WindowIdentifier>; passing None lets the portal
    // pick the focus root. We don't have a transient parent to attach to
    // because the binding happens at startup before the window may even be
    // visible (autostart --hidden), and the bind dialog's lifetime is
    // user-driven anyway.
    proxy
        .bind_shortcuts(&session, &suggested_bindings(), None)
        .await?;
    let mut stream = proxy.receive_activated().await?;
    while let Some(activation) = stream.next().await {
        on_shortcut(activation.shortcut_id());
    }
    Ok(())
}

/// Re-open the binding dialog by creating a fresh session and re-binding
/// the suggested shortcuts. ashpd 0.10 doesn't expose the (proposed) portal
/// `ConfigureShortcuts` method directly, so the practical workaround is to
/// drive a new bind: KDE's portal will redisplay its picker when the
/// shortcut IDs are already taken or when the user has not yet confirmed
/// them, giving the user a path to change the chord.
///
/// This is fire-and-forget — the existing `run_global_shortcuts` listener
/// from the original session keeps running and continues receiving
/// activations. If the user picks a different chord, the portal updates
/// the binding under the same session.
pub async fn rebind_shortcuts() -> ashpd::Result<()> {
    let proxy = GlobalShortcuts::new().await?;
    let session = proxy.create_session().await?;
    proxy
        .bind_shortcuts(&session, &suggested_bindings(), None)
        .await?;
    Ok(())
}
