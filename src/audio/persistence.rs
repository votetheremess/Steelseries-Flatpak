use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use super::sinks::{CHAT_SINK_NAME, GAME_SINK_NAME};

const CONFIG_DIR: &str = "arctis-chatmix";
const CONFIG_FILE: &str = "assignments.txt";

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join(CONFIG_DIR).join(CONFIG_FILE))
}

#[derive(Debug)]
struct SinkInput {
    id: u32,
    sink_id: u32,
    app_name: String,
}

/// Parse `pactl list sink-inputs` (verbose) into a structured list.
fn list_sink_inputs() -> Vec<SinkInput> {
    let Ok(output) = Command::new("pactl").args(["list", "sink-inputs"]).output() else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&output.stdout);

    let mut inputs = Vec::new();
    let mut current_id: Option<u32> = None;
    let mut current_sink: Option<u32> = None;
    let mut current_app: Option<String> = None;

    let push_if_complete = |inputs: &mut Vec<SinkInput>,
                            id: &mut Option<u32>,
                            sink: &mut Option<u32>,
                            app: &mut Option<String>| {
        if let (Some(i), Some(s), Some(a)) = (id.take(), sink.take(), app.take()) {
            inputs.push(SinkInput {
                id: i,
                sink_id: s,
                app_name: a,
            });
        }
    };

    for line in text.lines() {
        if let Some(id_str) = line.strip_prefix("Sink Input #") {
            push_if_complete(&mut inputs, &mut current_id, &mut current_sink, &mut current_app);
            current_id = id_str.trim().parse().ok();
        } else {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("Sink: ") {
                current_sink = rest.trim().parse().ok();
            } else if let Some(rest) = trimmed.strip_prefix("application.name = \"") {
                current_app = Some(rest.trim_end_matches('"').to_string());
            }
        }
    }
    push_if_complete(&mut inputs, &mut current_id, &mut current_sink, &mut current_app);
    inputs
}

/// Map sink IDs to sink names by parsing `pactl list sinks short`.
fn list_sinks_by_id() -> HashMap<u32, String> {
    let Ok(output) = Command::new("pactl").args(["list", "sinks", "short"]).output() else {
        return HashMap::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let id: u32 = parts.next()?.parse().ok()?;
            let name = parts.next()?.to_string();
            Some((id, name))
        })
        .collect()
}

/// Save the current set of apps assigned to ChatMix sinks to a config file.
pub fn save_assignments() {
    let sinks = list_sinks_by_id();
    let inputs = list_sink_inputs();

    let mut assignments: HashMap<String, String> = HashMap::new();
    for input in inputs {
        let Some(sink_name) = sinks.get(&input.sink_id) else {
            continue;
        };
        if sink_name == GAME_SINK_NAME || sink_name == CHAT_SINK_NAME {
            assignments.insert(input.app_name, sink_name.clone());
        }
    }

    let Some(path) = config_path() else {
        log::warn!("Could not determine config path");
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let content: String = assignments
        .iter()
        .map(|(app, sink)| format!("{app}\t{sink}\n"))
        .collect();

    match fs::write(&path, content) {
        Ok(()) => log::info!(
            "Saved {} app assignment(s) to {}",
            assignments.len(),
            path.display()
        ),
        Err(e) => log::warn!("Failed to save assignments to {}: {e}", path.display()),
    }
}

/// Read saved app→sink assignments and move matching sink-inputs back to ChatMix sinks.
pub fn restore_assignments() {
    let Some(path) = config_path() else {
        return;
    };
    let Ok(content) = fs::read_to_string(&path) else {
        log::debug!("No saved assignments at {}", path.display());
        return;
    };

    let saved: HashMap<String, String> = content
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let app = parts.next()?.to_string();
            let sink = parts.next()?.to_string();
            Some((app, sink))
        })
        .collect();

    if saved.is_empty() {
        return;
    }

    let inputs = list_sink_inputs();
    let mut moved = 0;
    for input in inputs {
        if let Some(target_sink) = saved.get(&input.app_name) {
            match move_sink_input(input.id, target_sink) {
                Ok(()) => {
                    log::info!("Restored {} → {}", input.app_name, target_sink);
                    moved += 1;
                }
                Err(e) => log::warn!(
                    "Failed to restore {} → {}: {e}",
                    input.app_name,
                    target_sink
                ),
            }
        }
    }
    if moved > 0 {
        log::info!("Restored {moved}/{} app assignment(s)", saved.len());
    }
}

fn move_sink_input(id: u32, sink: &str) -> Result<(), String> {
    let output = Command::new("pactl")
        .args(["move-sink-input", &id.to_string(), sink])
        .output()
        .map_err(|e| format!("pactl: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    Ok(())
}
