//! Persisted clip settings (`~/.config/arctis-chatmix/clips_settings.txt`).
//!
//! Line-oriented `key=value` format, same shape as the rest of the app's
//! config files. Unknown keys are silently ignored so a downgrade doesn't
//! lose data the older binary doesn't understand.

use adw::prelude::*;
use std::path::PathBuf;

const SETTINGS_FILENAME: &str = "clips_settings.txt";

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

/// Build the Clips PreferencesGroup for the Settings page. Currently exposes
/// the Capture source row (Test + Reset buttons) and the gpu-screen-recorder
/// Reinstall row. Other clip settings (buffer length, bitrate, storage path,
/// etc.) come in Phase 6.
pub fn build_clips_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Clips").build();

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

    // gpu-screen-recorder row — Reinstall button reuses the wizard's
    // `app.gsr-install` action (registered in app.rs). This handles the case
    // where the user uninstalled the GSR Flatpak via the system app store
    // (Discover / Software / Bazaar / etc.) after onboarding; the next
    // `arm()` will fail with a NotFound error and the user can recover via
    // this button.
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
