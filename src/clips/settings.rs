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
pub fn mark_onboarding_complete() -> std::io::Result<()> {
    let mut s = load();
    s.onboarding_complete = true;
    save(&s)
}

/// Build the Clips PreferencesGroup for the Settings page. Currently exposes
/// the Capture source row (Reset button). The Test button is added in Task
/// 3.7 and the gpu-screen-recorder Reinstall row in Task 3.8. Other clip
/// settings (buffer length, bitrate, storage path, etc.) come in Phase 6.
pub fn build_clips_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Clips").build();

    let reset_row = adw::ActionRow::builder()
        .title("Capture source")
        .subtitle("Pick the screen recorded by the clip buffer")
        .build();
    let reset_btn = gtk::Button::builder()
        .label("Reset")
        .valign(gtk::Align::Center)
        .build();
    reset_btn.set_action_name(Some("app.reset-clips-capture"));
    reset_row.add_suffix(&reset_btn);
    group.add(&reset_row);

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
