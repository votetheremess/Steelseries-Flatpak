use std::process::Command;

use super::sinks::ALL_SINKS;

pub struct AudioRouter {
    loopback_ids: Vec<u32>,
}

impl AudioRouter {
    pub fn create(headset_sink: &str) -> Result<Self, String> {
        // Clean up any orphaned loopback modules from a previous session (including
        // legacy names) before creating new ones. Multiple loopbacks reading from
        // the same monitor source cause audio feedback / staticky hum.
        cleanup_orphaned_loopbacks();

        let mut loopback_ids = Vec::with_capacity(ALL_SINKS.len());
        for (name, _) in ALL_SINKS {
            let id = load_loopback(name, headset_sink)?;
            log::info!("Created loopback for {name} (module {id})");
            loopback_ids.push(id);
        }

        Ok(AudioRouter { loopback_ids })
    }

    pub fn destroy(&self) {
        for id in &self.loopback_ids {
            unload_module(*id);
        }
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

/// Find any existing module-loopback instances whose source is one of our
/// managed names (current sinks, sources, or legacy names) and unload them.
/// Called before creating fresh loopbacks to prevent duplicates accumulating
/// and causing audio feedback. Also catches loopbacks from previous versions
/// where some nodes were sinks that are now sources (e.g. Mic).
fn cleanup_orphaned_loopbacks() {
    use super::sinks::{ALL_SOURCES, LEGACY_SINK_MIGRATIONS};

    let Ok(output) = Command::new("pactl")
        .args(["list", "modules", "short"])
        .output()
    else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Collect every managed name — current sinks, current sources, and legacy
    let mut known: Vec<&str> = ALL_SINKS.iter().map(|(n, _)| *n).collect();
    known.extend(ALL_SOURCES.iter().map(|(n, _)| *n));
    known.extend(LEGACY_SINK_MIGRATIONS.iter().map(|(old, _)| *old));

    let mut cleaned = 0;
    for line in stdout.lines() {
        // Lines look like: `<id>\tmodule-loopback\tsource=<name>.monitor sink=<target> ...`
        if !line.contains("module-loopback") {
            continue;
        }
        let referenced = known.iter().any(|name| {
            line.contains(&format!("source={name}.monitor"))
                || line.contains(&format!("source={name}"))
        });
        if !referenced {
            continue;
        }
        let Some(id_str) = line.split_whitespace().next() else {
            continue;
        };
        let Ok(id) = id_str.parse::<u32>() else {
            continue;
        };
        log::warn!("Unloading orphaned loopback module {id}: {line}");
        unload_module(id);
        cleaned += 1;
    }
    if cleaned > 0 {
        log::info!("Cleaned up {cleaned} orphaned loopback(s)");
    }
}
