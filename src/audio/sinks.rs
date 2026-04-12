use std::process::Command;

// Sinks that are volume-controlled by the HID dial
pub const GAME_SINK_NAME: &str = "SteelSeries_Game";
pub const CHAT_SINK_NAME: &str = "SteelSeries_Chat";

// Passive utility sinks (no app logic — user routes apps to them manually)
pub const MUSIC_SINK_NAME: &str = "SteelSeries_Music";
pub const AUX_SINK_NAME: &str = "SteelSeries_Aux";

// Virtual source (microphone) — appears as an input device
pub const MIC_SOURCE_NAME: &str = "SteelSeries_Mic";

/// Every virtual sink this app manages, with internal node.name and display description.
pub const ALL_SINKS: &[(&str, &str)] = &[
    (GAME_SINK_NAME, "SteelSeries Game"),
    (CHAT_SINK_NAME, "SteelSeries Chat"),
    (MUSIC_SINK_NAME, "SteelSeries Music"),
    (AUX_SINK_NAME, "SteelSeries Aux"),
];

/// Every virtual source (input) this app manages.
pub const ALL_SOURCES: &[(&str, &str)] = &[
    (MIC_SOURCE_NAME, "SteelSeries Mic"),
];

/// Legacy sink names from previous app versions. Still cleaned up on startup
/// and translated to the new names in `persistence::read_saved` so users who
/// upgrade don't lose their app→sink assignments. Mic used to be a sink; now
/// it's a source but the node.name is the same so old loopback cleanup still
/// finds it via `SteelSeries_Mic`.
pub const LEGACY_SINK_MIGRATIONS: &[(&str, &str)] = &[
    ("ChatMix_Game", GAME_SINK_NAME),
    ("ChatMix_Chat", CHAT_SINK_NAME),
    ("ChatMix_Music", MUSIC_SINK_NAME),
    ("ChatMix_Aux", AUX_SINK_NAME),
    ("ChatMix_Mic", MIC_SOURCE_NAME),
];

/// True if the given sink name is one the app manages as a sink (not a source).
/// Used by persistence to decide whether a sink-input assignment belongs to us.
pub fn is_managed(sink_name: &str) -> bool {
    ALL_SINKS.iter().any(|(name, _)| *name == sink_name)
}

/// If `old_name` is a legacy sink name, return the current name. Otherwise None.
pub fn migrate_legacy_name(old_name: &str) -> Option<&'static str> {
    LEGACY_SINK_MIGRATIONS
        .iter()
        .find(|(old, _)| *old == old_name)
        .map(|(_, new)| *new)
}

pub struct VirtualSinks {
    _created: bool,
}

impl VirtualSinks {
    pub fn create() -> Result<Self, String> {
        // Destroy any orphans from a previous crash BEFORE creating fresh ones
        cleanup_orphaned();

        for (name, description) in ALL_SINKS {
            create_pw_node(name, description, "Audio/Sink", "FL,FR")?;
            log::info!("Created sink {name}");
        }
        for (name, description) in ALL_SOURCES {
            create_pw_node(name, description, "Audio/Source/Virtual", "MONO")?;
            log::info!("Created source {name}");
        }

        Ok(VirtualSinks { _created: true })
    }

    pub fn destroy(&self) {
        for (name, _) in ALL_SINKS {
            destroy_pw_node(name);
        }
        for (name, _) in ALL_SOURCES {
            destroy_pw_node(name);
        }
        log::info!("Virtual sinks + sources destroyed");
    }
}

impl Drop for VirtualSinks {
    fn drop(&mut self) {
        self.destroy();
    }
}

pub fn set_sink_volume(sink_name: &str, volume_percent: u32) -> Result<(), String> {
    let output = Command::new("pactl")
        .args(["set-sink-volume", sink_name, &format!("{volume_percent}%")])
        .output()
        .map_err(|e| format!("Failed to run pactl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pactl set-sink-volume failed: {stderr}"));
    }
    Ok(())
}

fn create_pw_node(
    node_name: &str,
    description: &str,
    media_class: &str,
    audio_position: &str,
) -> Result<(), String> {
    let props = format!(
        "{{ factory.name=support.null-audio-sink node.name={node_name} node.description=\"{description}\" media.class={media_class} object.linger=true audio.position=[{audio_position}] monitor.channel-volumes=true monitor.passthrough=true }}"
    );

    let output = Command::new("pw-cli")
        .args(["create-node", "adapter", &props])
        .output()
        .map_err(|e| format!("Failed to run pw-cli: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pw-cli create-node failed: {stderr}"));
    }

    Ok(())
}

fn destroy_pw_node(node_name: &str) {
    if let Err(e) = Command::new("pw-cli")
        .args(["destroy", node_name])
        .output()
    {
        log::warn!("Failed to destroy node {node_name}: {e}");
    }
}

fn cleanup_orphaned() {
    // Destroy anything we manage now
    for (name, _) in ALL_SINKS {
        destroy_pw_node(name);
    }
    for (name, _) in ALL_SOURCES {
        destroy_pw_node(name);
    }
    // Destroy legacy names from previous app versions so they don't linger
    for (old_name, _) in LEGACY_SINK_MIGRATIONS {
        destroy_pw_node(old_name);
    }
}
