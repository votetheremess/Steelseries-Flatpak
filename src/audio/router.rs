use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::eq::model::{Band, FilterType, EqTarget, SinkEq, NUM_BANDS};

use super::sinks::ALL_SINKS;

// ---------------------------------------------------------------------------
// AudioRouter — owns the EQ filter-chains (output sinks) + mic loopback
// ---------------------------------------------------------------------------

pub struct AudioRouter {
    eq_pipeline: EqPipeline,
    mic_linked: bool,
}

impl AudioRouter {
    pub fn create(headset_sink: &str) -> Result<Self, String> {
        cleanup_orphaned_loopbacks();

        // Load saved EQ state so filter-chains start with the right settings
        let saved_eq = crate::eq::presets::load_eq_state();

        // Start the EQ pipeline — filter-chains for output sinks
        let eq_pipeline = EqPipeline::start(headset_sink.to_string(), &saved_eq)?;

        // Mic uses pw-link to connect the headset mic source to the virtual
        // Audio/Source/Virtual node. This makes it appear as a proper input
        // device in apps like Discord (unlike sink monitors which are hidden).
        let mic_linked = match find_headset_source() {
            Ok(source) => match link_mic_source(&source) {
                Ok(()) => {
                    log::info!("Linked headset mic ({source}) → SteelSeries_Mic");
                    true
                }
                Err(e) => {
                    log::warn!("Failed to link mic source: {e}");
                    false
                }
            },
            Err(e) => {
                log::warn!("Could not find headset mic source: {e}");
                false
            }
        };

        Ok(AudioRouter {
            eq_pipeline,
            mic_linked,
        })
    }

    /// Update EQ for a specific sink. Called from the debounced UI callback.
    pub fn update_eq(&self, sink_name: &str, bands: &[Band; NUM_BANDS]) {
        log::info!("EQ update requested for {sink_name}");
        self.eq_pipeline.apply(sink_name, *bands);
    }

    pub fn destroy(&mut self) {
        if self.mic_linked {
            unlink_mic_source();
            self.mic_linked = false;
        }
        log::info!("Audio routing destroyed");
    }
}

impl Drop for AudioRouter {
    fn drop(&mut self) {
        self.destroy();
    }
}

// ---------------------------------------------------------------------------
// EqPipeline — persistent pw-cli session on a dedicated thread
// ---------------------------------------------------------------------------

enum EqCommand {
    Apply {
        sink_name: String,
        bands: [Band; NUM_BANDS],
    },
    Shutdown,
}

pub struct EqPipeline {
    tx: mpsc::Sender<EqCommand>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl EqPipeline {
    fn start(
        headset_sink: String,
        saved_eq: &HashMap<EqTarget, SinkEq>,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();

        let initial: Vec<(String, [Band; NUM_BANDS])> = ALL_SINKS
            .iter()
            .filter(|(name, _, _)| *name != super::sinks::MIC_SOURCE_NAME)
            .map(|(name, _, _)| {
                let target = EqTarget::ALL
                    .iter()
                    .find(|t| t.sink_name() == *name)
                    .copied();
                let bands = target
                    .and_then(|t| saved_eq.get(&t))
                    .map(|eq| eq.bands)
                    .unwrap_or_else(crate::eq::model::default_bands);
                (name.to_string(), bands)
            })
            .collect();

        let headset = headset_sink.clone();
        let handle = std::thread::Builder::new()
            .name("eq-pipeline".into())
            .spawn(move || eq_thread(rx, headset, initial))
            .map_err(|e| format!("Failed to spawn EQ thread: {e}"))?;

        Ok(EqPipeline {
            tx,
            handle: Some(handle),
        })
    }

    fn apply(&self, sink_name: &str, bands: [Band; NUM_BANDS]) {
        let _ = self.tx.send(EqCommand::Apply {
            sink_name: sink_name.to_string(),
            bands,
        });
    }
}

impl Drop for EqPipeline {
    fn drop(&mut self) {
        let _ = self.tx.send(EqCommand::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// EQ thread — owns the pw-cli child process
// ---------------------------------------------------------------------------

/// The "effective label" used in the filter-chain for a band.
/// Disabled bands become pass-through peaking at 0 dB.
fn effective_label(band: &Band) -> &'static str {
    if !band.enabled {
        "bq_peaking"
    } else {
        match band.filter_type {
            FilterType::Peaking => "bq_peaking",
            FilterType::LowShelf => "bq_lowshelf",
            FilterType::HighShelf => "bq_highshelf",
            FilterType::LowPass => "bq_lowpass",
            FilterType::HighPass => "bq_highpass",
        }
    }
}

/// Whether only Freq/Q/Gain changed (labels are the same), meaning we can
/// update filter controls in-place without tearing down the filter-chain.
fn needs_rebuild(old: &[Band; NUM_BANDS], new: &[Band; NUM_BANDS]) -> bool {
    for (o, n) in old.iter().zip(new.iter()) {
        if effective_label(o) != effective_label(n) {
            return true;
        }
    }
    false
}

struct FilterChainInfo {
    module_proxy_id: u32,
    /// Global ID of the capture node — used for set-param (controls live here)
    capture_node_id: Option<u32>,
}

fn eq_thread(
    rx: mpsc::Receiver<EqCommand>,
    headset_sink: String,
    initial: Vec<(String, [Band; NUM_BANDS])>,
) {
    let Ok(mut session) = PwCliSession::start() else {
        log::error!("Failed to start pw-cli session — EQ disabled");
        while rx.recv().is_ok() {}
        return;
    };

    let mut chains: HashMap<String, FilterChainInfo> = HashMap::new();
    let mut current_bands: HashMap<String, [Band; NUM_BANDS]> = HashMap::new();

    // Load initial filter-chains for each output sink
    for (sink_name, bands) in &initial {
        match session.load_filter_chain(sink_name, &headset_sink, bands) {
            Ok(info) => {
                log::info!(
                    "EQ filter-chain for {sink_name} loaded (module {}, capture node {:?})",
                    info.module_proxy_id, info.capture_node_id
                );
                chains.insert(sink_name.clone(), info);
                current_bands.insert(sink_name.clone(), *bands);
            }
            Err(e) => {
                log::error!("Failed to load initial EQ for {sink_name}: {e}");
            }
        }
    }

    // Process commands from the GTK main thread
    while let Ok(cmd) = rx.recv() {
        match cmd {
            EqCommand::Apply { sink_name, bands } => {
                // Skip if bands are identical to what's loaded
                if current_bands.get(&sink_name).is_some_and(|cur| {
                    cur.iter().zip(bands.iter()).all(|(a, b)| {
                        a.frequency == b.frequency
                            && a.gain_db == b.gain_db
                            && a.q == b.q
                            && a.filter_type == b.filter_type
                            && a.enabled == b.enabled
                    })
                }) {
                    continue; // nothing changed (e.g. tab switch)
                }

                let has_chain = chains.contains_key(&sink_name);
                let rebuild = !has_chain
                    || current_bands
                        .get(&sink_name)
                        .is_some_and(|cur| needs_rebuild(cur, &bands));

                if rebuild {
                    // Full rebuild: filter types changed, need new graph
                    log::info!("EQ rebuild: {sink_name}");
                    if let Some(info) = chains.remove(&sink_name) {
                        session.destroy_module(info.module_proxy_id);
                    }
                    session.destroy_by_name(&format!("eq_cap_{sink_name}"));
                    session.destroy_by_name(&format!("eq_out_{sink_name}"));

                    match session.load_filter_chain(&sink_name, &headset_sink, &bands) {
                        Ok(info) => {
                            log::info!(
                                "EQ rebuild: {sink_name} loaded (module {}, node {:?})",
                                info.module_proxy_id, info.capture_node_id
                            );
                            chains.insert(sink_name.clone(), info);
                            current_bands.insert(sink_name, bands);
                        }
                        Err(e) => {
                            log::error!("EQ rebuild FAILED for {sink_name}: {e}");
                            current_bands.remove(&sink_name);
                        }
                    }
                } else if let Some(node_id) = chains[&sink_name].capture_node_id {
                    // In-place update: only Freq/Q/Gain changed — no audio gap
                    log::debug!("EQ set-param: {sink_name} (node {node_id})");
                    if let Err(e) = session.update_filter_params(node_id, &bands) {
                        log::error!("EQ set-param failed for {sink_name}: {e}");
                    }
                    current_bands.insert(sink_name, bands);
                } else {
                    // No capture node ID — fall back to rebuild
                    log::warn!("EQ: no capture node ID for {sink_name}, rebuilding");
                    if let Some(info) = chains.remove(&sink_name) {
                        session.destroy_module(info.module_proxy_id);
                    }
                    match session.load_filter_chain(&sink_name, &headset_sink, &bands) {
                        Ok(info) => {
                            chains.insert(sink_name.clone(), info);
                            current_bands.insert(sink_name, bands);
                        }
                        Err(e) => {
                            log::error!("EQ fallback rebuild FAILED for {sink_name}: {e}");
                        }
                    }
                }
            }
            EqCommand::Shutdown => {
                log::info!("EQ pipeline shutting down");
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PwCliSession — persistent pw-cli with continuous stdout reader thread
// ---------------------------------------------------------------------------

struct PwCliSession {
    child: Child,
    stdin: ChildStdin,
    /// Lines from pw-cli stdout, continuously read by a background thread.
    /// This prevents the pipe buffer from filling up and deadlocking pw-cli.
    line_rx: mpsc::Receiver<String>,
    _reader: std::thread::JoinHandle<()>,
}

impl PwCliSession {
    fn start() -> Result<Self, String> {
        let mut child = Command::new("pw-cli")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start pw-cli: {e}"))?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        // Spawn a reader thread that continuously drains stdout line-by-line.
        // This prevents the pipe buffer from filling up — if pw-cli blocks on
        // a full pipe, it can't process our commands and we deadlock.
        let (line_tx, line_rx) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("pw-cli-reader".into())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(l) => {
                            if line_tx.send(l).is_err() {
                                break; // main thread dropped the receiver
                            }
                        }
                        Err(_) => break, // pipe closed (child killed)
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn pw-cli reader: {e}"))?;

        let session = PwCliSession {
            child,
            stdin,
            line_rx,
            _reader: reader,
        };

        // Drain the welcome banner
        session.drain_lines(Duration::from_millis(500));

        Ok(session)
    }

    /// Load a 10-band filter-chain for a sink.
    /// Returns both the module proxy ID (for destroy) and the capture node's
    /// global ID (for set-param — EQ controls live on the capture node).
    fn load_filter_chain(
        &mut self,
        sink_name: &str,
        headset_sink: &str,
        bands: &[Band; NUM_BANDS],
    ) -> Result<FilterChainInfo, String> {
        let cmd = build_filter_chain_cmd(sink_name, headset_sink, bands);
        self.send_command(&cmd)?;

        let cap_name = format!("eq_cap_{sink_name}");
        self.read_filter_chain_ids(&cap_name, Duration::from_secs(5))
    }

    /// Update biquad controls in-place via set-param. No audio gap.
    /// `node_id` must be the capture node's global ID (not the module proxy ID).
    fn update_filter_params(
        &mut self,
        node_id: u32,
        bands: &[Band; NUM_BANDS],
    ) -> Result<(), String> {
        // Build the flat params array: "key" value "key" value ...
        let mut entries = String::new();
        for (i, band) in bands.iter().enumerate() {
            let (freq, q, gain) = if band.enabled {
                (band.frequency, band.q, band.gain_db)
            } else {
                (band.frequency, band.q, 0.0)
            };
            if !entries.is_empty() {
                entries.push(' ');
            }
            let label = effective_label(band);
            if label == "bq_lowpass" || label == "bq_highpass" {
                entries.push_str(&format!(
                    "\"eq{i}:Freq\" {freq:.1} \"eq{i}:Q\" {q:.2}"
                ));
            } else {
                entries.push_str(&format!(
                    "\"eq{i}:Freq\" {freq:.1} \"eq{i}:Q\" {q:.2} \"eq{i}:Gain\" {gain:.1}"
                ));
            }
        }

        self.send_command(&format!(
            "set-param {node_id} Props {{ params = [ {entries} ] }}"
        ))?;
        self.drain_lines(Duration::from_millis(50));
        Ok(())
    }

    fn destroy_module(&mut self, id: u32) {
        if let Err(e) = self.send_command(&format!("destroy {id}")) {
            log::warn!("Failed to send destroy for module {id}: {e}");
        }
        // Give pw-cli time to process and emit events
        self.drain_lines(Duration::from_millis(300));
    }

    /// Safety net: try to destroy a PipeWire object by its node.name.
    /// pw-cli looks up the name in the global registry if it's not a number.
    /// This catches orphaned nodes whose module ID was lost.
    fn destroy_by_name(&mut self, name: &str) {
        if let Err(e) = self.send_command(&format!("destroy {name}")) {
            log::warn!("Failed to send destroy-by-name for {name}: {e}");
        }
        self.drain_lines(Duration::from_millis(100));
    }

    fn send_command(&mut self, cmd: &str) -> Result<(), String> {
        writeln!(self.stdin, "{cmd}")
            .map_err(|e| format!("pw-cli stdin write failed: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("pw-cli stdin flush failed: {e}"))
    }

    /// Read pw-cli output after load-module, extracting:
    /// - Module proxy ID from `N = @module:M`
    /// - Capture node global ID from `added global: id N, type ...Node`
    ///   followed by `node.name = "<cap_name>"`
    fn read_filter_chain_ids(
        &self,
        cap_name: &str,
        timeout: Duration,
    ) -> Result<FilterChainInfo, String> {
        let deadline = Instant::now() + timeout;
        let mut module_id: Option<u32> = None;
        let mut last_node_global_id: Option<u32> = None;
        let mut capture_node_id: Option<u32> = None;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.line_rx.recv_timeout(remaining) {
                Ok(line) => {
                    // Check for module proxy ID
                    if module_id.is_none() {
                        if let Some(id) = parse_module_id_from_line(&line) {
                            module_id = Some(id);
                        }
                    }
                    // Track last seen node global ID from "added global" events
                    if let Some(id) = parse_added_node_global_id(&line) {
                        last_node_global_id = Some(id);
                    }
                    // When we see our capture node name, associate it with
                    // the last seen global node ID
                    if capture_node_id.is_none()
                        && line.contains(&format!("node.name = \"{cap_name}\""))
                    {
                        capture_node_id = last_node_global_id;
                    }
                    // Stop once we have both
                    if module_id.is_some() && capture_node_id.is_some() {
                        // Drain a bit more to clear remaining events
                        self.drain_lines(Duration::from_millis(200));
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        let module_proxy_id =
            module_id.ok_or("No module ID found in pw-cli output")?;
        if capture_node_id.is_none() {
            log::warn!("Could not find capture node ID for {cap_name} — set-param will fall back to rebuild");
        }
        Ok(FilterChainInfo {
            module_proxy_id,
            capture_node_id,
        })
    }

    /// Drain and discard lines for the given duration.
    fn drain_lines(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.line_rx.recv_timeout(remaining) {
                Ok(_) => {} // discard
                Err(_) => break,
            }
        }
    }
}

impl Drop for PwCliSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Filter-chain command builder
// ---------------------------------------------------------------------------

fn build_filter_chain_cmd(
    sink_name: &str,
    headset_sink: &str,
    bands: &[Band; NUM_BANDS],
) -> String {
    let mut nodes = String::new();
    for (i, band) in bands.iter().enumerate() {
        let (label, control) = if !band.enabled {
            (
                "bq_peaking",
                format!(
                    "\"Freq\" = {:.1} \"Q\" = {:.2} \"Gain\" = 0.0",
                    band.frequency, band.q
                ),
            )
        } else {
            let label = match band.filter_type {
                FilterType::Peaking => "bq_peaking",
                FilterType::LowShelf => "bq_lowshelf",
                FilterType::HighShelf => "bq_highshelf",
                FilterType::LowPass => "bq_lowpass",
                FilterType::HighPass => "bq_highpass",
            };
            let control = if band.filter_type.uses_gain() {
                format!(
                    "\"Freq\" = {:.1} \"Q\" = {:.2} \"Gain\" = {:.1}",
                    band.frequency, band.q, band.gain_db
                )
            } else {
                format!(
                    "\"Freq\" = {:.1} \"Q\" = {:.2}",
                    band.frequency, band.q
                )
            };
            (label, control)
        };

        if !nodes.is_empty() {
            nodes.push(' ');
        }
        nodes.push_str(&format!(
            "{{ type = builtin name = eq{i} label = {label} control = {{ {control} }} }}"
        ));
    }

    let mut links = String::new();
    for i in 0..(NUM_BANDS - 1) {
        if !links.is_empty() {
            links.push(' ');
        }
        links.push_str(&format!(
            "{{ output = \"eq{i}:Out\" input = \"eq{}:In\" }}",
            i + 1
        ));
    }

    let is_mic = sink_name == super::sinks::MIC_SOURCE_NAME;
    let channels = if is_mic { 1 } else { 2 };
    let position = if is_mic { "MONO" } else { "FL FR" };

    let (capture_target, capture_flags, playback_target) = if is_mic {
        (sink_name.to_string(), "", sink_name.to_string())
    } else {
        (
            sink_name.to_string(),
            " stream.capture.sink = true",
            headset_sink.to_string(),
        )
    };

    let desc = sink_name.replace('_', " ");

    // node.passive: don't influence default sink selection
    // node.dont-reconnect: CRITICAL — prevents WirePlumber from re-routing
    //   the playback stream to a virtual sink on headset disconnect, which
    //   would create a feedback loop (sink → filter → sink → filter → ...)
    // stream.dont-remix: don't let PipeWire remix channels
    format!(
        "load-module libpipewire-module-filter-chain {{ \
         node.description = \"{desc} EQ\" \
         filter.graph = {{ \
         nodes = [ {nodes} ] \
         links = [ {links} ] \
         }} \
         audio.channels = {channels} \
         audio.position = [ {position} ] \
         capture.props = {{ \
         node.name = eq_cap_{sink_name} \
         target.object = {capture_target}{capture_flags} \
         node.passive = true \
         node.dont-reconnect = true \
         stream.dont-remix = true \
         }} \
         playback.props = {{ \
         node.name = eq_out_{sink_name} \
         target.object = {playback_target} \
         node.passive = true \
         node.dont-reconnect = true \
         stream.dont-remix = true \
         }} }}"
    )
}

/// Extract module proxy ID from a single pw-cli output line.
/// Matches the pattern `N = @module:M` and returns M.
fn parse_module_id_from_line(line: &str) -> Option<u32> {
    let idx = line.find("@module:")?;
    let rest = &line[idx + 8..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Extract a node global ID from pw-cli's "added global" event line.
/// Matches: `remote 0 added global: \tid <N>, type PipeWire:Interface:Node/3`
fn parse_added_node_global_id(line: &str) -> Option<u32> {
    if !line.contains("added global:") || !line.contains("Node") {
        return None;
    }
    // Find "id " followed by digits
    let idx = line.find("id ")?;
    let rest = &line[idx + 3..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

// ---------------------------------------------------------------------------
// Headset sink/source discovery
// ---------------------------------------------------------------------------

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

pub fn find_headset_source() -> Result<String, String> {
    let output = Command::new("pactl")
        .args(["list", "sources", "short"])
        .output()
        .map_err(|e| format!("Failed to run pactl: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("SteelSeries") && line.contains("Arctis_Nova_Elite") {
            if line.contains("SteelSeries_Mic") || line.contains(".monitor") {
                continue;
            }
            if let Some(name) = line.split_whitespace().nth(1) {
                return Ok(name.to_string());
            }
        }
    }

    Err("Arctis Nova Elite microphone source not found".to_string())
}

// ---------------------------------------------------------------------------
// Mic pw-link (direct port connection to Audio/Source/Virtual)
// ---------------------------------------------------------------------------

fn link_mic_source(headset_source: &str) -> Result<(), String> {
    let output = Command::new("pw-link")
        .args([headset_source, super::sinks::MIC_SOURCE_NAME])
        .output()
        .map_err(|e| format!("Failed to run pw-link: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pw-link failed: {stderr}"));
    }
    Ok(())
}

fn unlink_mic_source() {
    if let Ok(source) = find_headset_source() {
        let _ = Command::new("pw-link")
            .args(["-d", &source, super::sinks::MIC_SOURCE_NAME])
            .output();
    }
}

fn unload_module(module_id: u32) {
    if let Err(e) = Command::new("pactl")
        .args(["unload-module", &module_id.to_string()])
        .output()
    {
        log::warn!("Failed to unload module {module_id}: {e}");
    }
}

// ---------------------------------------------------------------------------
// Orphaned loopback cleanup
// ---------------------------------------------------------------------------

fn cleanup_orphaned_loopbacks() {
    use super::sinks::LEGACY_SINK_MIGRATIONS;

    let Ok(output) = Command::new("pactl")
        .args(["list", "modules", "short"])
        .output()
    else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut known: Vec<&str> = ALL_SINKS.iter().map(|(n, _, _)| *n).collect();
    known.extend(LEGACY_SINK_MIGRATIONS.iter().map(|(old, _)| *old));

    let mut cleaned = 0;
    for line in stdout.lines() {
        if !line.contains("module-loopback") {
            continue;
        }
        let referenced = known.iter().any(|name| {
            line.contains(&format!("source={name}.monitor"))
                || line.contains(&format!("source={name}"))
                || line.contains(&format!("sink={name}"))
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
