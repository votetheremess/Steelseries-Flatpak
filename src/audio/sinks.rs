use std::process::Command;

pub const GAME_SINK_NAME: &str = "ChatMix_Game";
pub const CHAT_SINK_NAME: &str = "ChatMix_Chat";

pub struct VirtualSinks {
    _created: bool, // prevent manual construction
}

impl VirtualSinks {
    pub fn create() -> Result<Self, String> {
        // Clean up any orphaned sinks from a previous crash
        cleanup_orphaned();

        create_pw_sink(GAME_SINK_NAME, "ChatMix Game")?;
        log::info!("Created Game sink");

        create_pw_sink(CHAT_SINK_NAME, "ChatMix Chat")?;
        log::info!("Created Chat sink");

        Ok(VirtualSinks { _created: true })
    }

    pub fn destroy(&self) {
        destroy_pw_sink(GAME_SINK_NAME);
        destroy_pw_sink(CHAT_SINK_NAME);
        log::info!("Virtual sinks destroyed");
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

fn create_pw_sink(node_name: &str, description: &str) -> Result<(), String> {
    let props = format!(
        "{{ factory.name=support.null-audio-sink node.name={node_name} node.description=\"{description}\" media.class=Audio/Sink object.linger=true audio.position=[FL,FR] monitor.channel-volumes=true monitor.passthrough=true }}"
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

fn destroy_pw_sink(node_name: &str) {
    if let Err(e) = Command::new("pw-cli")
        .args(["destroy", node_name])
        .output()
    {
        log::warn!("Failed to destroy sink {node_name}: {e}");
    }
}

fn cleanup_orphaned() {
    destroy_pw_sink(GAME_SINK_NAME);
    destroy_pw_sink(CHAT_SINK_NAME);
}
