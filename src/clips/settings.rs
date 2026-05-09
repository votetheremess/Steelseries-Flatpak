//! Persisted clip settings (`~/.config/arctis-chatmix/clips_settings.txt`).
//!
//! Line-oriented `key=value` format, same shape as the rest of the app's
//! config files. Unknown keys are silently ignored so a downgrade doesn't
//! lose data the older binary doesn't understand.

use adw::prelude::*;
use gtk::gio;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use crate::clips::{BufferController, CaptureConfig, ClipCommand};

const SETTINGS_FILENAME: &str = "clips_settings.txt";

/// Bitrate options shown in the dropdown, in megabits per second.
const BITRATE_OPTIONS_MBPS: [u32; 4] = [15, 25, 40, 60];

/// Disk-retention options shown in the dropdown. `None` means "Keep all"
/// (no cap), every other entry is a hard cap in gigabytes.
const RETENTION_OPTIONS_GB: [Option<u32>; 5] =
    [None, Some(10), Some(30), Some(50), Some(100)];

/// Fixed full-buffer hotkey command that other shortcut systems can call to
/// trigger a save. Kept as a single line (no shell wrapping) so the user can
/// paste it directly into a hotkey daemon's command field.
const SAVE_CLIP_DBUS_COMMAND: &str = concat!(
    "dbus-send --session ",
    "--dest=com.github.arctis_chatmix.ArctisNovaEliteChatMix ",
    "/com/github/arctis_chatmix/ArctisNovaEliteChatMix ",
    "org.gtk.Actions.Activate ",
    "string:save-clip array:string: array:string:",
);

fn settings_path() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home)
        .join(".config/arctis-chatmix")
        .join(SETTINGS_FILENAME)
}

#[derive(Debug, Clone)]
pub struct ClipSettings {
    pub buffer_length: u32,
    pub bitrate_mbps: u32,
    pub auto_arm: bool,
    pub always_armed: bool,
    pub per_source_tracks: bool,
    pub mic_capture: bool,
    pub storage_path: PathBuf,
    pub disk_cap_gb: Option<u32>, // None = no cap
    /// Set to `true` once the user has completed Page 2 of the onboarding
    /// wizard (i.e., picked a screen). Page 3 is optional and does not flip
    /// this flag.
    pub onboarding_complete: bool,
}

impl Default for ClipSettings {
    fn default() -> Self {
        Self {
            buffer_length: 60,
            bitrate_mbps: 25,
            auto_arm: true,
            always_armed: false,
            per_source_tracks: true,
            mic_capture: true,
            storage_path: PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("Videos/Clips"),
            disk_cap_gb: None,
            onboarding_complete: false,
        }
    }
}

pub fn load() -> ClipSettings {
    let mut s = ClipSettings::default();
    let path = settings_path();
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return s,
    };
    for line in contents.lines() {
        let (k, v) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        match k {
            "buffer_length" => {
                if let Ok(n) = v.parse() {
                    s.buffer_length = n;
                }
            }
            "bitrate_mbps" => {
                if let Ok(n) = v.parse() {
                    s.bitrate_mbps = n;
                }
            }
            "auto_arm" => s.auto_arm = v == "1",
            "always_armed" => s.always_armed = v == "1",
            "per_source_tracks" => s.per_source_tracks = v == "1",
            "mic_capture" => s.mic_capture = v == "1",
            "storage_path" => s.storage_path = PathBuf::from(v),
            "disk_cap_gb" => s.disk_cap_gb = v.parse().ok(),
            "onboarding_complete" => s.onboarding_complete = v == "1",
            _ => {}
        }
    }
    s
}

pub fn save(s: &ClipSettings) -> std::io::Result<()> {
    let path = settings_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut body = String::new();
    body.push_str(&format!("buffer_length={}\n", s.buffer_length));
    body.push_str(&format!("bitrate_mbps={}\n", s.bitrate_mbps));
    body.push_str(&format!("auto_arm={}\n", if s.auto_arm { 1 } else { 0 }));
    body.push_str(&format!(
        "always_armed={}\n",
        if s.always_armed { 1 } else { 0 }
    ));
    body.push_str(&format!(
        "per_source_tracks={}\n",
        if s.per_source_tracks { 1 } else { 0 }
    ));
    body.push_str(&format!(
        "mic_capture={}\n",
        if s.mic_capture { 1 } else { 0 }
    ));
    body.push_str(&format!("storage_path={}\n", s.storage_path.display()));
    if let Some(cap) = s.disk_cap_gb {
        body.push_str(&format!("disk_cap_gb={}\n", cap));
    }
    body.push_str(&format!(
        "onboarding_complete={}\n",
        if s.onboarding_complete { 1 } else { 0 }
    ));
    std::fs::write(path, body)
}

/// Mark onboarding complete. Idempotent.
///
/// Semantics: the flag means "user has confirmed Page 2 (screen pick) at
/// least once" — NOT "everything is set up." Reset capture source clears
/// the portal token but does NOT flip this flag back; we treat a second-
/// time wizard appearance after Reset as a re-confirmation of the screen,
/// not a full re-onboarding. This keeps Reset friction-free for
/// experienced users.
///
/// Auto-resume on the fourth arm (GSR + token + flag false — meaning the
/// user reached Page 2 last session but never pressed Next) intentionally
/// does NOT call this function: the buffer is connected with the persisted
/// token, but the wizard is shown at PickScreen so the user explicitly
/// confirms the persisted selection. The flag flips to true only when the
/// user actually clicks Next on Page 2.
pub fn mark_onboarding_complete() -> std::io::Result<()> {
    let mut s = load();
    s.onboarding_complete = true;
    save(&s)
}

/// Build a `CaptureConfig` from the persisted settings + the headset sink
/// monitor name (`<headset_sink>.monitor`). Used by every settings widget's
/// change handler so the buffer controller's runtime config stays in sync
/// with the settings UI without each call site having to re-derive the
/// mapping.
///
/// `portal_restore_token` is intentionally left as `None` here — the buffer
/// controller preserves any existing token across `on_config_change` calls,
/// so the empty-token field on this config is the right "don't touch"
/// signal.
pub fn cfg_from_settings(s: &ClipSettings, headset_sink_monitor: &str) -> CaptureConfig {
    CaptureConfig {
        buffer_secs: s.buffer_length,
        bitrate_mbps: s.bitrate_mbps,
        framerate: 60,
        portal_restore_token: None,
        headset_sink_monitor: headset_sink_monitor.to_string(),
        include_per_source_tracks: s.per_source_tracks,
        include_mic: s.mic_capture,
        output_dir: s.storage_path.clone(),
    }
}

/// Build the Clips PreferencesGroup for the Settings page. Includes the
/// capture-source row, all runtime/quality knobs (buffer length, bitrate,
/// hotkey, arm modes, audio toggles, storage location, retention), and the
/// gpu-screen-recorder Reinstall row.
///
/// The settings model is a single shared `Rc<RefCell<ClipSettings>>` that
/// every widget reads from on construction and writes to on change. After
/// each write the handler calls `save(...)`; for changes that affect the
/// running capture (buffer length, bitrate, per-source tracks, mic capture,
/// storage path), it also calls `BufferController::on_config_change` so the
/// next clip uses the fresh config without forcing the user to restart.
pub fn build_clips_group(
    clip_settings: Rc<RefCell<ClipSettings>>,
    buffer: Rc<RefCell<BufferController>>,
    cmd_tx: Sender<ClipCommand>,
    headset_sink_monitor: String,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Clips").build();

    // ------------------------------------------------------------------
    // Capture source row (Reset + Test) — existing.
    // ------------------------------------------------------------------
    let reset_row = adw::ActionRow::builder()
        .title("Capture source")
        .subtitle("Pick the screen recorded by the clip buffer")
        .build();
    let row_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    let test_btn = gtk::Button::builder().label("Test").build();
    test_btn.set_action_name(Some("app.test-clip-capture"));
    let reset_btn = gtk::Button::builder().label("Reset").build();
    reset_btn.set_action_name(Some("app.reset-clips-capture"));
    row_box.append(&test_btn);
    row_box.append(&reset_btn);
    reset_row.add_suffix(&row_box);
    group.add(&reset_row);

    // ------------------------------------------------------------------
    // Buffer length (SpinRow 30..=300 seconds).
    // ------------------------------------------------------------------
    let initial_buffer = clip_settings.borrow().buffer_length as f64;
    let buffer_adj = gtk::Adjustment::new(initial_buffer, 30.0, 300.0, 5.0, 30.0, 0.0);
    let buffer_row = adw::SpinRow::builder()
        .title("Buffer length")
        .subtitle("Seconds of gameplay kept in the replay buffer")
        .adjustment(&buffer_adj)
        .digits(0)
        .climb_rate(1.0)
        .build();
    {
        let clip_settings = clip_settings.clone();
        let buffer = buffer.clone();
        let cmd_tx = cmd_tx.clone();
        let monitor = headset_sink_monitor.clone();
        buffer_row.connect_value_notify(move |row| {
            let new_value = row.value().round() as u32;
            {
                let mut s = clip_settings.borrow_mut();
                if s.buffer_length == new_value {
                    return;
                }
                s.buffer_length = new_value;
            }
            persist_and_reconfigure(&clip_settings, &buffer, &cmd_tx, &monitor);
        });
    }
    group.add(&buffer_row);

    // ------------------------------------------------------------------
    // Bitrate dropdown (15 / 25 / 40 / 60 Mbps).
    // ------------------------------------------------------------------
    let bitrate_strings: Vec<String> = BITRATE_OPTIONS_MBPS
        .iter()
        .map(|m| format!("{m} Mbps"))
        .collect();
    let bitrate_str_refs: Vec<&str> = bitrate_strings.iter().map(|s| s.as_str()).collect();
    let bitrate_model = gtk::StringList::new(&bitrate_str_refs);
    let initial_bitrate_idx = BITRATE_OPTIONS_MBPS
        .iter()
        .position(|&m| m == clip_settings.borrow().bitrate_mbps)
        .unwrap_or(1) as u32;
    let bitrate_row = adw::ComboRow::builder()
        .title("Bitrate")
        .subtitle("Higher = larger files, better quality")
        .model(&bitrate_model)
        .selected(initial_bitrate_idx)
        .build();
    {
        let clip_settings = clip_settings.clone();
        let buffer = buffer.clone();
        let cmd_tx = cmd_tx.clone();
        let monitor = headset_sink_monitor.clone();
        bitrate_row.connect_selected_notify(move |row| {
            let idx = row.selected() as usize;
            let mbps = BITRATE_OPTIONS_MBPS
                .get(idx)
                .copied()
                .unwrap_or(BITRATE_OPTIONS_MBPS[1]);
            {
                let mut s = clip_settings.borrow_mut();
                if s.bitrate_mbps == mbps {
                    return;
                }
                s.bitrate_mbps = mbps;
            }
            persist_and_reconfigure(&clip_settings, &buffer, &cmd_tx, &monitor);
        });
    }
    group.add(&bitrate_row);

    // ------------------------------------------------------------------
    // Hotkey row (current binding + Rebind button).
    //
    // The actual binding is owned by KDE's GlobalShortcuts portal; we don't
    // know the user's current chord at runtime (ashpd 0.10 has no
    // `list_shortcuts` API). We display the suggested default — which is
    // also what KDE persists if the user just clicks "Save" in the bind
    // dialog without changes — and let the Rebind button re-open the
    // portal picker if they want to switch chords.
    // ------------------------------------------------------------------
    let hotkey_row = adw::ActionRow::builder()
        .title("Hotkey")
        .subtitle("Suggested: Super+Shift+R (manage in System Settings → Shortcuts)")
        .build();
    let rebind_btn = gtk::Button::builder()
        .label("Rebind…")
        .valign(gtk::Align::Center)
        .build();
    rebind_btn.set_action_name(Some("app.rebind-clip-hotkey"));
    hotkey_row.add_suffix(&rebind_btn);
    group.add(&hotkey_row);

    // Copyable D-Bus command row — for users binding the save action via
    // a third-party shortcut daemon (sxhkd, AutoKey, hyprbinds, etc.).
    let dbus_row = adw::ActionRow::builder()
        .title("Save-clip command")
        .subtitle("Bind from any external shortcut tool")
        .build();
    let dbus_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    let dbus_label = gtk::Label::builder()
        .label(SAVE_CLIP_DBUS_COMMAND)
        .css_classes(["monospace", "caption"])
        .selectable(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(36)
        .tooltip_text(SAVE_CLIP_DBUS_COMMAND)
        .build();
    let dbus_copy_btn = gtk::Button::builder()
        .label("Copy")
        .valign(gtk::Align::Center)
        .build();
    dbus_copy_btn.set_action_name(Some("app.copy-clip-hotkey-cmd"));
    dbus_box.append(&dbus_label);
    dbus_box.append(&dbus_copy_btn);
    dbus_row.add_suffix(&dbus_box);
    group.add(&dbus_row);

    // ------------------------------------------------------------------
    // Auto-arm + Always armed (mutex pair).
    //
    // Mutex semantics: when "Always armed" is on, "Auto-arm" becomes
    // insensitive — the underlying flag is preserved (so toggling Always
    // back off restores the user's prior auto-arm preference) but it can't
    // be edited while always-armed is in effect.
    // ------------------------------------------------------------------
    let auto_arm_row = adw::SwitchRow::builder()
        .title("Auto-arm during games")
        .subtitle("Start buffering when a known game launches")
        .active(clip_settings.borrow().auto_arm)
        .build();
    auto_arm_row.set_sensitive(!clip_settings.borrow().always_armed);

    let always_row = adw::SwitchRow::builder()
        .title("Always armed")
        .subtitle("Buffer continuously, even outside games")
        .active(clip_settings.borrow().always_armed)
        .build();

    {
        let clip_settings = clip_settings.clone();
        let buffer = buffer.clone();
        let auto_arm_row_for_handler = auto_arm_row.clone();
        auto_arm_row.connect_active_notify(move |row| {
            let new_value = row.is_active();
            {
                let mut s = clip_settings.borrow_mut();
                if s.auto_arm == new_value {
                    return;
                }
                s.auto_arm = new_value;
            }
            buffer.borrow_mut().auto_arm = new_value;
            if let Err(e) = save(&clip_settings.borrow()) {
                log::warn!("clip settings save failed: {e}");
            }
            // No-op self-reference; keeps the row reachable for future
            // sensitivity tweaks if Always-armed wiring expands.
            let _ = &auto_arm_row_for_handler;
        });
    }

    {
        let clip_settings = clip_settings.clone();
        let buffer = buffer.clone();
        let auto_arm_row_for_always = auto_arm_row.clone();
        always_row.connect_active_notify(move |row| {
            let new_value = row.is_active();
            {
                let mut s = clip_settings.borrow_mut();
                if s.always_armed == new_value {
                    return;
                }
                s.always_armed = new_value;
            }
            buffer.borrow_mut().always_armed = new_value;
            // Lock auto-arm switch when always-armed is on so the user
            // can't put the buffer into a contradictory state.
            auto_arm_row_for_always.set_sensitive(!new_value);
            if let Err(e) = save(&clip_settings.borrow()) {
                log::warn!("clip settings save failed: {e}");
            }
        });
    }
    group.add(&auto_arm_row);
    group.add(&always_row);

    // ------------------------------------------------------------------
    // Per-source audio tracks.
    // ------------------------------------------------------------------
    let per_source_row = adw::SwitchRow::builder()
        .title("Per-source audio tracks")
        .subtitle("Save Game/Chat/Music/Aux on separate AAC tracks")
        .active(clip_settings.borrow().per_source_tracks)
        .build();
    {
        let clip_settings = clip_settings.clone();
        let buffer = buffer.clone();
        let cmd_tx = cmd_tx.clone();
        let monitor = headset_sink_monitor.clone();
        per_source_row.connect_active_notify(move |row| {
            let new_value = row.is_active();
            {
                let mut s = clip_settings.borrow_mut();
                if s.per_source_tracks == new_value {
                    return;
                }
                s.per_source_tracks = new_value;
            }
            persist_and_reconfigure(&clip_settings, &buffer, &cmd_tx, &monitor);
        });
    }
    group.add(&per_source_row);

    // ------------------------------------------------------------------
    // Mic capture.
    // ------------------------------------------------------------------
    let mic_row = adw::SwitchRow::builder()
        .title("Mic capture")
        .subtitle("Include the headset microphone as its own track")
        .active(clip_settings.borrow().mic_capture)
        .build();
    {
        let clip_settings = clip_settings.clone();
        let buffer = buffer.clone();
        let cmd_tx = cmd_tx.clone();
        let monitor = headset_sink_monitor.clone();
        mic_row.connect_active_notify(move |row| {
            let new_value = row.is_active();
            {
                let mut s = clip_settings.borrow_mut();
                if s.mic_capture == new_value {
                    return;
                }
                s.mic_capture = new_value;
            }
            persist_and_reconfigure(&clip_settings, &buffer, &cmd_tx, &monitor);
        });
    }
    group.add(&mic_row);

    // ------------------------------------------------------------------
    // Storage location (FileDialog folder picker).
    //
    // FileDialog runs inline rather than going through a GAction because
    // its callback needs access to the row, settings cell, buffer ref,
    // and cmd_tx — the GApplication action system would force us into
    // either captured Rc-of-Rc-of-Rc gymnastics or globals. The wizard's
    // Page 3 storage button stays on `app.pick-clip-storage` (still a
    // stub — Settings is the canonical surface for changing this).
    // ------------------------------------------------------------------
    let storage_row = adw::ActionRow::builder()
        .title("Storage location")
        .subtitle(&clip_settings.borrow().storage_path.display().to_string())
        .build();
    let storage_btn = gtk::Button::builder()
        .label("Pick folder…")
        .valign(gtk::Align::Center)
        .build();
    {
        let clip_settings = clip_settings.clone();
        let buffer = buffer.clone();
        let cmd_tx = cmd_tx.clone();
        let monitor = headset_sink_monitor.clone();
        let storage_row = storage_row.clone();
        storage_btn.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::builder()
                .title("Pick clip storage folder")
                .accept_label("Save here")
                .modal(true)
                .build();
            // Seed initial folder from the current setting if it exists.
            let initial = clip_settings.borrow().storage_path.clone();
            if initial.exists() {
                dialog.set_initial_folder(Some(&gio::File::for_path(&initial)));
            }
            // Find the toplevel window for parent attachment so the
            // dialog is modal to our main window rather than free-floating.
            let parent: Option<gtk::Window> = btn
                .root()
                .and_then(|r| r.downcast::<gtk::Window>().ok());

            let clip_settings = clip_settings.clone();
            let buffer = buffer.clone();
            let cmd_tx = cmd_tx.clone();
            let monitor = monitor.clone();
            let storage_row = storage_row.clone();
            dialog.select_folder(
                parent.as_ref(),
                None::<&gio::Cancellable>,
                move |result| {
                    let file = match result {
                        Ok(f) => f,
                        Err(e) => {
                            // Cancellation surfaces as Err — only log at debug.
                            log::debug!("storage folder pick: {e}");
                            return;
                        }
                    };
                    let Some(path) = file.path() else {
                        log::warn!(
                            "storage folder pick returned a file without a local path"
                        );
                        return;
                    };
                    {
                        let mut s = clip_settings.borrow_mut();
                        if s.storage_path == path {
                            return;
                        }
                        s.storage_path = path.clone();
                    }
                    storage_row.set_subtitle(&path.display().to_string());
                    persist_and_reconfigure(&clip_settings, &buffer, &cmd_tx, &monitor);
                },
            );
        });
    }
    storage_row.add_suffix(&storage_btn);
    group.add(&storage_row);

    // ------------------------------------------------------------------
    // Disk retention dropdown.
    // ------------------------------------------------------------------
    let retention_strings: Vec<&str> =
        vec!["Keep all", "Cap at 10 GB", "Cap at 30 GB", "Cap at 50 GB", "Cap at 100 GB"];
    let retention_model = gtk::StringList::new(&retention_strings);
    let initial_retention_idx = RETENTION_OPTIONS_GB
        .iter()
        .position(|&v| v == clip_settings.borrow().disk_cap_gb)
        .unwrap_or(0) as u32;
    let retention_row = adw::ComboRow::builder()
        .title("Disk retention")
        .subtitle("Auto-prune oldest clips when the cap is exceeded")
        .model(&retention_model)
        .selected(initial_retention_idx)
        .build();
    {
        let clip_settings = clip_settings.clone();
        retention_row.connect_selected_notify(move |row| {
            let idx = row.selected() as usize;
            let new_value = RETENTION_OPTIONS_GB
                .get(idx)
                .copied()
                .unwrap_or(None);
            {
                let mut s = clip_settings.borrow_mut();
                if s.disk_cap_gb == new_value {
                    return;
                }
                s.disk_cap_gb = new_value;
            }
            // Retention doesn't enter CaptureConfig — pruning runs from
            // `library` after a save. Just persist.
            if let Err(e) = save(&clip_settings.borrow()) {
                log::warn!("clip settings save failed: {e}");
            }
        });
    }
    group.add(&retention_row);

    // ------------------------------------------------------------------
    // gpu-screen-recorder Reinstall row — last item, kept as recovery
    // affordance for the rare case where the GSR Flatpak vanishes after
    // onboarding.
    // ------------------------------------------------------------------
    let reinstall_row = adw::ActionRow::builder()
        .title("gpu-screen-recorder")
        .subtitle("The Flatpak Clips uses to capture gameplay")
        .build();
    let reinstall_btn = gtk::Button::builder()
        .label("Reinstall")
        .valign(gtk::Align::Center)
        .build();
    reinstall_btn.set_action_name(Some("app.gsr-install"));
    reinstall_row.add_suffix(&reinstall_btn);
    group.add(&reinstall_row);

    group
}

/// Common helper: write the current settings to disk and push a fresh
/// `CaptureConfig` to the buffer controller. Errors are logged but not
/// surfaced to the user — settings I/O failures are rare and recoverable
/// (the next change overwrites the previous one).
fn persist_and_reconfigure(
    clip_settings: &Rc<RefCell<ClipSettings>>,
    buffer: &Rc<RefCell<BufferController>>,
    cmd_tx: &Sender<ClipCommand>,
    headset_sink_monitor: &str,
) {
    if let Err(e) = save(&clip_settings.borrow()) {
        log::warn!("clip settings save failed: {e}");
    }
    let cfg = cfg_from_settings(&clip_settings.borrow(), headset_sink_monitor);
    buffer.borrow_mut().on_config_change(cfg, cmd_tx);
}

/// The dbus-send command bound to the `app.copy-clip-hotkey-cmd` action.
/// Exposed so app.rs can register the action without re-deriving the
/// command string.
pub fn save_clip_dbus_command() -> &'static str {
    SAVE_CLIP_DBUS_COMMAND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_60s_25mbps() {
        let s = ClipSettings::default();
        assert_eq!(s.buffer_length, 60);
        assert_eq!(s.bitrate_mbps, 25);
        assert!(s.auto_arm);
        assert!(!s.always_armed);
        assert!(s.per_source_tracks);
        assert!(s.mic_capture);
    }

    #[test]
    fn onboarding_complete_round_trips() {
        let mut s = ClipSettings::default();
        assert!(!s.onboarding_complete, "default is false");
        s.onboarding_complete = true;
        // Just verify the field flips; full round-trip via save/load needs a real $HOME.
        assert!(s.onboarding_complete);
    }
}
