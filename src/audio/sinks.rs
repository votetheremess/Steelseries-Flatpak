use std::process::Command;

// Sinks that are volume-controlled by the HID dial
pub const GAME_SINK_NAME: &str = "SteelSeries_Game";
pub const CHAT_SINK_NAME: &str = "SteelSeries_Chat";

// Passive utility sinks (no app logic — user routes apps to them manually)
pub const MUSIC_SINK_NAME: &str = "SteelSeries_Music";
pub const AUX_SINK_NAME: &str = "SteelSeries_Aux";

// Virtual source (microphone) — created as Audio/Source/Virtual so it appears
// as a selectable input device in apps (Discord, OBS, etc.)
pub const MIC_SOURCE_NAME: &str = "SteelSeries_Mic";

/// Output sinks: internal node.name, display description, audio position.
pub const ALL_SINKS: &[(&str, &str, &str)] = &[
    (GAME_SINK_NAME, "SteelSeries Game", "FL,FR"),
    (CHAT_SINK_NAME, "SteelSeries Chat", "FL,FR"),
    (MUSIC_SINK_NAME, "SteelSeries Music", "FL,FR"),
    (AUX_SINK_NAME, "SteelSeries Aux", "FL,FR"),
];

/// Virtual source for the microphone. Created separately as Audio/Source/Virtual
/// (not Audio/Sink) because apps like Discord only discover proper Source nodes,
/// not sink monitors. Connected to the hardware mic via pw-link.
pub const ALL_SOURCES: &[(&str, &str, &str)] = &[
    (MIC_SOURCE_NAME, "SteelSeries Mic", "MONO"),
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

/// True if the given sink name is one the app manages as an output sink.
/// Used by persistence to decide whether a sink-input assignment belongs to us.
pub fn is_managed(sink_name: &str) -> bool {
    ALL_SINKS.iter().any(|(name, _, _)| *name == sink_name)
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
        cleanup_orphaned();

        for (name, description, position) in ALL_SINKS {
            create_pw_node(name, description, "Audio/Sink", position)?;
            log::info!("Created sink {name}");
        }
        for (name, description, position) in ALL_SOURCES {
            create_pw_node(name, description, "Audio/Source/Virtual", position)?;
            log::info!("Created source {name}");
        }

        Ok(VirtualSinks { _created: true })
    }

    pub fn destroy(&self) {
        for (name, _, _) in ALL_SINKS {
            destroy_pw_node(name);
        }
        for (name, _, _) in ALL_SOURCES {
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

pub fn set_source_volume(source_name: &str, volume_percent: u32) -> Result<(), String> {
    let output = Command::new("pactl")
        .args(["set-source-volume", source_name, &format!("{volume_percent}%")])
        .output()
        .map_err(|e| format!("Failed to run pactl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pactl set-source-volume failed: {stderr}"));
    }
    Ok(())
}

pub fn get_sink_volume(sink_name: &str) -> Result<u32, String> {
    let output = Command::new("pactl")
        .args(["get-sink-volume", sink_name])
        .output()
        .map_err(|e| format!("Failed to run pactl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pactl get-sink-volume failed: {stderr}"));
    }

    parse_volume_percent(&String::from_utf8_lossy(&output.stdout))
}

pub fn get_source_volume(source_name: &str) -> Result<u32, String> {
    let output = Command::new("pactl")
        .args(["get-source-volume", source_name])
        .output()
        .map_err(|e| format!("Failed to run pactl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pactl get-source-volume failed: {stderr}"));
    }

    parse_volume_percent(&String::from_utf8_lossy(&output.stdout))
}

/// Extract the first `NNN%` value from pactl volume output.
/// Format: `Volume: front-left: 65536 / 100% / 0.00 dB, ...`
fn parse_volume_percent(output: &str) -> Result<u32, String> {
    for word in output.split_whitespace() {
        if let Some(pct) = word.strip_suffix('%') {
            if let Ok(val) = pct.parse::<u32>() {
                return Ok(val);
            }
        }
    }
    Err("Could not parse volume percentage from pactl output".to_string())
}

/// List physical output sinks (excludes our virtual sinks and EQ filter-chain nodes).
/// Returns `(node_name, description)` pairs.
pub fn list_physical_sinks() -> Vec<(String, String)> {
    parse_device_list("sinks", |name| {
        ALL_SINKS.iter().any(|(n, _, _)| *n == name)
            || ALL_SOURCES.iter().any(|(n, _, _)| *n == name)
            || name.starts_with("eq_")
    })
}

/// List physical input sources (excludes our virtual source, monitors, and EQ nodes).
/// Returns `(node_name, description)` pairs.
pub fn list_physical_sources() -> Vec<(String, String)> {
    parse_device_list("sources", |name| {
        name == MIC_SOURCE_NAME
            || name.ends_with(".monitor")
            || name.starts_with("eq_")
            || ALL_SINKS.iter().any(|(n, _, _)| name == format!("{n}.monitor"))
    })
}

/// Parse `pactl list <kind>` output to extract (Name, Description) pairs,
/// filtering out entries where `exclude(name)` returns true.
fn parse_device_list<F: Fn(&str) -> bool>(kind: &str, exclude: F) -> Vec<(String, String)> {
    let Ok(output) = Command::new("pactl")
        .args(["list", kind])
        .output()
    else {
        return Vec::new();
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_desc: Option<String> = None;

    for line in stdout.lines() {
        let trimmed = line.trim();

        // New block starts with "Sink #" or "Source #"
        if trimmed.starts_with("Sink #") || trimmed.starts_with("Source #") {
            // Flush previous entry
            if let (Some(name), Some(desc)) = (current_name.take(), current_desc.take()) {
                if !exclude(&name) {
                    results.push((name, desc));
                }
            }
            current_name = None;
            current_desc = None;
            continue;
        }

        if let Some(val) = trimmed.strip_prefix("Name: ") {
            current_name = Some(val.to_string());
        } else if let Some(val) = trimmed.strip_prefix("Description: ") {
            current_desc = Some(val.to_string());
        }
    }

    // Flush last entry
    if let (Some(name), Some(desc)) = (current_name, current_desc) {
        if !exclude(&name) {
            results.push((name, desc));
        }
    }

    results
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
    for (name, _, _) in ALL_SINKS {
        destroy_pw_node(name);
    }
    for (name, _, _) in ALL_SOURCES {
        destroy_pw_node(name);
    }
    for (old_name, _) in LEGACY_SINK_MIGRATIONS {
        destroy_pw_node(old_name);
    }
}
