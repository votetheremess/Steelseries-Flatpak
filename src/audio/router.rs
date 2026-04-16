use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::eq::model::{Band, FilterType, EqTarget, SinkEq, SpatialState, NUM_BANDS};
use crate::eq::spatial;

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

        // Load saved mixer routing for per-sink output targets
        let saved_routing = super::persistence::load_mixer_routing();
        let mut initial_targets = HashMap::new();
        for (name, _, _) in ALL_SINKS {
            let target = saved_routing
                .get(*name)
                .cloned()
                .unwrap_or_else(|| headset_sink.to_string());
            initial_targets.insert(name.to_string(), target);
        }

        // Start the EQ pipeline — filter-chains for output sinks
        let eq_pipeline =
            EqPipeline::start(headset_sink.to_string(), &saved_eq, initial_targets)?;

        // Mic uses pw-link to connect the headset mic source to the virtual
        // Audio/Source/Virtual node. This makes it appear as a proper input
        // device in apps like Discord (unlike sink monitors which are hidden).
        let mic_source = saved_routing
            .get("mic")
            .cloned()
            .or_else(|| find_headset_source().ok());

        let mic_linked = match mic_source {
            Some(source) => match link_mic_source(&source) {
                Ok(()) => {
                    log::info!("Linked mic ({source}) → SteelSeries_Mic");
                    true
                }
                Err(e) => {
                    log::warn!("Failed to link mic source: {e}");
                    false
                }
            },
            None => {
                log::warn!("Could not find mic source");
                false
            }
        };

        // Set our virtual devices as system defaults so WirePlumber routes
        // new streams through our pipeline instead of directly to hardware.
        // Game is the default output — unassigned apps go here.
        // Mic is the default input — capture streams go through our virtual source.
        set_default_sink(super::sinks::GAME_SINK_NAME);
        set_default_source(super::sinks::MIC_SOURCE_NAME);

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

    /// Update spatial audio settings for a specific sink. May trigger a full
    /// filter-chain rebuild if `enabled` flips; otherwise pushes live Azimuth /
    /// Elevation / Radius updates to the 7 spatializer nodes via set-param.
    /// No-op for Chat and Mic (they don't support spatial).
    pub fn set_spatial(&self, sink_name: &str, state: SpatialState) {
        let target = EqTarget::ALL.iter().find(|t| t.sink_name() == sink_name);
        if let Some(t) = target {
            if !t.supports_spatial() {
                return;
            }
        } else {
            return;
        }
        self.eq_pipeline.set_spatial(sink_name, state);
    }

    /// Change which physical device an output sink routes to.
    /// Tears down and rebuilds the EQ filter-chain with the new target.
    pub fn reroute_sink(&self, sink_name: &str, new_target: &str) {
        log::info!("Rerouting {sink_name} → {new_target}");
        self.eq_pipeline.reroute(sink_name, new_target);
        super::persistence::save_mixer_routing_entry(sink_name, new_target);
    }

    /// Change which physical source the mic routes from.
    pub fn reroute_mic(&mut self, new_source: &str) {
        log::info!("Rerouting mic → {new_source}");
        // Persist the user's intent up-front. If the live link fails (missing
        // device, port mismatch, etc.), we still remember the selection so the
        // dropdown reflects it and the next launch tries again.
        super::persistence::save_mixer_routing_entry("mic", new_source);

        if self.mic_linked {
            unlink_mic_source();
        }
        match link_mic_source(new_source) {
            Ok(()) => {
                self.mic_linked = true;
                log::info!("Mic rerouted to {new_source}");
            }
            Err(e) => {
                log::error!("Failed to reroute mic to {new_source}: {e}");
            }
        }
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
    Reroute {
        sink_name: String,
        new_target: String,
    },
    SetSpatial {
        sink_name: String,
        state: SpatialState,
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
        initial_targets: HashMap<String, String>,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();

        let initial: Vec<(String, [Band; NUM_BANDS], SpatialState)> = ALL_SINKS
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
                let spatial_state = target
                    .and_then(|t| saved_eq.get(&t))
                    .map(|eq| eq.spatial)
                    .unwrap_or_default();
                (name.to_string(), bands, spatial_state)
            })
            .collect();

        let headset = headset_sink.clone();
        let handle = std::thread::Builder::new()
            .name("eq-pipeline".into())
            .spawn(move || eq_thread(rx, headset, initial, initial_targets))
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

    fn reroute(&self, sink_name: &str, new_target: &str) {
        let _ = self.tx.send(EqCommand::Reroute {
            sink_name: sink_name.to_string(),
            new_target: new_target.to_string(),
        });
    }

    fn set_spatial(&self, sink_name: &str, state: SpatialState) {
        let _ = self.tx.send(EqCommand::SetSpatial {
            sink_name: sink_name.to_string(),
            state,
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
            FilterType::Notch => "bq_notch",
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
    initial: Vec<(String, [Band; NUM_BANDS], SpatialState)>,
    initial_targets: HashMap<String, String>,
) {
    let Ok(mut session) = PwCliSession::start() else {
        log::error!("Failed to start pw-cli session — EQ disabled");
        while rx.recv().is_ok() {}
        return;
    };

    let mut chains: HashMap<String, FilterChainInfo> = HashMap::new();
    let mut current_bands: HashMap<String, [Band; NUM_BANDS]> = HashMap::new();
    let mut current_spatial: HashMap<String, SpatialState> = HashMap::new();
    // Per-sink output target — initialized from saved routing, defaults to headset
    let mut targets: HashMap<String, String> = initial_targets;

    // Load initial filter-chains for each output sink. Spatial is only engaged
    // if the sink supports it AND the HeSuVi IR for the current profile can
    // be extracted.
    for (sink_name, bands, spatial_state) in &initial {
        let target = targets
            .entry(sink_name.clone())
            .or_insert_with(|| headset_sink.clone());
        let ir_path = active_spatial_ir(sink_name, spatial_state);
        let spatial_ctx = ir_path.as_deref().map(|p| (spatial_state, p));
        match session.load_filter_chain(sink_name, target, bands, spatial_ctx) {
            Ok(info) => {
                log::info!(
                    "Filter-chain for {sink_name} loaded (module {}, capture node {:?}, spatial={})",
                    info.module_proxy_id,
                    info.capture_node_id,
                    spatial_ctx.is_some()
                );
                chains.insert(sink_name.clone(), info);
                current_bands.insert(sink_name.clone(), *bands);
                current_spatial.insert(sink_name.clone(), *spatial_state);
            }
            Err(e) => {
                log::error!("Failed to load initial filter-chain for {sink_name}: {e}");
            }
        }
    }

    // Helper: destroy + rebuild a filter-chain for a sink with current state.
    let rebuild_chain = |session: &mut PwCliSession,
                         chains: &mut HashMap<String, FilterChainInfo>,
                         sink_name: &str,
                         target: &str,
                         bands: &[Band; NUM_BANDS],
                         spatial_state: &SpatialState|
     -> bool {
        if let Some(info) = chains.remove(sink_name) {
            session.destroy_module(info.module_proxy_id);
        }
        session.destroy_by_name(&format!("eq_cap_{sink_name}"));
        session.destroy_by_name(&format!("eq_out_{sink_name}"));

        let ir_path = active_spatial_ir(sink_name, spatial_state);
        let spatial_ctx = ir_path.as_deref().map(|p| (spatial_state, p));
        match session.load_filter_chain(sink_name, target, bands, spatial_ctx) {
            Ok(info) => {
                chains.insert(sink_name.to_string(), info);
                true
            }
            Err(e) => {
                log::error!("Filter-chain rebuild FAILED for {sink_name}: {e}");
                false
            }
        }
    };

    // Process commands from the GTK main thread
    while let Ok(cmd) = rx.recv() {
        match cmd {
            EqCommand::Apply { sink_name, bands } => {
                // Mic has no EQ filter-chain — it uses pw-link, not a filter-chain.
                // Creating one would route mic audio to the headset speaker and
                // cause WirePlumber to rewire the mic source connections.
                if sink_name == super::sinks::MIC_SOURCE_NAME {
                    continue;
                }

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

                let target = targets
                    .get(&sink_name)
                    .cloned()
                    .unwrap_or_else(|| headset_sink.clone());
                let spatial_state = current_spatial
                    .get(&sink_name)
                    .copied()
                    .unwrap_or_default();

                let has_chain = chains.contains_key(&sink_name);
                let rebuild = !has_chain
                    || current_bands
                        .get(&sink_name)
                        .is_some_and(|cur| needs_rebuild(cur, &bands));

                if rebuild {
                    log::info!("EQ rebuild: {sink_name}");
                    if rebuild_chain(
                        &mut session,
                        &mut chains,
                        &sink_name,
                        &target,
                        &bands,
                        &spatial_state,
                    ) {
                        current_bands.insert(sink_name, bands);
                    } else {
                        current_bands.remove(&sink_name);
                    }
                } else if let Some(node_id) = chains[&sink_name].capture_node_id {
                    // In-place update: only Freq/Q/Gain changed — no audio gap
                    log::debug!("EQ set-param: {sink_name} (node {node_id})");
                    let spatial_active =
                        active_spatial_ir(&sink_name, &spatial_state).is_some();
                    let result = if spatial_active {
                        session.update_filter_params_spatial(node_id, &bands)
                    } else {
                        session.update_filter_params(node_id, &bands)
                    };
                    if let Err(e) = result {
                        log::error!("EQ set-param failed for {sink_name}: {e}");
                    }
                    current_bands.insert(sink_name, bands);
                } else {
                    // No capture node ID — fall back to rebuild
                    log::warn!("EQ: no capture node ID for {sink_name}, rebuilding");
                    if rebuild_chain(
                        &mut session,
                        &mut chains,
                        &sink_name,
                        &target,
                        &bands,
                        &spatial_state,
                    ) {
                        current_bands.insert(sink_name, bands);
                    }
                }
            }
            EqCommand::Reroute { sink_name, new_target } => {
                log::info!("Rerouting {sink_name} → {new_target}");
                targets.insert(sink_name.clone(), new_target.clone());
                let bands = current_bands
                    .get(&sink_name)
                    .copied()
                    .unwrap_or_else(crate::eq::model::default_bands);
                let spatial_state = current_spatial
                    .get(&sink_name)
                    .copied()
                    .unwrap_or_default();
                if rebuild_chain(
                    &mut session,
                    &mut chains,
                    &sink_name,
                    &new_target,
                    &bands,
                    &spatial_state,
                ) {
                    current_bands.insert(sink_name, bands);
                }
            }
            EqCommand::SetSpatial { sink_name, state } => {
                if sink_name == super::sinks::MIC_SOURCE_NAME {
                    continue;
                }
                let prev = current_spatial
                    .get(&sink_name)
                    .copied()
                    .unwrap_or_default();

                // Decide what kind of update this is. The chain's *topology*
                // only depends on enable + profile; wet_mix is a runtime
                // gain blend that we can push via set-param without the
                // ~300 ms rebuild gap.
                let topology_unchanged = match (prev.enabled, state.enabled) {
                    (false, false) => true,
                    (true, true) => prev.profile == state.profile,
                    _ => false,
                };
                let wet_changed = (prev.wet_mix - state.wet_mix).abs() > 1e-3;

                // Always update tracked state so the next toggle / rebuild
                // uses the freshly-picked values.
                current_spatial.insert(sink_name.clone(), state);

                if topology_unchanged {
                    if !state.enabled {
                        // Both disabled — the simple chain ignores wet_mix.
                        continue;
                    }
                    if wet_changed {
                        if let Some(node_id) =
                            chains.get(&sink_name).and_then(|c| c.capture_node_id)
                        {
                            log::debug!(
                                "Spatial wet_mix set-param for {sink_name}: {:.3} → {:.3}",
                                prev.wet_mix,
                                state.wet_mix,
                            );
                            if let Err(e) = session.update_wet_mix(node_id, state.wet_mix) {
                                log::error!("Wet mix set-param failed for {sink_name}: {e}");
                            }
                        }
                    }
                    // Else: nothing audible changed.
                    continue;
                }

                log::info!(
                    "Spatial update for {sink_name}: enabled {} → {}, profile {} → {}, wet {:.2} → {:.2} (rebuilding chain)",
                    prev.enabled,
                    state.enabled,
                    prev.profile.key(),
                    state.profile.key(),
                    prev.wet_mix,
                    state.wet_mix,
                );
                let target = targets
                    .get(&sink_name)
                    .cloned()
                    .unwrap_or_else(|| headset_sink.clone());
                let bands = current_bands
                    .get(&sink_name)
                    .copied()
                    .unwrap_or_else(crate::eq::model::default_bands);
                rebuild_chain(
                    &mut session,
                    &mut chains,
                    &sink_name,
                    &target,
                    &bands,
                    &state,
                );
            }
            EqCommand::Shutdown => {
                log::info!("EQ pipeline shutting down");
                break;
            }
        }
    }
}

/// Returns the IR path to use for this sink if spatial should be active:
/// the sink supports spatial, `enabled` is true, and the HeSuVi IR for the
/// chosen profile could be extracted. Returns `None` otherwise (the caller
/// then emits the simple-chain topology).
fn active_spatial_ir(sink_name: &str, state: &SpatialState) -> Option<PathBuf> {
    if !state.enabled {
        return None;
    }
    let target = EqTarget::ALL.iter().find(|t| t.sink_name() == sink_name)?;
    if !target.supports_spatial() {
        return None;
    }
    spatial::ensure_ir_file(state.profile)
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

    /// Load a filter-chain for a sink. When `spatial_ctx` is `Some`, emits the
    /// virtual-7.1 upmix + HRTF graph; otherwise emits the simple 10-biquad
    /// chain. Returns module proxy ID + capture node global ID.
    fn load_filter_chain(
        &mut self,
        sink_name: &str,
        target: &str,
        bands: &[Band; NUM_BANDS],
        spatial_ctx: Option<(&SpatialState, &std::path::Path)>,
    ) -> Result<FilterChainInfo, String> {
        let cmd = build_filter_chain_cmd(sink_name, target, bands, spatial_ctx);
        self.send_command(&cmd)?;

        let cap_name = format!("eq_cap_{sink_name}");
        self.read_filter_chain_ids(&cap_name, Duration::from_secs(5))
    }

    /// Update biquad controls in-place via set-param. No audio gap. Used for
    /// the simple (non-spatial) chain where EQ nodes are named `eq0`..`eq9`.
    fn update_filter_params(
        &mut self,
        node_id: u32,
        bands: &[Band; NUM_BANDS],
    ) -> Result<(), String> {
        let entries = build_band_params("eq", bands);
        self.send_command(&format!(
            "set-param {node_id} Props {{ params = [ {entries} ] }}"
        ))?;
        self.drain_lines(Duration::from_millis(50));
        Ok(())
    }

    /// Same as `update_filter_params` but for the spatial topology, where the
    /// EQ is duplicated as `eq_l_0`..`eq_l_9` + `eq_r_0`..`eq_r_9`. We push the
    /// same band values to both sides in a single set-param call.
    fn update_filter_params_spatial(
        &mut self,
        node_id: u32,
        bands: &[Band; NUM_BANDS],
    ) -> Result<(), String> {
        let left = build_band_params("eq_l", bands);
        let right = build_band_params("eq_r", bands);
        self.send_command(&format!(
            "set-param {node_id} Props {{ params = [ {left} {right} ] }}"
        ))?;
        self.drain_lines(Duration::from_millis(50));
        Ok(())
    }

    /// Live-update the dry/wet blend on the spatial output mixers via
    /// set-param. Wet inputs (1..7) get `0.5 * wet`, dry input (8) gets
    /// `1.0 - wet`. Zero audio gap.
    fn update_wet_mix(&mut self, node_id: u32, wet_mix: f64) -> Result<(), String> {
        let wet = wet_mix.clamp(0.0, 1.0);
        let wet_g = 0.5 * wet;
        let dry_g = 1.0 - wet;
        let mut entries = String::new();
        for side in ["mix_out_l", "mix_out_r"] {
            for ch in 1..=7 {
                if !entries.is_empty() {
                    entries.push(' ');
                }
                entries.push_str(&format!("\"{side}:Gain {ch}\" {wet_g:.3}"));
            }
            entries.push_str(&format!(" \"{side}:Gain 8\" {dry_g:.3}"));
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
    target: &str,
    bands: &[Band; NUM_BANDS],
    spatial_ctx: Option<(&SpatialState, &std::path::Path)>,
) -> String {
    let is_mic = sink_name == super::sinks::MIC_SOURCE_NAME;
    let (channels, position) = if is_mic { (1, "MONO") } else { (2, "FL FR") };

    let (capture_target, capture_flags, playback_target) = if is_mic {
        (sink_name.to_string(), "", sink_name.to_string())
    } else {
        (
            sink_name.to_string(),
            " stream.capture.sink = true",
            target.to_string(),
        )
    };

    let desc = sink_name.replace('_', " ");
    let (graph_body, desc_suffix) = if let Some((state, hrtf)) = spatial_ctx {
        (build_spatial_graph(bands, state, hrtf), " Spatial")
    } else {
        (build_simple_graph(bands, is_mic), " EQ")
    };
    let desc = format!("{desc}{desc_suffix}");

    // node.passive: don't influence default sink selection
    // node.dont-reconnect: CRITICAL — prevents WirePlumber from re-routing
    //   the playback stream to a virtual sink on headset disconnect, which
    //   would create a feedback loop (sink → filter → sink → filter → ...)
    // stream.dont-remix: don't let PipeWire remix channels
    format!(
        "load-module libpipewire-module-filter-chain {{ \
         node.description = \"{desc}\" \
         filter.graph = {{ {graph_body} }} \
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

/// Build the inside of `filter.graph = { ... }` for the simple 10-biquad chain.
/// PipeWire auto-replicates this per channel (mono for mic, stereo for sinks).
fn build_simple_graph(bands: &[Band; NUM_BANDS], _is_mic: bool) -> String {
    let mut nodes = String::new();
    for (i, band) in bands.iter().enumerate() {
        if !nodes.is_empty() {
            nodes.push(' ');
        }
        nodes.push_str(&build_biquad_node(&format!("eq{i}"), band));
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

    format!("nodes = [ {nodes} ] links = [ {links} ]")
}

/// Build a single biquad filter-chain node declaration.
fn build_biquad_node(name: &str, band: &Band) -> String {
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
            FilterType::Notch => "bq_notch",
        };
        let control = if band.filter_type.uses_gain() {
            format!(
                "\"Freq\" = {:.1} \"Q\" = {:.2} \"Gain\" = {:.1}",
                band.frequency, band.q, band.gain_db
            )
        } else {
            format!("\"Freq\" = {:.1} \"Q\" = {:.2}", band.frequency, band.q)
        };
        (label, control)
    };
    format!("{{ type = builtin name = {name} label = {label} control = {{ {control} }} }}")
}

/// Build a set-param band params blob using a per-side prefix (e.g. `"eq"`,
/// `"eq_l"`, `"eq_r"`). Skips the `Gain` entry for filter types that don't
/// use it.
fn build_band_params(prefix: &str, bands: &[Band; NUM_BANDS]) -> String {
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
        if label == "bq_lowpass" || label == "bq_highpass" || label == "bq_notch" {
            entries.push_str(&format!(
                "\"{prefix}{i}:Freq\" {freq:.1} \"{prefix}{i}:Q\" {q:.2}"
            ));
        } else {
            entries.push_str(&format!(
                "\"{prefix}{i}:Freq\" {freq:.1} \"{prefix}{i}:Q\" {q:.2} \"{prefix}{i}:Gain\" {gain:.1}"
            ));
        }
    }
    entries
}

/// Build the HeSuVi-IR convolution graph body (inside `filter.graph = {}`).
///
/// Topology:
///
///   capture:FL → eq_l_0 → … → eq_l_9 ──┐
///   capture:FR → eq_r_0 → … → eq_r_9 ──┤
///                                       │
///                            stereo → 7-channel upmix (7 mixer nodes):
///                              up_l  = 1·L + 0·R
///                              up_r  = 0·L + 1·R
///                              up_c  = 0.5·L + 0.5·R
///                              up_ls = 1·L + 0·R       (copy of L)
///                              up_lb = 1·L + 0·R       (copy of L)
///                              up_rs = 0·L + 1·R       (copy of R)
///                              up_rb = 0·L + 1·R       (copy of R)
///                                       │
///                            14 convolvers, each reading one channel of the
///                            selected HeSuVi WAV (channel N = speaker-to-ear IR):
///                              conv_l_lear, conv_l_rear   ← up_l  (ch 0,1)
///                              conv_ls_lear, conv_ls_rear ← up_ls (ch 2,3)
///                              conv_lb_lear, conv_lb_rear ← up_lb (ch 4,5)
///                              conv_c_lear, conv_c_rear   ← up_c  (ch 6,7)
///                              conv_r_lear, conv_r_rear   ← up_r  (ch 8,9)
///                              conv_rs_lear, conv_rs_rear ← up_rs (ch 10,11)
///                              conv_rb_lear, conv_rb_rear ← up_rb (ch 12,13)
///                                       │
///                              mix_out_l = sum(conv_*_lear) → playback:FL
///                              mix_out_r = sum(conv_*_rear) → playback:FR
///
/// HeSuVi channel order matches the EqualizerAPO convention: for each of the
/// 7 speakers, the left-ear IR is the even-indexed channel and the right-ear
/// IR is the next odd-indexed one.
fn build_spatial_graph(
    bands: &[Band; NUM_BANDS],
    state: &SpatialState,
    hrtf: &std::path::Path,
) -> String {
    let hrtf = hrtf.to_string_lossy();

    let mut nodes = String::new();
    let mut links = String::new();

    // Per-side EQ chains: eq_l_0..eq_l_9 and eq_r_0..eq_r_9
    for side in ["l", "r"] {
        for (i, band) in bands.iter().enumerate() {
            if !nodes.is_empty() {
                nodes.push(' ');
            }
            nodes.push_str(&build_biquad_node(&format!("eq_{side}_{i}"), band));
        }
        for i in 0..(NUM_BANDS - 1) {
            if !links.is_empty() {
                links.push(' ');
            }
            links.push_str(&format!(
                "{{ output = \"eq_{side}_{i}:Out\" input = \"eq_{side}_{}:In\" }}",
                i + 1
            ));
        }
    }

    // Stereo → 7-channel upmix (canonical HeSuVi/EqualizerAPO fanout). The
    // IR itself decorrelates and spatializes — we just duplicate stereo to
    // the expected 7 virtual-speaker inputs.
    //   (tag, gain_from_L, gain_from_R)
    let upmix: [(&str, f64, f64); 7] = [
        ("l", 1.0, 0.0),
        ("r", 0.0, 1.0),
        ("c", 0.5, 0.5),
        ("ls", 1.0, 0.0),
        ("lb", 1.0, 0.0),
        ("rs", 0.0, 1.0),
        ("rb", 0.0, 1.0),
    ];
    for (tag, gl, gr) in upmix {
        nodes.push_str(&format!(
            " {{ type = builtin name = up_{tag} label = mixer \
               control = {{ \"Gain 1\" = {gl:.3} \"Gain 2\" = {gr:.3} }} }}"
        ));
        links.push_str(&format!(
            " {{ output = \"eq_l_{last}:Out\" input = \"up_{tag}:In 1\" }} \
               {{ output = \"eq_r_{last}:Out\" input = \"up_{tag}:In 2\" }}",
            last = NUM_BANDS - 1
        ));
    }

    // 14 convolvers, one per channel of the HeSuVi IR. HeSuVi channel order:
    //   [L-left_ear, L-right_ear, LS-left_ear, LS-right_ear, LB-..., C-..., R-..., RS-..., RB-...]
    //   (tag, upmix_source, channel_idx, ear) — ear is "lear" or "rear"
    let convolvers: [(&str, &str, u32, &str); 14] = [
        ("l_lear",  "l",  0,  "lear"),
        ("l_rear",  "l",  1,  "rear"),
        ("ls_lear", "ls", 2,  "lear"),
        ("ls_rear", "ls", 3,  "rear"),
        ("lb_lear", "lb", 4,  "lear"),
        ("lb_rear", "lb", 5,  "rear"),
        ("c_lear",  "c",  6,  "lear"),
        ("c_rear",  "c",  7,  "rear"),
        ("r_lear",  "r",  8,  "lear"),
        ("r_rear",  "r",  9,  "rear"),
        ("rs_lear", "rs", 10, "lear"),
        ("rs_rear", "rs", 11, "rear"),
        ("rb_lear", "rb", 12, "lear"),
        ("rb_rear", "rb", 13, "rear"),
    ];
    for (tag, src, ch, _ear) in convolvers {
        nodes.push_str(&format!(
            " {{ type = builtin name = conv_{tag} label = convolver \
               config = {{ filename = \"{hrtf}\" channel = {ch} }} }}"
        ));
        links.push_str(&format!(
            " {{ output = \"up_{src}:Out\" input = \"conv_{tag}:In\" }}"
        ));
    }

    // Output mixers: sum the 7 wet (HRIR-convolved) left-ear / right-ear outs
    // PLUS one dry-bypass tap. The dry input lets a runtime "wet mix" knob
    // blend between fully-processed (wet=1) and untouched stereo (wet=0)
    // without rebuilding the chain — gains 1..7 = wet_per_input * wet_mix,
    // gain 8 = (1 - wet_mix) for the dry pass-through.
    //
    // wet_per_input = 0.5 keeps headroom safe when summing 7 correlated
    // HRIR outs (HeSuVi IRs are calibrated around this level).
    let wet = state.wet_mix.clamp(0.0, 1.0);
    let wet_g = 0.5 * wet;
    let dry_g = 1.0 - wet;
    nodes.push_str(&format!(
        " {{ type = builtin name = mix_out_l label = mixer \
             control = {{ \
               \"Gain 1\" = {w:.3} \"Gain 2\" = {w:.3} \"Gain 3\" = {w:.3} \
               \"Gain 4\" = {w:.3} \"Gain 5\" = {w:.3} \"Gain 6\" = {w:.3} \
               \"Gain 7\" = {w:.3} \"Gain 8\" = {d:.3} }} }} \
           {{ type = builtin name = mix_out_r label = mixer \
             control = {{ \
               \"Gain 1\" = {w:.3} \"Gain 2\" = {w:.3} \"Gain 3\" = {w:.3} \
               \"Gain 4\" = {w:.3} \"Gain 5\" = {w:.3} \"Gain 6\" = {w:.3} \
               \"Gain 7\" = {w:.3} \"Gain 8\" = {d:.3} }} }}",
        w = wet_g,
        d = dry_g,
    ));

    // Route each convolver to its output mixer. Same gain for all 7 wet
    // inputs so the mapping is just sequential.
    let left_ear_tags = ["l_lear", "ls_lear", "lb_lear", "c_lear", "r_lear", "rs_lear", "rb_lear"];
    let right_ear_tags = ["l_rear", "ls_rear", "lb_rear", "c_rear", "r_rear", "rs_rear", "rb_rear"];
    for (i, tag) in left_ear_tags.iter().enumerate() {
        links.push_str(&format!(
            " {{ output = \"conv_{tag}:Out\" input = \"mix_out_l:In {port}\" }}",
            port = i + 1
        ));
    }
    for (i, tag) in right_ear_tags.iter().enumerate() {
        links.push_str(&format!(
            " {{ output = \"conv_{tag}:Out\" input = \"mix_out_r:In {port}\" }}",
            port = i + 1
        ));
    }
    // Dry bypass — In 8 takes the untouched EQ tail per side.
    links.push_str(&format!(
        " {{ output = \"eq_l_{last}:Out\" input = \"mix_out_l:In 8\" }} \
           {{ output = \"eq_r_{last}:Out\" input = \"mix_out_r:In 8\" }}",
        last = NUM_BANDS - 1,
    ));

    // Explicit inputs/outputs bind capture:FL → eq_l_0:In, capture:FR → eq_r_0:In,
    // mix_out_l:Out → playback:FL, mix_out_r:Out → playback:FR.
    format!(
        "nodes = [ {nodes} ] \
         links = [ {links} ] \
         inputs = [ \"eq_l_0:In\" \"eq_r_0:In\" ] \
         outputs = [ \"mix_out_l:Out\" \"mix_out_r:Out\" ]"
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

fn link_mic_source(source: &str) -> Result<(), String> {
    let mic_input = format!("{}:input_MONO", super::sinks::MIC_SOURCE_NAME);
    log::info!("Linking mic source {source} → {mic_input}");

    // Bare-node linking (`pw-link <src> <sink>`) silently drops ports when the
    // channel counts don't match: a stereo source (`capture_FL` + `capture_FR`)
    // against our mono sink (`input_MONO`) matches only FL and leaves the link
    // incomplete or fails outright depending on pipewire version. Pick a
    // specific output port ourselves so the link is deterministic.
    let src_ports = list_source_output_ports(source);
    if src_ports.is_empty() {
        // Fall back to bare-node linking; pw-link may still find something.
        let output = Command::new("pw-link")
            .args([source, super::sinks::MIC_SOURCE_NAME])
            .output()
            .map_err(|e| format!("Failed to run pw-link: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "pw-link failed (no ports found for {source}): {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        return Ok(());
    }

    // Prefer a mono capture port; otherwise take the first port (usually FL).
    let chosen = src_ports
        .iter()
        .find(|p| p.ends_with(":capture_MONO"))
        .or_else(|| src_ports.iter().find(|p| p.ends_with(":capture_FL")))
        .or_else(|| src_ports.first())
        .unwrap()
        .clone();

    let output = Command::new("pw-link")
        .args([&chosen, &mic_input])
        .output()
        .map_err(|e| format!("Failed to run pw-link: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "pw-link {chosen} → {mic_input} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    log::info!("pw-link: connected {chosen} → {mic_input}");
    Ok(())
}

/// List PipeWire output ports for a given node name (e.g. an ALSA capture source).
/// Uses `pw-link -o`, which prints one port per line in the form `node:port`.
fn list_source_output_ports(source: &str) -> Vec<String> {
    let Ok(output) = Command::new("pw-link").args(["-o"]).output() else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let prefix = format!("{source}:");
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(&prefix))
        .map(String::from)
        .collect()
}

/// Disconnect ALL sources currently linked to SteelSeries_Mic's input port.
/// Uses `pw-link -l` to find current connections rather than assuming only the
/// headset is linked — the user may have rerouted to a different mic.
fn unlink_mic_source() {
    let mic_input = format!("{}:input_MONO", super::sinks::MIC_SOURCE_NAME);
    let Ok(output) = Command::new("pw-link").args(["-l"]).output() else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Look for lines showing connections TO our mic input port:
    //   SteelSeries_Mic:input_MONO
    //     |<- some_source:capture_MONO
    let mut in_mic_block = false;
    let mut unlinked = 0;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !line.starts_with(' ') && !line.starts_with('\t') {
            in_mic_block = trimmed == mic_input;
            continue;
        }
        if in_mic_block {
            if let Some(source_port) = trimmed.strip_prefix("|<- ") {
                log::info!("Unlinking {source_port} from {mic_input}");
                let status = Command::new("pw-link")
                    .args(["-d", source_port, &mic_input])
                    .output();
                match status {
                    Ok(out) if out.status.success() => unlinked += 1,
                    Ok(out) => log::warn!(
                        "pw-link -d {source_port} {mic_input} failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    ),
                    Err(e) => log::warn!("Failed to run pw-link -d: {e}"),
                }
            }
        }
    }
    log::info!("Unlinked {unlinked} mic source(s) from {mic_input}");
}

fn set_default_sink(sink_name: &str) {
    match Command::new("pactl")
        .args(["set-default-sink", sink_name])
        .output()
    {
        Ok(output) if output.status.success() => {
            log::info!("Set default sink to {sink_name}");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("Failed to set default sink: {stderr}");
        }
        Err(e) => log::warn!("Failed to run pactl set-default-sink: {e}"),
    }
}

fn set_default_source(source_name: &str) {
    match Command::new("pactl")
        .args(["set-default-source", source_name])
        .output()
    {
        Ok(output) if output.status.success() => {
            log::info!("Set default source to {source_name}");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("Failed to set default source: {stderr}");
        }
        Err(e) => log::warn!("Failed to run pactl set-default-source: {e}"),
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
