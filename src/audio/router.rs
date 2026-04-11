use std::process::Command;

use super::sinks::{CHAT_SINK_NAME, GAME_SINK_NAME};

pub struct AudioRouter {
    game_loopback_id: u32,
    chat_loopback_id: u32,
}

impl AudioRouter {
    pub fn create(headset_sink: &str) -> Result<Self, String> {
        let game_loopback_id = load_loopback(GAME_SINK_NAME, headset_sink)?;
        log::info!("Created Game loopback (module {game_loopback_id})");

        let chat_loopback_id = load_loopback(CHAT_SINK_NAME, headset_sink)?;
        log::info!("Created Chat loopback (module {chat_loopback_id})");

        Ok(AudioRouter {
            game_loopback_id,
            chat_loopback_id,
        })
    }

    pub fn destroy(&self) {
        unload_module(self.game_loopback_id);
        unload_module(self.chat_loopback_id);
        log::info!("Audio loopbacks destroyed");
    }
}

impl Drop for AudioRouter {
    fn drop(&mut self) {
        self.destroy();
    }
}

pub fn find_headset_sink() -> Result<String, String> {
    let output = Command::new("pactl")
        .args(["list", "sinks", "short"])
        .output()
        .map_err(|e| format!("Failed to run pactl: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("SteelSeries") && line.contains("Arctis_Nova_Elite") {
            if let Some(name) = line.split_whitespace().nth(1) {
                return Ok(name.to_string());
            }
        }
    }

    Err("Arctis Nova Elite audio sink not found in PipeWire/PulseAudio".to_string())
}

fn load_loopback(source_sink: &str, target_sink: &str) -> Result<u32, String> {
    let output = Command::new("pactl")
        .args([
            "load-module",
            "module-loopback",
            &format!("source={source_sink}.monitor"),
            &format!("sink={target_sink}"),
            "latency_msec=1",
        ])
        .output()
        .map_err(|e| format!("Failed to run pactl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pactl load-module loopback failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("Failed to parse module ID '{stdout}': {e}"))
}

fn unload_module(module_id: u32) {
    if let Err(e) = Command::new("pactl")
        .args(["unload-module", &module_id.to_string()])
        .output()
    {
        log::warn!("Failed to unload loopback module {module_id}: {e}");
    }
}
