use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

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
pub(crate) struct SinkInput {
    pub(crate) id: u32,
    pub(crate) sink_id: u32,
    pub(crate) app_name: String,
}

pub(crate) const SUPPRESSION_WINDOW: Duration = Duration::from_millis(3000);

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
/// Merge rules (monotonic):
/// - Apps currently on a ChatMix sink → save/update their entry
/// - Apps currently running but NOT on a managed sink → leave saved entry alone
/// - Apps not currently running → keep their previous entry untouched
///
/// Move-off-managed is no longer treated as a "forget me" signal here. The
/// state-sync tick is the single source of truth for user-initiated moves;
/// it writes through `update_saved_assignment` on every observed user move,
/// and only ever ADDS / UPDATES entries (never deletes). This keeps the save
/// file monotonic so a transient PulseAudio glitch reporting an unmanaged sink
/// can't erase the user's preferences.
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
        }
        // else: monotonic — leave existing entry intact
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

// ---------------------------------------------------------------------------
// Stream-state reconciliation (Issue 1: persist user moves between Phase-1's
// initial restore and shutdown's save_assignments)
// ---------------------------------------------------------------------------
//
// Replaces the old new-stream watcher (`restore_new_streams` + `seen_ids`)
// with a per-tick reconciler that:
//   1. routes newly observed streams whose app has a saved target;
//   2. observes user-driven moves and writes them through;
//   3. suppresses our own moves for a window so the next pactl tick doesn't
//      look like the user moving the stream back to where it was;
//   4. garbage-collects state for streams that have died.
//
// The decision logic is split out into `reconcile_pure` /
// `reconcile_pure_with_updates` so we can drive it from unit tests without
// shelling out to pactl.

/// Pure decision function: given inputs / state, append the moves to perform.
/// No saved-assignment updates produced. No I/O.
pub(crate) fn reconcile_pure(
    inputs: &[SinkInput],
    sinks_by_id: &HashMap<u32, String>,
    saved: &HashMap<String, String>,
    tracked: &mut HashMap<u32, String>,
    pre: &mut HashMap<u32, (String, Instant)>,
    moves: &mut Vec<(u32, String)>,
    now: Instant,
) {
    let mut updates_buf = Vec::new();
    reconcile_pure_with_updates(inputs, sinks_by_id, saved, tracked, pre, moves, &mut updates_buf, now);
}

/// Full decision function: also reports saved-assignment updates from observed
/// user moves to managed sinks.
pub(crate) fn reconcile_pure_with_updates(
    inputs: &[SinkInput],
    sinks_by_id: &HashMap<u32, String>,
    saved: &HashMap<String, String>,
    tracked: &mut HashMap<u32, String>,
    pre: &mut HashMap<u32, (String, Instant)>,
    moves: &mut Vec<(u32, String)>,
    updates: &mut Vec<(String, String)>,
    now: Instant,
) {
    let mut live_ids: HashSet<u32> = HashSet::new();
    for input in inputs {
        live_ids.insert(input.id);
        let Some(current_sink) = sinks_by_id.get(&input.sink_id) else {
            continue;
        };

        if !tracked.contains_key(&input.id) {
            // Vacant — first time we're seeing this stream.
            // Never touch our own EQ filter-chain streams (would create a
            // feedback loop: sink → filter → sink → ...).
            if input.app_name == "pw-cli" {
                continue;
            }
            if let Some(target) = saved.get(&input.app_name) {
                if current_sink != target {
                    moves.push((input.id, target.clone()));
                    tracked.insert(input.id, target.clone());
                    pre.insert(input.id, (current_sink.clone(), now));
                } else {
                    tracked.insert(input.id, target.clone());
                }
            } else {
                tracked.insert(input.id, current_sink.clone());
            }
        } else {
            // Occupied — we've seen this stream before. Decide whether the
            // sink change vs tracked is our own move (suppress) or the user's
            // (persist).
            let tracked_sink = tracked[&input.id].clone();
            if &tracked_sink == current_sink {
                continue;
            }

            let suppress = matches!(pre.get(&input.id), Some((pre_sink, t))
                if pre_sink == current_sink && now.duration_since(*t) < SUPPRESSION_WINDOW);
            if suppress {
                continue;
            }

            if is_managed(current_sink) {
                updates.push((input.app_name.clone(), current_sink.clone()));
                tracked.insert(input.id, current_sink.clone());
                pre.remove(&input.id);
            }
            // else: monotonic — silently ignore move-off-managed
        }
    }
    pre.retain(|_, (_, t)| now.duration_since(*t) < SUPPRESSION_WINDOW);
    tracked.retain(|id, _| live_ids.contains(id));
    pre.retain(|id, _| live_ids.contains(id));
}

/// I/O wrapper around `reconcile_pure_with_updates`: queries pactl for the
/// current state and applies any moves / saved-assignment updates the pure
/// decision function emits.
pub fn reconcile_stream_state(
    tracked: &mut HashMap<u32, String>,
    self_move_pre_sink: &mut HashMap<u32, (String, Instant)>,
) {
    let saved = read_saved_for_path();
    let sinks_by_id = list_sinks_by_id();
    let inputs = list_sink_inputs();

    let mut moves = Vec::new();
    let mut updates = Vec::new();
    reconcile_pure_with_updates(
        &inputs,
        &sinks_by_id,
        &saved,
        tracked,
        self_move_pre_sink,
        &mut moves,
        &mut updates,
        Instant::now(),
    );

    for (id, target) in moves {
        match move_sink_input(id, &target) {
            Ok(()) => log::info!("Auto-routed stream {id} → {target}"),
            Err(e) => {
                log::warn!("Auto-route {id} → {target} failed: {e}");
                // Drop both maps' entries for this id. Next tick will treat it as
                // Vacant and either retry the move or accept the stream where it
                // ended up. Simpler and safer than guessing the rollback target.
                tracked.remove(&id);
                self_move_pre_sink.remove(&id);
            }
        }
    }
    for (app, sink) in updates {
        update_saved_assignment(&app, &sink);
    }
}

fn read_saved_for_path() -> HashMap<String, String> {
    match config_path() {
        Some(p) => read_saved(&p),
        None => HashMap::new(),
    }
}

/// Monotonic add/update of a single app→sink saved entry.
pub fn update_saved_assignment(app: &str, sink: &str) {
    let Some(path) = config_path() else {
        return;
    };
    let mut saved = read_saved(&path);
    saved.insert(app.into(), sink.into());
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let content: String = saved.iter().map(|(a, s)| format!("{a}\t{s}\n")).collect();
    if let Err(e) = fs::write(&path, content) {
        log::warn!("update_saved_assignment failed: {e}");
    }
}

/// Snapshot of sink-input ID → sink_name at startup time. Used to seed the
/// state-sync tick's `tracked` map so the first reconcile doesn't re-process
/// streams that the startup `restore_assignments` already handled.
pub fn initial_tracked() -> HashMap<u32, String> {
    let sinks_by_id = list_sinks_by_id();
    let mut out = HashMap::new();
    for input in list_sink_inputs() {
        if let Some(sink_name) = sinks_by_id.get(&input.sink_id) {
            out.insert(input.id, sink_name.clone());
        }
    }
    out
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
mod reconcile_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn input(id: u32, sink_id: u32, app: &str) -> SinkInput {
        SinkInput { id, sink_id, app_name: app.into() }
    }

    #[test]
    fn vacant_with_saved_routes_and_seeds_suppression() {
        let inputs = vec![input(10, 1, "Tidal")];
        let mut sinks_by_id = HashMap::new();
        sinks_by_id.insert(1u32, "SteelSeries_Game".to_string());
        let mut saved = HashMap::new();
        saved.insert("Tidal".into(), "SteelSeries_Music".into());

        let mut tracked = HashMap::new();
        let mut pre = HashMap::new();
        let mut moves = Vec::new();

        reconcile_pure(&inputs, &sinks_by_id, &saved, &mut tracked, &mut pre, &mut moves, Instant::now());

        assert_eq!(moves, vec![(10u32, "SteelSeries_Music".to_string())]);
        assert_eq!(tracked.get(&10), Some(&"SteelSeries_Music".into()));
        assert_eq!(pre.get(&10).map(|(s, _)| s.as_str()), Some("SteelSeries_Game"));
    }

    #[test]
    fn occupied_skips_within_suppression_window_when_pre_matches() {
        let inputs = vec![input(10, 1, "Tidal")];
        let mut sinks_by_id = HashMap::new();
        sinks_by_id.insert(1u32, "SteelSeries_Game".to_string());  // pactl stale
        let saved = HashMap::new();  // no longer in saved or different irrelevant

        let mut tracked = HashMap::new();
        tracked.insert(10u32, "SteelSeries_Music".to_string());
        let mut pre = HashMap::new();
        pre.insert(10u32, ("SteelSeries_Game".to_string(), Instant::now()));  // fresh suppression

        let mut moves = Vec::new();
        reconcile_pure(&inputs, &sinks_by_id, &saved, &mut tracked, &mut pre, &mut moves, Instant::now());

        assert!(moves.is_empty(), "should not re-route during suppression");
        assert_eq!(tracked.get(&10), Some(&"SteelSeries_Music".into()), "tracked unchanged during suppression");
        assert!(pre.contains_key(&10), "suppression entry retained for multi-tick lag");
    }

    #[test]
    fn occupied_user_move_to_managed_persists() {
        let inputs = vec![input(10, 2, "Tidal")];
        let mut sinks_by_id = HashMap::new();
        sinks_by_id.insert(2u32, "SteelSeries_Music".to_string());
        let saved = HashMap::new();

        let mut tracked = HashMap::new();
        tracked.insert(10u32, "SteelSeries_Game".to_string());
        let mut pre = HashMap::new();

        let mut moves = Vec::new();
        let mut updates = Vec::new();

        reconcile_pure_with_updates(
            &inputs, &sinks_by_id, &saved,
            &mut tracked, &mut pre, &mut moves, &mut updates,
            Instant::now(),
        );

        assert!(moves.is_empty());
        assert_eq!(updates, vec![("Tidal".to_string(), "SteelSeries_Music".to_string())]);
        assert_eq!(tracked.get(&10), Some(&"SteelSeries_Music".into()));
    }

    #[test]
    fn occupied_user_move_to_unmanaged_silently_ignored() {
        let inputs = vec![input(10, 3, "Tidal")];
        let mut sinks_by_id = HashMap::new();
        sinks_by_id.insert(3u32, "alsa_output.laptop_speakers".to_string());
        let saved = HashMap::new();

        let mut tracked = HashMap::new();
        tracked.insert(10u32, "SteelSeries_Music".to_string());
        let mut pre = HashMap::new();

        let mut moves = Vec::new();
        let mut updates = Vec::new();

        reconcile_pure_with_updates(
            &inputs, &sinks_by_id, &saved,
            &mut tracked, &mut pre, &mut moves, &mut updates,
            Instant::now(),
        );

        assert!(moves.is_empty());
        assert!(updates.is_empty(), "monotonic: move-off-managed does not destroy saved entry");
    }

    #[test]
    fn suppression_expires_after_window() {
        let inputs = vec![input(10, 1, "Tidal")];
        let mut sinks_by_id = HashMap::new();
        sinks_by_id.insert(1u32, "SteelSeries_Game".to_string());
        let saved = HashMap::new();

        let mut tracked = HashMap::new();
        tracked.insert(10u32, "SteelSeries_Music".to_string());
        let mut pre = HashMap::new();
        // Backdate suppression entry
        let stale_t = Instant::now() - Duration::from_millis(4000);
        pre.insert(10u32, ("SteelSeries_Game".to_string(), stale_t));

        let mut moves = Vec::new();
        let mut updates = Vec::new();

        reconcile_pure_with_updates(
            &inputs, &sinks_by_id, &saved,
            &mut tracked, &mut pre, &mut moves, &mut updates,
            Instant::now(),
        );

        // Window expired (4s > 3s SUPPRESSION_WINDOW); processed as user move to managed.
        assert_eq!(updates, vec![("Tidal".to_string(), "SteelSeries_Game".to_string())]);
        assert!(!pre.contains_key(&10), "suppression entry removed after consumption");
    }

    #[test]
    fn dead_ids_are_garbage_collected() {
        let inputs: Vec<SinkInput> = vec![];  // stream 10 is gone
        let sinks_by_id = HashMap::new();
        let saved = HashMap::new();

        let mut tracked = HashMap::new();
        tracked.insert(10u32, "SteelSeries_Music".to_string());
        let mut pre = HashMap::new();
        pre.insert(10u32, ("SteelSeries_Game".to_string(), Instant::now()));

        let mut moves = Vec::new();
        reconcile_pure(&inputs, &sinks_by_id, &saved, &mut tracked, &mut pre, &mut moves, Instant::now());

        assert!(tracked.is_empty(), "dead stream id removed from tracked");
        assert!(pre.is_empty(), "dead stream id removed from suppression");
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
