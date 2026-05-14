use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use super::sinks::{is_managed, migrate_legacy_name};

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

/// Save the current set of apps assigned to ChatMix sinks to a config file,
/// merging with any existing saved entries.
///
/// Merge rules:
/// - Apps currently on a ChatMix sink → save/update their entry
/// - Apps currently running but NOT on a ChatMix sink → remove from save (user moved them out)
/// - Apps not currently running → keep their previous entry untouched
///
/// This way, launching and closing the app without interacting doesn't wipe
/// the save file, but intentionally moving an app off ChatMix is respected.
pub fn save_assignments() {
    let Some(path) = config_path() else {
        log::warn!("Could not determine config path");
        return;
    };

    // Start from any existing saved data
    let mut assignments: HashMap<String, String> = read_saved(&path);

    let sinks = list_sinks_by_id();
    let inputs = list_sink_inputs();

    for input in inputs {
        let Some(sink_name) = sinks.get(&input.sink_id) else {
            continue;
        };
        if is_managed(sink_name) {
            // Currently on a managed sink → update the saved entry
            assignments.insert(input.app_name, sink_name.clone());
        } else {
            // Currently running but NOT on a managed sink → user moved it out, forget it
            assignments.remove(&input.app_name);
        }
    }

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

fn read_saved(path: &std::path::Path) -> HashMap<String, String> {
    let Ok(content) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let app = parts.next()?.to_string();
            let sink = parts.next()?.to_string();
            // Translate legacy sink names (ChatMix_* → SteelSeries_*) on load
            let sink = match migrate_legacy_name(&sink) {
                Some(new_name) => {
                    log::info!("Migrating legacy assignment: {app} {sink} → {new_name}");
                    new_name.to_string()
                }
                None => sink,
            };
            Some((app, sink))
        })
        .collect()
}

/// Read saved app→sink assignments and move matching sink-inputs back to ChatMix sinks.
pub fn restore_assignments() {
    let Some(path) = config_path() else {
        return;
    };
    let saved = read_saved(&path);

    if saved.is_empty() {
        log::debug!("No saved assignments at {}", path.display());
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

/// Returns the set of sink-input IDs currently active.
/// Used to seed the watcher's "seen" set so we don't re-process existing streams.
pub fn initial_seen_ids() -> HashSet<u32> {
    list_sink_inputs().into_iter().map(|i| i.id).collect()
}

/// Polled periodically to detect newly appeared sink-inputs and route them
/// to their saved ChatMix sinks. Only acts on IDs not yet in `seen_ids`.
///
/// This respects user intent: once an ID has been seen, we never touch it
/// again, so if the user manually moves a stream off ChatMix, it stays.
pub fn restore_new_streams(seen_ids: &mut HashSet<u32>) {
    let Some(path) = config_path() else {
        return;
    };
    let saved = read_saved(&path);
    if saved.is_empty() {
        // Still need to track new IDs so if saved gets populated later,
        // we don't retroactively move old streams.
        for input in list_sink_inputs() {
            seen_ids.insert(input.id);
        }
        return;
    }

    let sinks_by_id = list_sinks_by_id();
    let inputs = list_sink_inputs();

    for input in inputs {
        // insert() returns true if the value was newly inserted.
        let was_new = seen_ids.insert(input.id);
        if !was_new {
            continue;
        }

        // Never touch our own EQ filter-chain streams — moving them would
        // create a feedback loop (sink → filter → sink → filter → …).
        if input.app_name == "pw-cli" {
            continue;
        }

        let Some(target_sink) = saved.get(&input.app_name) else {
            continue;
        };

        let current_sink_name = sinks_by_id.get(&input.sink_id).map(String::as_str);
        if current_sink_name == Some(target_sink.as_str()) {
            continue; // already on the right sink
        }

        match move_sink_input(input.id, target_sink) {
            Ok(()) => log::info!(
                "Auto-routed new stream {} → {}",
                input.app_name,
                target_sink
            ),
            Err(e) => log::warn!(
                "Failed to auto-route {} → {}: {e}",
                input.app_name,
                target_sink
            ),
        }
    }
}

/// Delete the saved assignments file, if it exists.
pub fn clear_saved() -> Result<(), String> {
    let Some(path) = config_path() else {
        return Err("Could not determine config path".into());
    };
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("Failed to delete {path:?}: {e}"))?;
        log::info!("Cleared saved assignments at {}", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mixer routing persistence (channel → device)
// ---------------------------------------------------------------------------

const MIXER_ROUTING_FILE: &str = "mixer_routing.txt";

fn mixer_routing_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join(CONFIG_DIR).join(MIXER_ROUTING_FILE))
}

/// Load saved mixer routing: channel_name → device_name.
/// Skips the "mic" line — it's a 5-field record loaded separately via
/// `load_mixer_routing_mic`.
pub fn load_mixer_routing() -> HashMap<String, String> {
    let Some(path) = mixer_routing_path() else {
        return HashMap::new();
    };
    parse_mixer_routing_from(&path)
}

pub(crate) fn parse_mixer_routing_from(path: &std::path::Path) -> HashMap<String, String> {
    let Ok(content) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let channel = parts.next()?.to_string();
            if channel == "mic" {
                return None; // 5-field record, read separately
            }
            let device = parts.next()?.to_string();
            Some((channel, device))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Mic preference (5-field mic line in mixer_routing.txt)
// ---------------------------------------------------------------------------
//
// The mic line in mixer_routing.txt carries extra stable-id columns so we can
// re-attach to the same physical microphone after a USB-port change, Bluetooth
// reconnect, or PipeWire node-name renaming. Sink lines stay 2-field; only the
// mic record uses 5 fields:
//
//   mic <TAB> node_name <TAB> product_name <TAB> bus_path <TAB> bluez_address
//
// All four mic columns are optional; legacy 2- or 3-field forms parse cleanly.
// Empty bus_path / bluez_address are normal for ALSA / non-Bluetooth devices.

#[derive(Debug, Clone, Default)]
pub struct MicPreference {
    pub node_name: String,
    pub product_name: String,
    pub bus_path: String,
    pub bluez_address: String,
}

impl MicPreference {
    pub fn from_fields(node: String, product: String, bus_path: String, bluez: String) -> Self {
        Self {
            node_name: node,
            product_name: product,
            bus_path,
            bluez_address: bluez,
        }
    }
}

pub(crate) fn parse_mic_preference_from(path: &std::path::Path) -> Option<MicPreference> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let mut parts = line.split('\t');
        if parts.next()? != "mic" {
            continue;
        }
        let node_name = parts.next()?.to_string();
        let product_name = parts.next().unwrap_or("").to_string();
        let bus_path = parts.next().unwrap_or("").to_string();
        let bluez_address = parts.next().unwrap_or("").to_string();
        return Some(MicPreference {
            node_name,
            product_name,
            bus_path,
            bluez_address,
        });
    }
    None
}

pub fn load_mixer_routing_mic() -> Option<MicPreference> {
    parse_mic_preference_from(&mixer_routing_path()?)
}

pub fn save_mixer_routing_mic(pref: &MicPreference) {
    let Some(path) = mixer_routing_path() else {
        log::warn!("Could not determine mixer routing path");
        return;
    };
    // load_mixer_routing() skips the mic line by design, so reading + writing
    // sink entries here is safe (won't clobber the 5-field record we're about
    // to write).
    let routing = load_mixer_routing();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut content = String::new();
    for (ch, dev) in &routing {
        content.push_str(&format!("{ch}\t{dev}\n"));
    }
    content.push_str(&format!(
        "mic\t{}\t{}\t{}\t{}\n",
        pref.node_name, pref.product_name, pref.bus_path, pref.bluez_address
    ));
    if let Err(e) = fs::write(&path, content) {
        log::warn!("Failed to save mixer routing (mic): {e}");
    }
}

/// Save a single mixer routing entry (merge with existing file). Preserves
/// any 5-field mic record so a sink reroute doesn't clobber the user's mic
/// preference.
pub fn save_mixer_routing_entry(channel: &str, device: &str) {
    let Some(path) = mixer_routing_path() else {
        log::warn!("Could not determine mixer routing path");
        return;
    };

    let mut routing = load_mixer_routing();
    routing.insert(channel.to_string(), device.to_string());

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut content = String::new();
    for (ch, dev) in &routing {
        content.push_str(&format!("{ch}\t{dev}\n"));
    }
    // Preserve the 5-field mic line if present.
    if let Some(pref) = parse_mic_preference_from(&path) {
        content.push_str(&format!(
            "mic\t{}\t{}\t{}\t{}\n",
            pref.node_name, pref.product_name, pref.bus_path, pref.bluez_address
        ));
    }

    if let Err(e) = fs::write(&path, content) {
        log::warn!("Failed to save mixer routing: {e}");
    }
}

// ---------------------------------------------------------------------------
// Volume persistence for virtual sinks + mic source
// ---------------------------------------------------------------------------
//
// Our 4 virtual sinks + mic source are destroyed and recreated on every app
// launch (VirtualSinks::create), so PipeWire's own state store doesn't carry
// their volumes across restarts. We save them ourselves.
//
// NOT saved on purpose:
//   - Game / Chat — volumes are set by the ChatMix HID dial every event;
//     saving user-slider values would create a confusing override vs the dial.
//   - Master — that's the physical headset sink; its volume is managed by
//     PipeWire's own state persistence (WirePlumber), no need to duplicate.

const VOLUMES_FILE: &str = "volumes.txt";

fn volumes_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join(CONFIG_DIR).join(VOLUMES_FILE))
}

/// Load saved volumes: PipeWire node name → volume percent (0..=100).
pub fn load_volumes() -> HashMap<String, u32> {
    let Some(path) = volumes_path() else {
        return HashMap::new();
    };
    let Ok(content) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let channel = parts.next()?.to_string();
            let pct: u32 = parts.next()?.trim().parse().ok()?;
            Some((channel, pct.min(100)))
        })
        .collect()
}

/// Save a single volume entry (merge with existing file). `channel` is the
/// PipeWire node name (e.g. `SteelSeries_Music`, `SteelSeries_Mic`).
pub fn save_volume_entry(channel: &str, volume_percent: u32) {
    let Some(path) = volumes_path() else {
        log::warn!("Could not determine volumes path");
        return;
    };

    let mut volumes = load_volumes();
    volumes.insert(channel.to_string(), volume_percent.min(100));

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let content: String = volumes
        .iter()
        .map(|(ch, v)| format!("{ch}\t{v}\n"))
        .collect();

    if let Err(e) = fs::write(&path, content) {
        log::warn!("Failed to save volumes: {e}");
    }
}

#[cfg(test)]
mod mic_preference_tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_mixer_file(dir: &TempDir, content: &str) -> PathBuf {
        let path = dir.path().join(MIXER_ROUTING_FILE);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn load_mic_preference_5_field() {
        let dir = TempDir::new().unwrap();
        let path = write_mixer_file(
            &dir,
            "SteelSeries_Game\talsa_output.foo\nmic\tnode_x\tProductX\tusb-0000:00:14.0-3\t00:11:22:33:44:55\n",
        );
        let pref = parse_mic_preference_from(&path).unwrap();
        assert_eq!(pref.node_name, "node_x");
        assert_eq!(pref.product_name, "ProductX");
        assert_eq!(pref.bus_path, "usb-0000:00:14.0-3");
        assert_eq!(pref.bluez_address, "00:11:22:33:44:55");
    }

    #[test]
    fn load_mic_preference_legacy_2_field() {
        let dir = TempDir::new().unwrap();
        let path = write_mixer_file(&dir, "mic\tnode_legacy\n");
        let pref = parse_mic_preference_from(&path).unwrap();
        assert_eq!(pref.node_name, "node_legacy");
        assert_eq!(pref.product_name, "");
        assert_eq!(pref.bus_path, "");
        assert_eq!(pref.bluez_address, "");
    }

    #[test]
    fn load_mic_preference_legacy_3_field() {
        let dir = TempDir::new().unwrap();
        let path = write_mixer_file(&dir, "mic\tnode_x\tProductX\n");
        let pref = parse_mic_preference_from(&path).unwrap();
        assert_eq!(pref.node_name, "node_x");
        assert_eq!(pref.product_name, "ProductX");
        assert_eq!(pref.bus_path, "");
        assert_eq!(pref.bluez_address, "");
    }

    #[test]
    fn load_mixer_routing_skips_mic_lines() {
        let dir = TempDir::new().unwrap();
        let path = write_mixer_file(
            &dir,
            "SteelSeries_Game\talsa_a\nmic\tnode_x\tP\tB\tM\nSteelSeries_Aux\talsa_b\n",
        );
        let routing = parse_mixer_routing_from(&path);
        assert_eq!(routing.get("SteelSeries_Game").map(|s| s.as_str()), Some("alsa_a"));
        assert_eq!(routing.get("SteelSeries_Aux").map(|s| s.as_str()), Some("alsa_b"));
        assert!(routing.get("mic").is_none(), "mic must not be in homogeneous routing map");
    }
}
