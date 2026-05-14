# Routing / Volume / Clips fixes — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` to dispatch this plan; tasks are tagged for parallel execution by **Implementer A** (Audio Bundle) and **Implementer B** (Clips UI Bundle) working in **separate worktrees** off `clipping-system`. Steps use checkbox (`- [ ]`) syntax for tracking. Authoritative design: `docs/superpowers/specs/2026-05-13-routing-volume-clips-fixes-design.md`.

**Goal:** Fix four issues on the `clipping-system` branch — app→sink routing persistence, mic hotplug auto-switch, virtual-source volume persistence, and Clips home-page section relocation + expansion with terminology pass — landing as a coordinated parallel-implementer change with a 90% QA + critic quality gate.

**Architecture:** A unified 2-second state-sync tick (`src/audio/state_sync.rs`) handles Issues 1, 2, 3 by orchestrating stream-reconciliation, mic-hotplug check, and virtual-volume capture in one closure on the GTK main thread. Issue 4 adds an independent UI section with new ClipCommand variants and a new persisted hotkey-label field. Two parallel implementers in separate worktrees with strictly disjoint file edits + four mutually-non-overlapping `app.rs` regions.

**Tech Stack:** Rust 2024, GTK4 / libadwaita, ashpd 0.11.1 GlobalShortcuts portal, `pactl` / `pw-cli` / `pw-link` shell-outs to PipeWire, distrobox `fedora-dev` for build deps.

---

## Parallel-execution map

| Phase | Agent | Worktree | Files |
|---|---|---|---|
| 0. Discriminator | `project-tester` | clipping-system worktree | Runs shell test + verifies GSR SIGINT semantics; reports H_A vs H_B |
| 1A. Audio Bundle | `plan-implementer` A | new worktree `SteelseriesFlatpak-routing-volume` | `src/audio/{persistence,router,sinks,state_sync}.rs`, `src/mixer.rs`, `src/app.rs` (3 disjoint regions) |
| 1B. Clips UI Bundle | `plan-implementer` B | new worktree `SteelseriesFlatpak-clips-ui` | `src/window.rs`, `src/clips/*.rs`, `src/app.rs` (2 disjoint regions: action cluster + `run_global_shortcuts` call site) |
| 2. QA synthesis | `qa-code-auditor` | reads both worktrees | Reports |
| 3. Adversarial review | `devils-advocate-critic` | reads QA report + both worktrees | Final report |
| 4. Verification | `project-tester` | merged worktree | Manual recipes |

Phases 1A and 1B run in **parallel**. Their work is mergeable because the five edit regions across `app.rs` (3 from A, 2 from B) are mutually disjoint — see "Coordination point: `src/app.rs`" at the bottom of this plan.

**Every commit MUST `cargo build` cleanly.** If a commit would leave the worktree non-compiling because of a downstream caller, either (a) merge that commit with its caller-fix commit, or (b) keep a temporary deprecated stub in the producer that's deleted in a subsequent commit. The implementer is responsible for choosing per-task; the plan defaults to (b) for clean bisects.

---

## Phase 0 — Pre-flight discriminator

This phase runs **before** Implementer A starts and determines whether Issue 3's fix path is H_A (the spec's primary design — port save/restore + periodic capture) or H_B (scope expansion — set initial volume via `pw-cli` Props at create-time).

### Task 0.1: Run the H_A vs H_B discriminator

**Owner:** `project-tester`
**Worktree:** `/var/home/admin/Documents/Code/SteelseriesFlatpak-clipping/`

- [ ] **Step 1: Build the current clipping-system app**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clipping && cargo build'
```

Expected: `Finished dev` line.

- [ ] **Step 2: Stop any running instance and launch the freshly-built binary**

```bash
pkill arctis-chatmix; sleep 1
/var/home/admin/Documents/Code/SteelseriesFlatpak-clipping/target/debug/arctis-chatmix &
sleep 3
```

Expected: app starts; sinks come up. `pactl list short sources | grep SteelSeries_Mic` should show the virtual source.

- [ ] **Step 3: Run the discriminator**

```bash
pactl set-source-volume SteelSeries_Mic 50%
sleep 0.5
pactl get-source-volume SteelSeries_Mic
```

- [ ] **Step 4: Interpret the result and report**

If the percentage on the second line of `get-source-volume` output is **50%** (or very close, like `front-left: 32768 / 50% / -6.02 dB`): **H_A is the candidate.** Bundle A proceeds as designed (port save/restore + periodic capture). Report this in the test summary.

If the percentage is anything else (default ~100%, or stuck on a previous value): **H_B is true.** Bundle A's scope expands to also include Task A11 (the `pw-cli` Props at create-time path). Flag this back to the team lead **before** Implementer A starts.

- [ ] **Step 4b: Verify GSR SIGINT semantics (separate, parallel check)**

This is a separate verification that informs the Pause-tooltip copy. With GSR in replay mode, send SIGINT and see whether a clip file is produced. Run:

```bash
mkdir -p /tmp/gsr-test && cd /tmp/gsr-test
flatpak run --command=gpu-screen-recorder com.dec05eba.gpu_screen_recorder \
  -w portal -r 20 -c mp4 -o /tmp/gsr-test/replay &
GSR_PID=$!
sleep 10
kill -INT "$GSR_PID"; wait "$GSR_PID" 2>/dev/null
ls -la /tmp/gsr-test/
```

- If `/tmp/gsr-test/` contains a `.mp4` file (or a `replay_*` file): SIGINT does flush the buffer to disk. The spec's "buffer is lost on pause" claim is wrong on this user's hardware. Flag back to the team lead — the Pause-button tooltip needs updating to "Pause recording. The current N-second clip will be saved on pause."
- If `/tmp/gsr-test/` is empty: SIGINT discards the buffer cleanly, matching the spec. Pause copy stays as designed.

- [ ] **Step 5: Stop the app (do not leave it running)**

```bash
pkill arctis-chatmix
```

---

## Phase 1A — Audio Bundle (Implementer A)

> **Implementer A**: All your work lives in worktree `/var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume/` branched off `clipping-system`. Build via `distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo build'`. Do not edit any files in `src/clips/` or `src/window.rs` — those belong to Implementer B.

### Task A1: Port volume helpers from main

**Files:**
- Modify: `src/audio/persistence.rs` (append helpers)

- [ ] **Step 1: Read the main-branch reference**

Read `/var/home/admin/Documents/Code/SteelseriesFlatpak/src/audio/persistence.rs` lines 282-402 (the `// Volume persistence` block). The helpers to port are `VOLUMES_FILE` constant, `volumes_path()`, `load_volumes()`, and `save_volume_entry()`.

- [ ] **Step 2: Append the ported block to `src/audio/persistence.rs`**

Append this block at the end of the file:

```rust
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
```

- [ ] **Step 3: Build to verify the port compiles**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo build 2>&1 | tail -10'
```

Expected: `Finished dev` line, no errors.

- [ ] **Step 4: Commit**

```bash
git add src/audio/persistence.rs
git commit -m "audio: port volume persistence helpers from main"
```

### Task A2: Add the `MicPreference` struct + `mixer_routing.txt` 5-field reader/writer

**Files:**
- Modify: `src/audio/persistence.rs`

- [ ] **Step 1: Write failing tests for the new helpers**

Add to `src/audio/persistence.rs` (place near the file's existing tests block; if none, add a `#[cfg(test)]` module at the file's end):

```rust
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
```

Note: this uses internal helpers `parse_mic_preference_from(&Path)` and `parse_mixer_routing_from(&Path)` which the implementation must expose for testing (or use `pub(crate)` helpers that take a path).

- [ ] **Step 2: Run tests to verify they fail**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test mic_preference 2>&1 | tail -20'
```

Expected: Tests fail to compile (`parse_mic_preference_from` not found, `MicPreference` not defined).

- [ ] **Step 3: Implement the struct + helpers + update `load_mixer_routing` to skip mic**

Add to `src/audio/persistence.rs` (just before the `Volume persistence` block from Task A1):

```rust
// ---------------------------------------------------------------------------
// Mic preference (5-field mic line in mixer_routing.txt)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct MicPreference {
    pub node_name: String,
    pub product_name: String,
    pub bus_path: String,
    pub bluez_address: String,
}

impl MicPreference {
    pub fn from_fields(node: String, product: String, bus_path: String, bluez: String) -> Self {
        Self { node_name: node, product_name: product, bus_path, bluez_address: bluez }
    }
}

pub(crate) fn parse_mic_preference_from(path: &std::path::Path) -> Option<MicPreference> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let mut parts = line.split('\t');
        if parts.next()? != "mic" { continue; }
        let node_name = parts.next()?.to_string();
        let product_name = parts.next().unwrap_or("").to_string();
        let bus_path = parts.next().unwrap_or("").to_string();
        let bluez_address = parts.next().unwrap_or("").to_string();
        return Some(MicPreference { node_name, product_name, bus_path, bluez_address });
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
    let routing = load_mixer_routing();  // non-mic channels only (skip-mic semantics)
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

// REPLACE the existing `save_mixer_routing_entry` (currently at
// `src/audio/persistence.rs:322-344`) with this updated version that
// preserves the 5-field mic line.
//
// The old version load_mixer_routing()'d the file, inserted the new entry,
// and wrote the whole map back. Once load_mixer_routing skips "mic" lines
// (per the new behavior), the old write would erase the 5-field mic record
// on every sink reroute. The replacement reads the mic preference separately
// and re-emits the 5-field line at the end.
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

pub(crate) fn parse_mixer_routing_from(path: &std::path::Path) -> HashMap<String, String> {
    let Ok(content) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let channel = parts.next()?.to_string();
            if channel == "mic" { return None; }  // 5-field record, read separately
            let device = parts.next()?.to_string();
            Some((channel, device))
        })
        .collect()
}
```

Then **rewrite** the existing `load_mixer_routing()` to use the new helper:

```rust
pub fn load_mixer_routing() -> HashMap<String, String> {
    let Some(path) = mixer_routing_path() else {
        return HashMap::new();
    };
    parse_mixer_routing_from(&path)
}
```

Also add `tempfile` as a dev-dependency in `Cargo.toml` if not present:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test mic_preference 2>&1 | tail -20'
```

Expected: 4 tests pass.

- [ ] **Step 5: Run the full test suite to confirm no regressions**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test 2>&1 | tail -15'
```

Expected: all existing tests still pass; 4 new tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/audio/persistence.rs Cargo.toml
git commit -m "audio: add MicPreference + 5-field mixer_routing reader/writer"
```

### Task A3: Extend `list_physical_sources` to return stable-id chain

**Files:**
- Modify: `src/audio/sinks.rs` (`parse_device_list` + `list_physical_sources` signatures)
- Modify: `src/mixer.rs` (2 destructuring sites — pass-through)

- [ ] **Step 1: Write failing test for the new parser**

Add to `src/audio/sinks.rs` in a `#[cfg(test)]` module:

```rust
#[cfg(test)]
mod parse_device_list_tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = "\
Source #45
\tState: SUSPENDED
\tName: alsa_input.usb-FOO-00
\tDescription: Foo USB Mic
\tProperties:
\t\tdevice.product.name = \"Foo Mic Pro\"
\t\tdevice.bus_path = \"usb-0000:00:14.0-3\"
Source #46
\tState: SUSPENDED
\tName: bluez_input.AA:BB:CC:DD:EE:FF.headset-head-unit
\tDescription: Sony WH-1000XM4
\tProperties:
\t\tapi.bluez5.address = \"AA:BB:CC:DD:EE:FF\"
\t\tdevice.product.name = \"WH-1000XM4\"
";

    #[test]
    fn parses_properties_block_for_two_records() {
        let out = parse_device_list_for_test(SAMPLE_OUTPUT);
        assert_eq!(out.len(), 2);
        let (name, _desc, prod, bus, bt) = &out[0];
        assert_eq!(name, "alsa_input.usb-FOO-00");
        assert_eq!(prod.as_deref(), Some("Foo Mic Pro"));
        assert_eq!(bus.as_deref(), Some("usb-0000:00:14.0-3"));
        assert!(bt.is_none());
        let (name, _, prod, bus, bt) = &out[1];
        assert_eq!(name, "bluez_input.AA:BB:CC:DD:EE:FF.headset-head-unit");
        assert_eq!(prod.as_deref(), Some("WH-1000XM4"));
        assert!(bus.is_none());
        assert_eq!(bt.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
    }
}
```

(This test calls a `parse_device_list_for_test(&str)` helper the implementation must expose.)

- [ ] **Step 2: Run test to verify it fails**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test parses_properties_block 2>&1 | tail -10'
```

Expected: `parse_device_list_for_test` not found.

- [ ] **Step 3: Update `parse_device_list` to extract stable-id properties**

Replace the body of `parse_device_list` in `src/audio/sinks.rs:184-228` with this version (preserving the function name + signature update). New signature:

```rust
type PhysicalSourceRecord = (
    String,         // node_name
    String,         // description
    Option<String>, // device.product.name
    Option<String>, // device.bus_path
    Option<String>, // api.bluez5.address
);

fn parse_device_list(kind: &str, exclude: impl Fn(&str) -> bool) -> Vec<PhysicalSourceRecord> {
    let Ok(output) = Command::new("pactl").args(["list", kind]).output() else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&output.stdout);
    parse_device_list_inner(&text, exclude)
}

pub(crate) fn parse_device_list_for_test(text: &str) -> Vec<PhysicalSourceRecord> {
    parse_device_list_inner(text, |_| false)
}

fn parse_device_list_inner(text: &str, exclude: impl Fn(&str) -> bool) -> Vec<PhysicalSourceRecord> {
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut product: Option<String> = None;
    let mut bus_path: Option<String> = None;
    let mut bluez_addr: Option<String> = None;
    let mut in_properties = false;

    let flush = |out: &mut Vec<PhysicalSourceRecord>,
                 name: &mut Option<String>,
                 desc: &mut Option<String>,
                 product: &mut Option<String>,
                 bus: &mut Option<String>,
                 bt: &mut Option<String>| {
        if let Some(n) = name.take() {
            if !exclude(&n) {
                out.push((
                    n,
                    desc.take().unwrap_or_default(),
                    product.take(),
                    bus.take(),
                    bt.take(),
                ));
            }
        }
        desc.take();
        product.take();
        bus.take();
        bt.take();
    };

    for line in text.lines() {
        let trimmed = line.trim();
        let is_indented = line != trimmed && !trimmed.is_empty();

        if line.starts_with("Source #") || line.starts_with("Sink #") {
            flush(&mut out, &mut name, &mut description, &mut product, &mut bus_path, &mut bluez_addr);
            in_properties = false;
            continue;
        }
        if trimmed.is_empty() {
            in_properties = false;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Name: ") {
            name = Some(rest.to_string());
            in_properties = false;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Description: ") {
            description = Some(rest.to_string());
            in_properties = false;
            continue;
        }
        if trimmed == "Properties:" {
            in_properties = true;
            continue;
        }

        if in_properties && is_indented {
            if let Some((key, value)) = trimmed.split_once(" = ") {
                let v = value.trim_matches('"').to_string();
                match key {
                    "device.product.name" => product = Some(v),
                    "device.bus_path" => bus_path = Some(v),
                    "api.bluez5.address" => bluez_addr = Some(v),
                    _ => {}
                }
            }
        } else if !is_indented {
            in_properties = false;
        }
    }
    // Flush trailing record
    flush(&mut out, &mut name, &mut description, &mut product, &mut bus_path, &mut bluez_addr);
    out
}
```

Update `list_physical_sinks()` and `list_physical_sources()` return types:

```rust
pub fn list_physical_sinks() -> Vec<PhysicalSourceRecord> {
    parse_device_list("sinks", |name| {
        ALL_SINKS.iter().any(|(n, _, _)| *n == name)
            || ALL_SOURCES.iter().any(|(n, _, _)| *n == name)
            || name.starts_with("eq_")
    })
}

pub fn list_physical_sources() -> Vec<PhysicalSourceRecord> {
    parse_device_list("sources", |name| {
        ALL_SINKS.iter().any(|(n, _, _)| *n == name)
            || ALL_SOURCES.iter().any(|(n, _, _)| *n == name)
            || name.starts_with("eq_")
            || name.ends_with(".monitor")
    })
}
```

- [ ] **Step 4: Update ALL call sites and tuple-destructure points in `src/mixer.rs`**

Find via:

```bash
grep -n "list_physical_sources\|list_physical_sinks\|Vec<(String, String)>" src/mixer.rs
```

There are 3 invocations (lines ~294, ~347, ~396) and multiple downstream destructure points (lines ~312, ~329, ~349, ~370, ~402, ~406, ~434). For each:

- **For-loop destructures**: `for (name, desc) in list_physical_sources()` → `for (name, desc, _product, _bus_path, _bluez_address) in list_physical_sources()`.
- **Typed `Vec` declarations**: a line like `let devices: Rc<RefCell<Vec<(String, String)>>> = Rc::new(...)` becomes `let devices: Rc<RefCell<Vec<(String, String, Option<String>, Option<String>, Option<String>)>>> = Rc::new(...)`.
- **Tuple field access**: `pair.0` and `pair.1` continue to work; positions 2-4 are unused at non-mic sites.

Do **not** strip the `_` prefix from the unused fields at non-mic sites — Implementer A's later mic-source picker (already-implemented in Task A4's `reroute_mic`) is the only consumer.

- [ ] **Step 5: Run tests + full build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test 2>&1 | tail -10 && cargo build 2>&1 | tail -5'
```

Expected: All tests pass (1 new + existing); build clean.

- [ ] **Step 6: Commit**

```bash
git add src/audio/sinks.rs src/mixer.rs
git commit -m "audio: extend list_physical_sources to return stable-id chain"
```

### Task A4: Add `current_mic_source` + `preferred_mic` to `AudioRouter`, `check_mic_hotplug`, and updated `reroute_mic`

**Files:**
- Modify: `src/audio/router.rs`

- [ ] **Step 1: Write failing test for `check_mic_hotplug`**

Add to `src/audio/router.rs` in a `#[cfg(test)]` module:

```rust
#[cfg(test)]
mod hotplug_tests {
    use super::*;
    use crate::audio::persistence::MicPreference;
    type Source = (String, String, Option<String>, Option<String>, Option<String>);

    fn s(node: &str, prod: Option<&str>, bus: Option<&str>, bt: Option<&str>) -> Source {
        (
            node.to_string(),
            format!("desc for {node}"),
            prod.map(String::from),
            bus.map(String::from),
            bt.map(String::from),
        )
    }
    fn pref(node: &str, prod: &str, bus: &str, bt: &str) -> MicPreference {
        MicPreference::from_fields(node.into(), prod.into(), bus.into(), bt.into())
    }

    #[test]
    fn noop_when_no_preference() {
        let result = decide_hotplug_target(None, Some("alsa_a"), &[s("alsa_a", None, None, None)]);
        assert_eq!(result, None);
    }
    #[test]
    fn noop_when_already_on_saved() {
        let p = pref("alsa_a", "FooMic", "", "");
        let result = decide_hotplug_target(Some(&p), Some("alsa_a"), &[s("alsa_a", Some("FooMic"), None, None)]);
        assert_eq!(result, None);
    }
    #[test]
    fn exact_node_match_wins() {
        let p = pref("alsa_a", "FooMic", "", "");
        let result = decide_hotplug_target(Some(&p), Some("alsa_b"), &[s("alsa_a", Some("FooMic"), None, None)]);
        assert_eq!(result, Some("alsa_a".into()));
    }
    #[test]
    fn bluez_address_match_when_node_renamed() {
        let p = pref("bluez_old_name", "WH-1000XM4", "", "AA:BB:CC:DD:EE:FF");
        let result = decide_hotplug_target(
            Some(&p),
            Some("alsa_fallback"),
            &[s("bluez_new_name", Some("WH-1000XM4"), None, Some("AA:BB:CC:DD:EE:FF"))],
        );
        assert_eq!(result, Some("bluez_new_name".into()));
    }
    #[test]
    fn bus_path_match_when_bluez_unavailable() {
        let p = pref("alsa_old", "FooMic", "usb-0000:00:14.0-3", "");
        let result = decide_hotplug_target(
            Some(&p),
            Some("alsa_fallback"),
            &[s("alsa_new", Some("FooMic"), Some("usb-0000:00:14.0-3"), None)],
        );
        assert_eq!(result, Some("alsa_new".into()));
    }
    #[test]
    fn product_name_match_last_resort() {
        let p = pref("alsa_old", "FooMic", "", "");
        let result = decide_hotplug_target(
            Some(&p),
            Some("alsa_fallback"),
            &[s("alsa_completely_new", Some("FooMic"), None, None)],
        );
        assert_eq!(result, Some("alsa_completely_new".into()));
    }
    #[test]
    fn no_match_returns_none() {
        let p = pref("alsa_old", "FooMic", "", "");
        let result = decide_hotplug_target(
            Some(&p),
            Some("alsa_fallback"),
            &[s("alsa_other", Some("BarMic"), None, None)],
        );
        assert_eq!(result, None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test hotplug 2>&1 | tail -10'
```

Expected: `decide_hotplug_target` not found.

- [ ] **Step 3: Add the pure decision function + extend `AudioRouter`**

Add to `src/audio/router.rs`:

```rust
use crate::audio::persistence::MicPreference;

/// Pure function for unit testing — decides which node to reroute to (if any).
/// Returns `Some(node_name)` if a reroute should happen, `None` otherwise.
pub(crate) fn decide_hotplug_target(
    preferred: Option<&MicPreference>,
    current: Option<&str>,
    available: &[(String, String, Option<String>, Option<String>, Option<String>)],
) -> Option<String> {
    let p = preferred?;
    if current == Some(p.node_name.as_str()) {
        return None;
    }
    // Step 1: exact node-name
    if available.iter().any(|s| s.0 == p.node_name) {
        return Some(p.node_name.clone());
    }
    // Step 2: bluez_address
    if !p.bluez_address.is_empty() {
        if let Some(s) = available.iter().find(|s| {
            s.4.as_deref().filter(|v| !v.is_empty()).map(|v| v == p.bluez_address).unwrap_or(false)
        }) {
            return Some(s.0.clone());
        }
    }
    // Step 3: bus_path
    if !p.bus_path.is_empty() {
        if let Some(s) = available.iter().find(|s| {
            s.3.as_deref().filter(|v| !v.is_empty()).map(|v| v == p.bus_path).unwrap_or(false)
        }) {
            return Some(s.0.clone());
        }
    }
    // Step 4: product_name
    if !p.product_name.is_empty() {
        if let Some(s) = available.iter().find(|s| {
            s.2.as_deref().filter(|v| !v.is_empty()).map(|v| v == p.product_name).unwrap_or(false)
        }) {
            return Some(s.0.clone());
        }
    }
    None
}
```

Extend the `AudioRouter` struct:

```rust
pub struct AudioRouter {
    eq_pipeline: EqPipeline,
    mic_linked: bool,
    current_mic_source: Option<String>,
    preferred_mic: Option<MicPreference>,
}
```

**Update `AudioRouter::create` (`router.rs:23-80`)**. The existing block:

```rust
let mic_source = saved_routing
    .get("mic")
    .cloned()
    .or_else(|| find_headset_source().ok());
```

is now broken because `saved_routing` no longer contains the `mic` key (Task A2 skipped it). Replace it with:

```rust
let preferred_mic = super::persistence::load_mixer_routing_mic();
let mic_source = preferred_mic
    .as_ref()
    .map(|p| p.node_name.clone())
    .or_else(|| find_headset_source().ok());
```

And in the `link_mic_source` Ok branch, also set `current_mic_source = Some(source.clone())`. The full updated tail of `AudioRouter::create`:

```rust
let (mic_linked, current_mic_source) = match mic_source.as_deref() {
    Some(source) => match link_mic_source(source) {
        Ok(()) => {
            log::info!("Linked mic ({source}) → SteelSeries_Mic");
            (true, Some(source.to_string()))
        }
        Err(e) => {
            log::warn!("Failed to link mic source: {e}");
            (false, None)
        }
    },
    None => {
        log::warn!("Could not find mic source");
        (false, None)
    }
};

set_default_sink(super::sinks::GAME_SINK_NAME);
set_default_source(super::sinks::MIC_SOURCE_NAME);

Ok(AudioRouter {
    eq_pipeline,
    mic_linked,
    current_mic_source,
    preferred_mic,
})
```

**Update `reroute_mic` (`router.rs:113-132`)**. It currently calls `save_mixer_routing_entry("mic", new_source)` which writes a 2-field mic line and would clobber the 5-field mic record. Replace its body:

```rust
pub fn reroute_mic(&mut self, new_source: &str) {
    log::info!("Rerouting mic → {new_source}");

    // Build the full MicPreference by looking up the new source's properties.
    let pref = super::sinks::list_physical_sources()
        .into_iter()
        .find(|(name, _, _, _, _)| name == new_source)
        .map(|(node, _desc, product, bus, bluez)| super::persistence::MicPreference {
            node_name: node,
            product_name: product.unwrap_or_default(),
            bus_path: bus.unwrap_or_default(),
            bluez_address: bluez.unwrap_or_default(),
        })
        .unwrap_or_else(|| super::persistence::MicPreference {
            node_name: new_source.to_string(),
            product_name: String::new(),
            bus_path: String::new(),
            bluez_address: String::new(),
        });

    // Persist intent up-front. Even if the link fails, the user's preference
    // is remembered so next launch tries again.
    super::persistence::save_mixer_routing_mic(&pref);

    if self.mic_linked {
        unlink_mic_source();
    }
    match link_mic_source(new_source) {
        Ok(()) => {
            self.mic_linked = true;
            self.current_mic_source = Some(new_source.to_string());
            self.preferred_mic = Some(pref);
            log::info!("Mic rerouted to {new_source}");
        }
        Err(e) => {
            log::error!("Failed to reroute mic to {new_source}: {e}");
        }
    }
}
```

Add the public method `check_mic_hotplug`:

```rust
pub fn check_mic_hotplug(&mut self) {
    let available = super::sinks::list_physical_sources();
    let target = decide_hotplug_target(
        self.preferred_mic.as_ref(),
        self.current_mic_source.as_deref(),
        &available,
    );
    if let Some(node) = target {
        log::info!("Mic hotplug: switching to {node}");
        self.reroute_mic(&node);
    }
}
```

- [ ] **Step 4: Run tests + build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test hotplug 2>&1 | tail -10 && cargo build 2>&1 | tail -5'
```

Expected: 7 hotplug tests pass; build clean.

- [ ] **Step 5: Commit**

```bash
git add src/audio/router.rs
git commit -m "audio: add check_mic_hotplug with 4-tier stable-id match"
```

### Task A5: Implement `reconcile_stream_state` with timestamped suppression

**Files:**
- Modify: `src/audio/persistence.rs`

- [ ] **Step 1: Write failing tests for `reconcile_stream_state`**

Add to `src/audio/persistence.rs` `#[cfg(test)]` block:

```rust
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
```

These tests use `reconcile_pure` and `reconcile_pure_with_updates` — pure decision functions returning a list of moves / saved-assignment updates, separated from I/O. Both must be defined for the tests to compile.

- [ ] **Step 2: Run tests to verify they fail**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test reconcile 2>&1 | tail -10'
```

Expected: `reconcile_pure` not found, `SinkInput` field visibility issue.

- [ ] **Step 3: Make `SinkInput` `pub(crate)` and implement the pure decision functions**

In `src/audio/persistence.rs`:

```rust
#[derive(Debug)]
pub(crate) struct SinkInput {
    pub(crate) id: u32,
    pub(crate) sink_id: u32,
    pub(crate) app_name: String,
}

pub(crate) const SUPPRESSION_WINDOW: Duration = Duration::from_millis(3000);

/// Pure decision function: given inputs / state, return what moves to perform
/// and what saved-assignment updates to apply. No I/O.
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
    use std::collections::HashSet;
    let mut live_ids: HashSet<u32> = HashSet::new();
    for input in inputs {
        live_ids.insert(input.id);
        let Some(current_sink) = sinks_by_id.get(&input.sink_id) else { continue };

        if !tracked.contains_key(&input.id) {
            // Vacant
            if input.app_name == "pw-cli" { continue; }
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
            // Occupied
            let tracked_sink = tracked[&input.id].clone();
            if &tracked_sink == current_sink { continue; }

            let suppress = matches!(pre.get(&input.id), Some((pre_sink, t))
                if pre_sink == current_sink && now.duration_since(*t) < SUPPRESSION_WINDOW);
            if suppress { continue; }

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

```

(The existing `is_managed` function from `super::sinks` is already in scope via the file's existing `use super::sinks::{is_managed, migrate_legacy_name};`. Do not add a local wrapper.)

Add `use std::time::{Duration, Instant};` at the top of the file.

- [ ] **Step 4: Add the I/O wrapper `reconcile_stream_state`**

```rust
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
        &inputs, &sinks_by_id, &saved,
        tracked, self_move_pre_sink, &mut moves, &mut updates,
        Instant::now(),
    );

    for (id, target) in moves {
        match move_sink_input(id, &target) {
            Ok(()) => log::info!("Auto-routed stream {id} → {target}"),
            Err(e) => {
                log::warn!("Auto-route {id} → {target} failed: {e}");
                // Roll back: use the pre-move sink from self_move_pre_sink (where
                // the Vacant branch stashed it) so the next tick treats this as
                // stable, not as a phantom user move.
                let rollback = self_move_pre_sink
                    .get(&id)
                    .map(|(s, _)| s.clone())
                    .unwrap_or_else(|| target.clone());
                tracked.insert(id, rollback);
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

pub fn update_saved_assignment(app: &str, sink: &str) {
    let Some(path) = config_path() else { return; };
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
```

**Build-clean policy at A5:** the existing `pub fn restore_new_streams(seen_ids: &mut HashSet<u32>)` has one caller (`src/app.rs:1393`) and `pub fn initial_seen_ids()` has one caller (`src/app.rs:1391`). Do **not** delete them in Task A5 — leave them in place; Task A7 will rewrite the caller block and delete both helpers in the same commit. This keeps every intermediate commit building.

Also amend `save_assignments` at the bottom of the function — find the `else { assignments.remove(&input.app_name); }` block (around line 113) and remove it. The new monotonic save behavior is "only add/update entries, never destroy on observation."

- [ ] **Step 5: Run tests + build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test reconcile 2>&1 | tail -10 && cargo build 2>&1 | tail -10'
```

Expected: 6 reconcile tests pass; build clean. `restore_new_streams` and `initial_seen_ids` are still in `persistence.rs` per the build-clean policy above — they'll be deleted in Task A7.

- [ ] **Step 6: Commit**

```bash
git add src/audio/persistence.rs
git commit -m "audio: add reconcile_stream_state with timestamped suppression"
```

### Task A6: Create `src/audio/state_sync.rs`

**Files:**
- Create: `src/audio/state_sync.rs`
- Modify: `src/audio/mod.rs` (add `pub mod state_sync;`)

- [ ] **Step 1: Write the new module**

Create `src/audio/state_sync.rs`:

```rust
//! Unified 2-second state-sync tick. Owns:
//! - User-move detection (Issue 1) via persistence::reconcile_stream_state
//! - Mic hotplug check (Issue 2) via AudioRouter::check_mic_hotplug
//! - Virtual-source volume capture (Issue 3) via capture_virtual_volumes
//!
//! See docs/superpowers/specs/2026-05-13-routing-volume-clips-fixes-design.md.

use std::collections::HashMap;
use std::time::Instant;

use super::persistence;
use super::router::AudioRouter;
use super::sinks::{self, AUX_SINK_NAME, MIC_SOURCE_NAME, MUSIC_SINK_NAME};

#[derive(Default)]
pub struct StateSyncState {
    pub tracked: HashMap<u32, String>,
    pub self_move_pre_sink: HashMap<u32, (String, Instant)>,
    pub last_saved_volumes: HashMap<String, u32>,
}

impl StateSyncState {
    pub fn new_seeded() -> Self {
        Self {
            tracked: persistence::initial_tracked(),
            self_move_pre_sink: HashMap::new(),
            last_saved_volumes: persistence::load_volumes(),
        }
    }
}

pub fn tick(state: &mut StateSyncState, router: &mut AudioRouter) {
    persistence::reconcile_stream_state(&mut state.tracked, &mut state.self_move_pre_sink);
    router.check_mic_hotplug();
    capture_virtual_volumes(&mut state.last_saved_volumes);
}

fn capture_virtual_volumes(last_saved: &mut HashMap<String, u32>) {
    for name in [MUSIC_SINK_NAME, AUX_SINK_NAME] {
        if let Ok(vol) = sinks::get_sink_volume(name) {
            if last_saved.get(name).copied() != Some(vol) {
                persistence::save_volume_entry(name, vol);
                last_saved.insert(name.to_string(), vol);
            }
        }
    }
    if let Ok(vol) = sinks::get_source_volume(MIC_SOURCE_NAME) {
        if last_saved.get(MIC_SOURCE_NAME).copied() != Some(vol) {
            persistence::save_volume_entry(MIC_SOURCE_NAME, vol);
            last_saved.insert(MIC_SOURCE_NAME.to_string(), vol);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_no_change_writes_nothing() {
        // No direct test for write-side without mocking pactl, but this asserts
        // capture_virtual_volumes is callable with the cache.
        let mut last = HashMap::new();
        last.insert(MIC_SOURCE_NAME.to_string(), 100);
        // Just exercise the path; we can't easily mock pactl, so the test only
        // confirms there's no panic.
        capture_virtual_volumes(&mut last);
    }
}
```

- [ ] **Step 2: Register the module in `src/audio/mod.rs`**

Add `pub mod state_sync;` to `src/audio/mod.rs` after the existing module declarations.

- [ ] **Step 3: Build to verify the module compiles**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo build 2>&1 | tail -10'
```

Expected: clean build. The existing `restore_new_streams` callers in app.rs are still intact (per A5's build-clean policy); they'll be replaced in Task A7.

- [ ] **Step 4: Commit**

```bash
git add src/audio/state_sync.rs src/audio/mod.rs
git commit -m "audio: add state_sync module orchestrating Issues 1+2+3"
```

### Task A7: Wire state-sync tick + first-tick seeding in `app.rs` and delete stub helpers

**Files:**
- Modify: `src/app.rs` (region 1: state-sync timer block around the existing `restore_new_streams` watcher)
- Modify: `src/audio/persistence.rs` (delete now-unused `restore_new_streams` + `initial_seen_ids`)

- [ ] **Step 1: Locate the existing watcher block**

```bash
grep -n "restore_new_streams\|STREAM_WATCH_SECS\|initial_seen_ids" src/app.rs
```

The watcher is registered around `app.rs:1392` (in the `connect_activate` closure). The block looks roughly like:

```rust
let mut seen_ids = persistence::initial_seen_ids();
glib::timeout_add_seconds_local(STREAM_WATCH_SECS, move || {
    persistence::restore_new_streams(&mut seen_ids);
    glib::ControlFlow::Continue
});
```

- [ ] **Step 2: Replace it with the state-sync tick**

The actual variable in `app.rs` is `resources: Rc<RefCell<Option<AppResources>>>` (verify with `grep -n 'let resources' src/app.rs`). The `AppResources` struct contains `router: Rc<RefCell<Option<AudioRouter>>>` already, so the tick closure needs to traverse through the outer `Option<AppResources>` to reach the inner router.

Replace the existing `restore_new_streams` watcher block with:

```rust
let sync_state = Rc::new(RefCell::new(state_sync::StateSyncState::new_seeded()));
let sync_state_for_tick = sync_state.clone();
let resources_for_tick = resources.clone();
glib::timeout_add_seconds_local(STREAM_WATCH_SECS, move || {
    let mut state = sync_state_for_tick.borrow_mut();
    if let Some(res) = resources_for_tick.borrow().as_ref() {
        if let Some(router) = res.router.borrow_mut().as_mut() {
            state_sync::tick(&mut state, router);
        }
    }
    glib::ControlFlow::Continue
});
```

Add `use crate::audio::state_sync;` near the existing `use crate::audio::...` imports at the top of `src/app.rs`.

After the new watcher block is in place, delete `pub fn restore_new_streams(...)` and `pub fn initial_seen_ids() -> HashSet<u32>` from `src/audio/persistence.rs` — they have no callers after this commit.

- [ ] **Step 3: Build and confirm the old watcher is fully replaced**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo build 2>&1 | tail -10'
```

Expected: clean build. Any lingering reference to `restore_new_streams` or `initial_seen_ids` indicates a missed call site.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs src/audio/persistence.rs
git commit -m "app: wire state_sync tick + delete restore_new_streams stub"
```

### Task A8: Add shutdown volume capture

**Files:**
- Modify: `src/app.rs` (region 2: `connect_shutdown` handler around line 1464)

- [ ] **Step 1: Locate the shutdown handler**

```bash
grep -n "connect_shutdown\|save_assignments\|drop(res)" src/app.rs
```

The handler is around line 1462-1478.

- [ ] **Step 2: Add the capture block immediately after `save_assignments()` and before `drop(res)`**

Insert:

```rust
// Capture virtual-source volumes one final time before destroying sinks.
// The periodic state-sync tick handles steady-state; this catches anything
// the user changed in the last <2s window.
for name in [sinks::MUSIC_SINK_NAME, sinks::AUX_SINK_NAME] {
    if let Ok(vol) = sinks::get_sink_volume(name) {
        persistence::save_volume_entry(name, vol);
    }
}
if let Ok(vol) = sinks::get_source_volume(sinks::MIC_SOURCE_NAME) {
    persistence::save_volume_entry(sinks::MIC_SOURCE_NAME, vol);
}
```

- [ ] **Step 3: Build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo build 2>&1 | tail -5'
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "app: capture virtual-source volumes at shutdown"
```

### Task A9: Apply `load_volumes` after `VirtualSinks::create` in `init_pipeline`

**Files:**
- Modify: `src/app.rs` (region 3: `init_pipeline` around line 1502-1517)

- [ ] **Step 1: Locate `init_pipeline`**

```bash
grep -n "fn init_pipeline\|VirtualSinks::create\|AudioRouter::create" src/app.rs
```

- [ ] **Step 2: Insert the apply-volumes loop**

Between `let sinks = VirtualSinks::create()?;` and `let router = AudioRouter::create(&headset_sink)?;`, add:

```rust
// Restore user-set volumes for Music/Aux/Mic. The virtual sinks above are
// recreated every launch so their volumes reset; load_volumes() returns
// whatever the periodic capture or shutdown handler last saved.
for (name, pct) in persistence::load_volumes() {
    let result = if name == sinks::MIC_SOURCE_NAME {
        sinks::set_source_volume(&name, pct)
    } else {
        sinks::set_sink_volume(&name, pct)
    };
    if let Err(e) = result {
        log::warn!("Failed to restore volume for {name}: {e}");
    } else {
        log::info!("Restored volume: {name} = {pct}%");
    }
}
```

- [ ] **Step 3: Build + run unit tests**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo build 2>&1 | tail -5 && cargo test 2>&1 | tail -15'
```

Expected: clean build; all tests pass (existing + new from A2/A3/A4/A5/A6).

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "app: apply load_volumes after VirtualSinks::create"
```

### Task A10: Add volume save-on-change in `mixer.rs`

**Files:**
- Modify: `src/mixer.rs` (slider `connect_value_changed` handler)

- [ ] **Step 1: Locate the slider handler**

```bash
grep -n "connect_value_changed\|set_sink_volume\|set_source_volume\|save_volume_entry" src/mixer.rs
```

The mixer's slider handler is around line 194-220.

- [ ] **Step 2: Add the save block inside the handler**

After the existing `set_sink_volume` / `set_source_volume` call inside `connect_value_changed`, add:

```rust
// Persist for channels whose volume isn't owned elsewhere:
// Game/Chat → HID-dial-owned; Master → WirePlumber-owned.
if name == sinks::MUSIC_SINK_NAME
    || name == sinks::AUX_SINK_NAME
    || name == sinks::MIC_SOURCE_NAME
{
    persistence::save_volume_entry(name, pct);
}
```

Add `use crate::audio::persistence;` near the top of the file if not present.

- [ ] **Step 3: Build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo build 2>&1 | tail -5'
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/mixer.rs
git commit -m "mixer: persist slider changes for Music/Aux/Mic"
```

### Task A11 (CONDITIONAL — only if Phase 0 revealed H_B)

**Files:**
- Modify: `src/audio/sinks.rs` (`create_pw_node` body, caller in `VirtualSinks::create`)
- Modify: `src/app.rs` (region 3, where the apply-volumes loop lives in Task A9 — relocate the volume application to Task A11's init path)

**Skip this task if Phase 0 said H_A is the candidate.**

In the H_B world, `pactl set-source-volume` silently fails on `Audio/Source/Virtual` nodes. The fix is to write the initial volume into the `Props` block at node-creation time via `pw-cli`, bypassing the broken layer.

- [ ] **Step 1: Look up saved volumes before `cleanup_orphaned` runs**

In `src/audio/sinks.rs`, modify `VirtualSinks::create()`:

```rust
pub fn create() -> Result<Self, String> {
    cleanup_orphaned();

    let saved = crate::audio::persistence::load_volumes();
    let initial_vol = |name: &str| -> Option<u32> { saved.get(name).copied() };

    for (name, description, position) in ALL_SINKS {
        create_pw_node_with_volume(name, description, "Audio/Sink", position, initial_vol(name))?;
        log::info!("Created sink {name}");
    }
    for (name, description, position) in ALL_SOURCES {
        create_pw_node_with_volume(name, description, "Audio/Source/Virtual", position, initial_vol(name))?;
        log::info!("Created source {name}");
    }

    Ok(VirtualSinks { _created: true })
}
```

- [ ] **Step 2: Add `create_pw_node_with_volume` next to the existing `create_pw_node`**

The existing `create_pw_node` builds a `pw-cli create-node adapter '{ ... }'` invocation. The new wrapper appends a `Props { channelVolumes }` entry when `initial_volume_pct` is `Some(pct)`. Convert percent to PipeWire's expected linear via cube — PipeWire treats `channelVolumes` as cubic-scaled volumes per the `Audio EQ Cookbook` convention used by PulseAudio:

```rust
fn create_pw_node_with_volume(
    name: &str,
    description: &str,
    media_class: &str,
    position: &str,
    initial_volume_pct: Option<u32>,
) -> Result<(), String> {
    let channel_volumes_section = match initial_volume_pct {
        Some(pct) => {
            let linear: f64 = (pct as f64 / 100.0).powf(3.0);
            // Comma-separate one value per channel based on `position`.
            let n_channels = position.split(',').count();
            let values: Vec<String> = (0..n_channels).map(|_| format!("{linear:.6}")).collect();
            format!(" channelVolumes = [{}]", values.join(","))
        }
        None => String::new(),
    };

    // Existing props string composition continues, with channel_volumes_section
    // appended inside the `props = { ... }` block.
    // IMPORTANT: include all existing props the original create_pw_node sets,
    // most critically `object.linger=true` which makes the node persist
    // beyond the pw-cli client session. Without it, the virtual sinks vanish
    // when pw-cli exits and the entire pipeline collapses.
    let props = format!(
        "factory.name=support.null-audio-sink \
         node.name={name} node.description=\"{description}\" \
         media.class={media_class} audio.position=[{position}] \
         monitor.channel-volumes=true monitor.passthrough=true \
         object.linger=true{channel_volumes_section}",
    );

    let output = Command::new("pw-cli")
        .args(["create-node", "adapter", &format!("{{ factory.name=support.null-audio-sink {props} }}")])
        .output()
        .map_err(|e| format!("Failed to run pw-cli: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "pw-cli create-node failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
```

(Reuse the rest of the existing `create_pw_node`'s argument-builder logic — the snippet above is a sketch; merge with the existing function rather than duplicating it. The new `channel_volumes_section` is the only added field.)

- [ ] **Step 3: Remove the duplicate apply-volumes loop from `init_pipeline` (Task A9)**

In `src/app.rs` region 3, the apply-volumes loop added in Task A9 is now redundant because the initial volume is set at create-time. **Keep** the loop only as a defensive belt-and-braces: it's harmless if `pactl set-source-volume` is silently ignored, and it remains useful if Phase 0's H_A turned out to be partially true (mixed behavior). No change needed.

- [ ] **Step 4: Run tests + build + manual verify**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo build 2>&1 | tail -5 && cargo test 2>&1 | tail -10'
```

Expected: clean. **Critical sanity-check**: PipeWire's adapter factory accepts node-property and stream-property keys, but `channelVolumes` is a SPA_PROP and may be silently ignored when passed as a node-creation factory property. Run the live app, then in another shell:

```bash
pw-cli i <node-id-of-SteelSeries_Mic> | grep -i volume
```

If `channelVolumes` is missing or all zeros, the H_B fallback is itself broken — switch to a two-step approach: create the node without channelVolumes (the existing create_pw_node), then immediately after, run `pw-cli s <node-id> Props '{ channelVolumes: [linear, linear] }'` from Rust via Command::new. This requires capturing the node-id from `pw-cli create-node` output. If both approaches fail, escalate to team lead.

Otherwise (volume honored): `pactl set-source-volume SteelSeries_Mic 70%` in the live app, quit, relaunch, and confirm `pactl get-source-volume SteelSeries_Mic` returns 70%.

- [ ] **Step 5: Commit**

```bash
git add src/audio/sinks.rs
git commit -m "audio: write initial volumes via pw-cli Props at create-time (H_B fallback)"
```

### Task A12: Implementer A self-review checklist

- [ ] **Step 1: Confirm all `app.rs` regions are within scope**

```bash
git diff main..HEAD -- src/app.rs | grep -E "^@@" | head -20
```

Expected hunk headers in three regions: around 1390 (timer block), 1462 (shutdown), 1502 (init_pipeline). No edits outside.

- [ ] **Step 2: Confirm no `src/clips/` or `src/window.rs` edits**

```bash
git diff main..HEAD --stat -- src/clips/ src/window.rs
```

Expected: zero changes.

- [ ] **Step 3: Run the full test suite once more**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume && cargo test 2>&1 | tail -20'
```

Expected: all tests pass.

- [ ] **Step 4: Generate the report**

Write a single-message report summarizing:
- Tasks completed (list)
- New tests added (with names)
- Files modified (with hunk counts)
- Any deviation from the spec (with reason)
- Build / test status

Hand this report to **`qa-code-auditor`** as the next agent (not back to the team lead).

---

## Phase 1B — Clips UI Bundle (Implementer B)

> **Implementer B**: All your work lives in worktree `/var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui/` branched off `clipping-system`. Build via `distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo build'`. Do not edit any files in `src/audio/` — those belong to Implementer A. The only `app.rs` region you touch is the action-registration cluster around lines 715-788 (the existing `app.save-clip`, `app.save-clip-short`, etc. block).

### Task B1: Add `BufferState::Paused` + `user_paused` to `BufferController`

**Files:**
- Modify: `src/clips/buffer.rs`

- [ ] **Step 1: Write failing tests for pause/resume transitions**

Note: `BufferController` does not currently expose a way to force-transition to `Armed` from tests — existing tests drive into `Armed` via `on_portal_pick_complete → on_game_started → on_backend_event(BackendEvent::Armed, ...)`. To keep these tests simple, Implementer B adds a small `#[cfg(test)]` testing seam in Step 3. The seam is `pub fn set_state_for_test(&mut self, s: BufferState)`.

Add to `src/clips/buffer.rs`'s `#[cfg(test)]` module:

```rust
#[test]
fn pause_from_armed_transitions_to_paused() {
    let mut bc = BufferController::new(CaptureConfig::default());
    bc.has_portal_pick = true;
    bc.set_state_for_test(BufferState::Armed);
    assert_eq!(bc.state(), BufferState::Armed);
    bc.pause();
    assert_eq!(bc.state(), BufferState::Paused);
    assert!(bc.user_paused());
}

#[test]
fn resume_from_paused_to_idle() {
    let mut bc = BufferController::new(CaptureConfig::default());
    bc.has_portal_pick = true;
    bc.pause();
    assert_eq!(bc.state(), BufferState::Paused);
    bc.resume();
    assert_eq!(bc.state(), BufferState::Idle);
    assert!(!bc.user_paused());
}

#[test]
fn pause_clears_pending_reconfigure() {
    let mut bc = BufferController::new(CaptureConfig::default());
    bc.has_portal_pick = true;
    bc.set_state_for_test(BufferState::Armed);
    bc.pending_reconfigure = true;
    bc.pause();
    assert!(!bc.pending_reconfigure);
}

#[test]
fn maybe_arm_respects_user_paused() {
    let mut bc = BufferController::new(CaptureConfig::default());
    bc.has_portal_pick = true;
    bc.always_armed = true;
    bc.user_paused_set(true);
    assert!(!bc.should_arm(), "user_paused must suppress always_armed");
    bc.user_paused_set(false);
    assert!(bc.should_arm(), "should_arm reflects always_armed when not paused");
}

#[test]
fn resume_when_always_armed_re_arms_immediately() {
    use std::sync::mpsc::channel;
    let (tx, rx) = channel();
    let mut bc = BufferController::new(CaptureConfig::default());
    bc.has_portal_pick = true;
    bc.always_armed = true;
    bc.pause();
    assert_eq!(bc.state(), BufferState::Paused);
    bc.resume(&tx);
    assert_eq!(bc.state(), BufferState::Arming, "resume must re-evaluate maybe_arm");
    assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
}

#[test]
fn pause_from_uninitialized_is_a_noop() {
    let mut bc = BufferController::new(CaptureConfig::default());
    // has_portal_pick = false → state stays Uninitialized
    bc.pause();
    assert_eq!(bc.state(), BufferState::Uninitialized, "pause must not strand the user");
    assert!(!bc.user_paused());
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo test --lib pause 2>&1 | tail -15'
```

Expected: `Paused` variant missing, `pause` / `resume` / `user_paused` / `user_paused_set` / `should_arm` methods missing.

- [ ] **Step 3: Add the variant, field, and methods**

In `src/clips/buffer.rs`:

```rust
pub enum BufferState {
    Uninitialized,
    Idle,
    Arming,
    Armed,
    Saving,
    ErrorState,
    Paused,   // NEW: user-initiated stop; buffer is lost
}

pub struct BufferController {
    // ... existing fields ...
    user_paused: bool,
}
```

Initialize `user_paused: false` in `new`. Add methods:

```rust
pub fn user_paused(&self) -> bool {
    self.user_paused
}

pub fn user_paused_set(&mut self, v: bool) {
    self.user_paused = v;
}

pub fn pause(&mut self) {
    // Guard: pausing from Uninitialized / ErrorState would strand the user.
    // The button is insensitive in those states (per B6 refresh), but the
    // GAction can still be activated via D-Bus; guard defensively.
    if matches!(self.state, BufferState::Uninitialized | BufferState::ErrorState) {
        return;
    }
    self.user_paused = true;
    self.pending_reconfigure = false;
    self.state = BufferState::Paused;
}

pub fn resume(&mut self, cmd_tx: &Sender<ClipCommand>) {
    self.user_paused = false;
    self.state = BufferState::Idle;
    // Re-evaluate the arm conditions. If a game is already running and
    // auto_arm (or always_armed) is on, we want to transition straight to
    // Arming rather than wait for the next on_game_started event — which
    // may never come because the detector only fires on transitions.
    self.maybe_arm(cmd_tx);
}

pub fn should_arm(&self) -> bool {
    if !self.has_portal_pick { return false; }
    if self.user_paused { return false; }
    self.always_armed || (self.auto_arm && self.current_game.is_some())
}

#[cfg(test)]
pub fn set_state_for_test(&mut self, s: BufferState) {
    self.state = s;
}
```

Update the existing `maybe_arm` (or equivalent) to call `should_arm()`. If `maybe_arm` already has inline arming logic, replace its condition with `if !self.should_arm() { return; }`. Verify by grep:

```bash
grep -n "maybe_arm\|always_armed\|auto_arm" src/clips/buffer.rs
```

Also add a `Paused` arm wherever `BufferState` is matched (`indicator.rs`, possibly `mod.rs`). For `indicator.rs`, the new state's user-visible label is **"Paused"** with `.dot-error` CSS class (or a new `.dot-paused` if the implementer prefers — visually similar to ErrorState but with non-error semantics; reuse `.dot-error` for now and add a comment).

- [ ] **Step 4: Run tests + build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo test --lib pause 2>&1 | tail -10 && cargo build 2>&1 | tail -10'
```

Expected: 4 new tests pass; build clean (the indicator.rs match arms may fail to compile if any `match state` is non-exhaustive — fix by adding `BufferState::Paused` arm in those matches).

- [ ] **Step 5: Commit**

```bash
git add src/clips/buffer.rs src/clips/indicator.rs
git commit -m "clips: add BufferState::Paused + user_paused field"
```

### Task B2: Add `ClipCommand::PauseRecording` + `ResumeRecording` AND supervisor match arms

> Merged from prior B2+B3 because adding the variants without arms breaks `backend.rs`'s exhaustive match. Single commit keeps every intermediate worktree state building.

**Files:**
- Modify: `src/clips/mod.rs`
- Modify: `src/clips/backend.rs` (around the command loop at line 1057)

- [ ] **Step 1: Add the variants**

In `src/clips/mod.rs`, find the `ClipCommand` enum and add:

```rust
pub enum ClipCommand {
    // ... existing variants ...
    PauseRecording,
    ResumeRecording,
}
```

- [ ] **Step 2: Locate the command loop**

```bash
grep -n "fn run_backend\|Ok(ClipCommand::" src/clips/backend.rs
```

Around `backend.rs:1057-1100`.

- [ ] **Step 3: Add the new arms**

Insert these arms in the `match` block (alongside the existing `StartReplay`, `SaveClip`, etc.):

```rust
Ok(ClipCommand::PauseRecording) => {
    log::info!("backend: handling PauseRecording");
    restart_attempts.clear();
    if active.is_some() {
        disarm(&mut active);
    }
}
Ok(ClipCommand::ResumeRecording) => {
    log::info!("backend: handling ResumeRecording");
    restart_attempts.clear();
    // Resume only clears the limiter; BufferController on the GTK side
    // re-enters Idle and the next BufferController tick triggers StartReplay
    // if conditions are met (auto_arm + game, or always_armed).
}
```

- [ ] **Step 4: Build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo build 2>&1 | tail -5'
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/clips/mod.rs src/clips/backend.rs
git commit -m "clips: add Pause/Resume command variants + backend handlers"
```

### Task B3: (RESERVED — merged into B2 to keep every commit building)

### Task B4: Add `save_hotkey_display` field to `ClipSettings`

**Files:**
- Modify: `src/clips/settings.rs`

- [ ] **Step 1: Add the field with load/save support**

In `src/clips/settings.rs`:

```rust
pub struct ClipSettings {
    // ... existing fields ...
    pub save_hotkey_display: String,
}
```

In `Default for ClipSettings`:

```rust
save_hotkey_display: "ALT+S".to_string(),
```

In `load()`, add a match arm:

```rust
"save_hotkey_display" => s.save_hotkey_display = v.to_string(),
```

In `save()`, add a line:

```rust
body.push_str(&format!("save_hotkey_display={}\n", s.save_hotkey_display));
```

- [ ] **Step 2: Build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo build 2>&1 | tail -5'
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/clips/settings.rs
git commit -m "clips: add save_hotkey_display field to ClipSettings"
```

### Task B5: Capture hotkey display after `bind_shortcuts`

**Files:**
- Modify: `src/clips/hotkey.rs`

- [ ] **Step 1: Locate the bind_shortcuts call**

```bash
grep -n "bind_shortcuts\|list_shortcuts" src/clips/hotkey.rs
```

Around `hotkey.rs:130`.

- [ ] **Step 2: Add the list_shortcuts read-back after a successful bind**

After the `bind_result?;` line at `src/clips/hotkey.rs:137` (where the existing `bindings` variable is bound and `bind_result` consumed), add:

```rust
// Read back the portal's display string for the bound chord and persist.
use ashpd::desktop::global_shortcuts::ListShortcutsOptions;
match proxy.list_shortcuts(&session, ListShortcutsOptions::default()).await {
    Ok(req) => match req.response() {
        Ok(resp) => {
            if let Some(s) = resp.shortcuts().iter().find(|s| s.id() == "save-clip") {
                let display = s.trigger_description().to_string();
                if let Some(cell) = settings_cell.as_ref() {
                    cell.borrow_mut().save_hotkey_display = display;
                    let _ = crate::clips::settings::save(&cell.borrow());
                }
            }
        }
        Err(e) => log::warn!("list_shortcuts response decode failed: {e}"),
    },
    Err(e) => log::warn!("list_shortcuts call failed: {e}"),
}
```

`list_shortcuts` in ashpd 0.11.1 requires both `session: &Session<Self>` and `options: ListShortcutsOptions` arguments. The default `ListShortcutsOptions` has no required fields. Variable naming in the existing code uses `bindings` for the `Vec<NewShortcut>` argument passed to `bind_shortcuts`; that's separate from the per-`Shortcut` items returned by `list_shortcuts`.

The `settings_cell: Option<Rc<RefCell<ClipSettings>>>` must be threaded into `run_global_shortcuts` (around `src/clips/hotkey.rs:114`) and `rebind_shortcuts` (likewise). Update their callers in `src/app.rs:1017` and `src/app.rs:1107` — both are within Implementer B's **second declared `app.rs` region** (see "Coordination point" below). Pass `Some(settings_cell.clone())` where `settings_cell` is the same `Rc<RefCell<ClipSettings>>` the existing `clips_settings_ctx_partial` already carries.

To minimize signature churn: add `settings_cell: Option<Rc<RefCell<ClipSettings>>>` as the last parameter of both `run_global_shortcuts` and `rebind_shortcuts`; existing other callers (if any) pass `None`.

- [ ] **Step 3: Update stale ashpd 0.10 comments**

Find and update:
- `src/clips/hotkey.rs:153` — replace the "ashpd 0.10 doesn't expose the (proposed) portal `ConfigureShortcuts` method directly" comment with "ashpd 0.11.1 exposes list_shortcuts; ConfigureShortcuts is still a portal-level API not available."
- `src/clips/settings.rs:322` — similar update.

- [ ] **Step 4: Build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo build 2>&1 | tail -10'
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/clips/hotkey.rs src/clips/settings.rs
git commit -m "clips: persist save_hotkey_display via list_shortcuts after bind"
```

### Task B6: Build the clips section widget

**Files:**
- Modify: `src/window.rs`

- [ ] **Step 1: Add `ClipsSectionWidgets` struct + `build_clips_section`**

In `src/window.rs`, add (place near `build_status_card`):

```rust
pub struct ClipsSectionWidgets {
    pub indicator: crate::clips::StatusIndicator,
    pub save_button: gtk::Button,
    pub pause_button: gtk::Button,
    pub duration_label: gtk::Label,
    pub hotkey_label: gtk::Label,
}

impl ClipsSectionWidgets {
    pub fn refresh(&self, buffer_state: crate::clips::buffer::BufferState, user_paused: bool, settings: &crate::clips::settings::ClipSettings) {
        // Pause button label/sensitivity
        use crate::clips::buffer::BufferState as S;
        let (label, sensitive) = match buffer_state {
            S::Uninitialized => ("Pause Recording", false),
            S::Idle | S::Arming | S::Armed => ("Pause Recording", true),
            S::Saving => ("Pause Recording", false),
            S::ErrorState => ("Pause Recording", false),
            S::Paused => ("Resume Recording", true),
        };
        // user_paused takes precedence on the label
        let label = if user_paused { "Resume Recording" } else { label };
        self.pause_button.set_label(label);
        self.pause_button.set_sensitive(sensitive);

        // Duration label
        let secs = settings.buffer_length;
        let text = if secs < 60 {
            format!("Recording last {secs} seconds")
        } else if secs < 3600 {
            format!("Recording last {} minutes", (secs as f64 / 60.0).round() as u32)
        } else {
            format!("Recording last {} hour{}", secs / 3600, if secs >= 7200 { "s" } else { "" })
        };
        self.duration_label.set_label(&text);

        // Hotkey label
        let hk = if settings.save_hotkey_display.is_empty() {
            "Hotkey: ALT+S".to_string()
        } else {
            format!("Hotkey: {}", settings.save_hotkey_display)
        };
        self.hotkey_label.set_label(&hk);
    }
}

fn build_clips_section() -> (adw::PreferencesGroup, ClipsSectionWidgets) {
    let group = adw::PreferencesGroup::builder().title("Clips").build();

    // Row 1: status indicator
    let indicator = crate::clips::build_status_indicator();
    let indicator_holder = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::Center)
        .margin_top(8)
        .margin_bottom(4)
        .build();
    indicator_holder.append(&indicator.root);
    let row_indicator = gtk::ListBoxRow::builder()
        .child(&indicator_holder)
        .activatable(false)
        .selectable(false)
        .build();
    group.add(&row_indicator);

    // Row 2: action row — Save button | Pause toggle | duration | hotkey
    let action_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_start(15)
        .margin_end(15)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    let save_button = gtk::Button::builder().label("Save Clip Now").build();
    save_button.add_css_class("suggested-action");
    save_button.set_action_name(Some("app.save-clip"));
    action_box.append(&save_button);

    let pause_button = gtk::Button::builder().label("Pause Recording").build();
    pause_button.set_action_name(Some("app.pause-recording-toggle"));
    pause_button.set_tooltip_text(Some("Pause recording. The last N seconds of recording will be lost."));
    action_box.append(&pause_button);

    let spacer = gtk::Box::builder().hexpand(true).build();
    action_box.append(&spacer);

    let duration_label = gtk::Label::builder().label("Recording last 60 seconds").build();
    duration_label.add_css_class("dim-label");
    action_box.append(&duration_label);

    let hotkey_label = gtk::Label::builder().label("Hotkey: ALT+S").build();
    hotkey_label.add_css_class("dim-label");
    action_box.append(&hotkey_label);

    let row_action = gtk::ListBoxRow::builder()
        .child(&action_box)
        .activatable(false)
        .selectable(false)
        .build();
    group.add(&row_action);

    let widgets = ClipsSectionWidgets {
        indicator,
        save_button,
        pause_button,
        duration_label,
        hotkey_label,
    };
    (group, widgets)
}
```

- [ ] **Step 2: Build to verify the new function compiles standalone**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo build 2>&1 | tail -10'
```

Expected: clean (the section isn't attached to anything yet — that's Task B7).

- [ ] **Step 3: Commit**

```bash
git add src/window.rs
git commit -m "window: add build_clips_section + ClipsSectionWidgets"
```

### Task B7: Remove the clips indicator from `build_status_card` AND attach the new section to grid row 1

> Merged with prior B8 because removing the indicator from Status without immediately providing a new home for `Widgets.clips_indicator` leaves the build broken.

**Files:**
- Modify: `src/window.rs`

- [ ] **Step 1: Locate the clips indicator inside Status card**

```bash
grep -n "clips_indicator\|clips_row\|build_status_indicator" src/window.rs
```

The indicator is added as a third row inside `build_status_card`'s PreferencesGroup, around lines 616-637.

- [ ] **Step 2: Remove the `clips_indicator` + `clips_row` from `build_status_card` and `StatusResult`, AND attach the new section**

Combined commit. Edit `build_status_card` to delete the block that builds the indicator and adds it as a row. Remove `clips_indicator` and `clips_row` fields from `StatusResult`. Remove the `row_height.add_widget(&status_result.clips_row);` line from `build_dashboard_page`.

Then in `build_dashboard_page`, after the existing `grid.attach(&device_card, 1, 0, 1, 1);` line, add:

```rust
let (clips_section, clips_section_widgets) = build_clips_section();
grid.attach(&clips_section, 0, 1, 2, 1);  // column span 2
```

Update the `Widgets` struct to add `clips_section`:

```rust
pub struct Widgets {
    // ... existing fields ...
    pub clips_section: Option<ClipsSectionWidgets>,
}
```

Source `clips_indicator: clips_section_widgets.indicator.clone()`. The `StatusIndicator` is `#[derive(Clone)]` (verified at `src/clips/indicator.rs:35`) with all fields being GObject refs (`gtk::Widget`, `gtk::Label`, `gtk::Image`, `gtk::Button`) — Clone is reference-clone via the underlying GObject refcount, so the cloned indicator and the one in `ClipsSectionWidgets` share the same widget tree. Mutating one (via `set_state`) updates both. Initialize as `clips_section: Some(clips_section_widgets)` in `build_dashboard_page`'s return.

Add a public accessor on `ChatMixWindow`:

```rust
impl ChatMixWindow {
    pub fn refresh_clips_section(
        &self,
        state: crate::clips::buffer::BufferState,
        paused: bool,
        settings: &crate::clips::settings::ClipSettings,
    ) {
        if let Some(section) = &self.inner.borrow().clips_section {
            section.refresh(state, paused, settings);
        }
    }
}
```

The action handler (Task B9) will call `window.refresh_clips_section(...)` rather than poking the private `inner` field directly.

- [ ] **Step 3: Update the on-state-change refresh path**

Find where `clips_indicator.set_state` is currently called (`grep -n clips_indicator.set_state src/window.rs`). Add an adjacent call to refresh the section's pause-button label and labels via the new accessor pattern in `ChatMixWindow::set_clips_state` (or wherever the indicator update lives now).

- [ ] **Step 4: Build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo build 2>&1 | tail -10'
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/window.rs
git commit -m "window: relocate clips indicator to dedicated row-1 section"
```

### Task B8: (RESERVED — merged into B7 to keep every commit building)

### Task B9: Add `app.pause-recording-toggle` GAction

**Files:**
- Modify: `src/app.rs` (region 4: action-registration cluster around lines 715-788)

- [ ] **Step 1: Locate the existing `save-clip` action cluster**

```bash
grep -n "save-clip\|app.add_action_entries" src/app.rs
```

- [ ] **Step 2: Add the new GAction adjacent to the existing save-clip block**

Insert after the `app.add_action_entries([save_action]);` for `save-clip`:

```rust
{
    let buffer = buffer.clone();
    let cmd_tx = cmd_tx.clone();
    let settings_cell_for_action = settings_cell.clone();  // Rc<RefCell<ClipSettings>>
    let window_for_refresh = window.clone();
    let toggle_action = gtk::gio::ActionEntry::builder("pause-recording-toggle")
        .activate(move |_app: &adw::Application, _action, _param| {
            let next_paused = !buffer.borrow().user_paused();
            if next_paused {
                buffer.borrow_mut().pause();
                let _ = cmd_tx.send(crate::clips::ClipCommand::PauseRecording);
            } else {
                buffer.borrow_mut().resume(&cmd_tx);
                // resume() may have already sent StartReplay via maybe_arm;
                // additionally send ResumeRecording to clear the supervisor's
                // restart-attempts limiter so a long-paused state doesn't
                // count against the rolling window.
                let _ = cmd_tx.send(crate::clips::ClipCommand::ResumeRecording);
            }
            // Refresh the clips section UI via the public accessor on ChatMixWindow.
            let state = buffer.borrow().state();
            let paused = buffer.borrow().user_paused();
            let settings = settings_cell_for_action.borrow();
            window_for_refresh.refresh_clips_section(state, paused, &settings);
        })
        .build();
    app.add_action_entries([toggle_action]);
}
```

(Variable names: `buffer`, `cmd_tx`, `settings_cell`, `window` are all in scope at the action cluster region. Verify by reading around line 715-788 before editing.)

- [ ] **Step 3: Build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo build 2>&1 | tail -10'
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "app: register app.pause-recording-toggle GAction"
```

### Task B10: Terminology pass — replace user-visible "buffering" with "recording"

**Files:**
- Modify: `src/clips/settings.rs`
- Modify: `src/clips/indicator.rs`
- Audit-only: `src/clips/notifications.rs`, `src/clips/browser.rs`, `src/window.rs`

- [ ] **Step 1: Apply enumerated `settings.rs` rewrites**

Make these exact line-by-line edits in `src/clips/settings.rs`:

- Line 228: `"Pick the screen recorded by the clip buffer"` → `"Pick the screen recorded by the clipper"`
- Line 251: `"Seconds of gameplay kept in the replay buffer"` → `"Seconds of gameplay kept available for recording"`
- Line 378: `"Start buffering when a known game launches"` → `"Start recording when a known game launches"`
- Line 385: `"Buffer continuously, even outside games"` → `"Record continuously, even outside games"`

Also locate the "Buffer length" UI label (likely a `set_title` call near line 246) and change to "Recording length". Verify with `grep -n "Buffer length" src/clips/settings.rs`.

- [ ] **Step 2: Apply terminology pass on `indicator.rs`**

```bash
grep -nE '"[^"]*[Bb]uffer(ing)?[^"]*"' src/clips/indicator.rs
```

For each user-visible string literal containing "Buffer" or "Buffering" (NOT log lines, NOT identifiers), rewrite the substring to "Record" or "Recording" preserving capitalization. Common ones:
- `"Buffering"` state-label → `"Recording"`
- `"Buffer armed"` → `"Recording armed"`
- `"Buffer saving"` → `"Recording saving"` (if present)

- [ ] **Step 3: Audit-only files**

Run:

```bash
grep -nE '"[^"]*[Bb]uffer(ing)?[^"]*"' src/clips/notifications.rs src/clips/browser.rs src/window.rs
```

Expected: zero user-visible string literals containing "buffer" — log lines or comments are OK. If any user-visible literal does turn up, apply the same rewrite rule.

- [ ] **Step 4: Build**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo build 2>&1 | tail -5'
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/clips/settings.rs src/clips/indicator.rs
git commit -m "clips: terminology pass — user-visible 'buffer' → 'recording'"
```

### Task B11: Implementer B self-review checklist

- [ ] **Step 1: Confirm no audio-module edits**

```bash
git diff main..HEAD --stat -- src/audio/
```

Expected: zero changes.

- [ ] **Step 2: Confirm `app.rs` edits only in the action cluster**

```bash
git diff main..HEAD -- src/app.rs | grep -E "^@@" | head -10
```

Expected: hunk headers in one region only (around lines 715-788).

- [ ] **Step 3: Run the full test suite**

```bash
distrobox enter fedora-dev -- bash -c 'cd /var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui && cargo test 2>&1 | tail -15'
```

Expected: all tests pass, 4 new pause/resume tests pass.

- [ ] **Step 4: Generate the report**

Write a single-message report summarizing:
- Tasks completed
- New tests added
- Files modified
- Any deviation from the spec (with reason)
- Build / test status

Hand this report to **`qa-code-auditor`** (the same QA that receives Implementer A's report).

---

## Phase 2 — QA synthesis

**Owner:** `qa-code-auditor`

Reads:
- Implementer A's report
- Implementer B's report
- Both worktrees (`/var/home/admin/Documents/Code/SteelseriesFlatpak-routing-volume/` and `/var/home/admin/Documents/Code/SteelseriesFlatpak-clips-ui/`)
- The merged-shape preview: a virtual three-way diff against `clipping-system` HEAD

Produces:
- One **comprehensive QA report** synthesizing both bundles' work
- Scores against the same 4-dimension rubric from the spec, 0–100 with sub-scores
- Names any merge-conflict risks (especially in `src/app.rs`)
- Flags any spec-deviation either implementer admitted to (or that QA noticed but they didn't)

Routes report to **`devils-advocate-critic`**.

## Phase 3 — Adversarial review

**Owner:** `devils-advocate-critic`

Reads:
- The QA synthesis report
- Both worktrees
- The spec

Produces:
- **Final adversarial report** for the team lead
- Scores against the rubric, 0–100 with sub-scores
- Top concerns ranked
- What was actually done well

Routes report back to the **team lead** (main session).

## Phase 4 — Score gate

The team lead reads both QA and critic reports. If either score < 90, the team lead identifies the failing dimension and dispatches a targeted re-implementation pass to whichever implementer owns the affected files. Re-review with QA + critic. Repeat until both scores ≥ 90.

Once both ≥ 90:
1. Merge `SteelseriesFlatpak-routing-volume` and `SteelseriesFlatpak-clips-ui` into the `clipping-system` worktree.
2. Resolve any `app.rs` conflicts: both edit-regions are disjoint by design, so a normal `git merge` should produce no conflicts. If it does, take both edits verbatim.
3. Build the merged result in the clipping-system worktree.
4. Hand off to **`project-tester`** for manual verification.

## Phase 5 — Manual verification

**Owner:** `project-tester`

Runs the manual verification recipes from the spec's Testing section:
- Pre-flight tick-latency baseline (`time pactl list sources`)
- Issue 1: Tidal-style stream move persists across app restart
- Issue 2: preferred mic auto-attaches on hotplug, including USB-port-change for the bus_path tier
- Issue 3: SteelSeries_Mic set to 70% externally survives quit + relaunch
- Issue 4: Clips section is row 1 full-width; Save / Pause buttons work; terminology pass complete

Reports back to the team lead with pass/fail per recipe.

---

## Coordination point: `src/app.rs`

Five edit regions in `src/app.rs`, all mutually disjoint:

- **Implementer A region 1:** state-sync watcher block (approximately `app.rs:1391–1395`).
- **Implementer A region 2:** `connect_shutdown` handler — volume capture (approximately `app.rs:1462–1478`).
- **Implementer A region 3:** `init_pipeline` — apply-volumes loop after `VirtualSinks::create()` (approximately `app.rs:1502–1517`).
- **Implementer B region 1:** action-registration cluster — new `app.pause-recording-toggle` (approximately `app.rs:715–788`).
- **Implementer B region 2:** `run_global_shortcuts` and `rebind_shortcuts` call sites — pass `Some(settings_cell.clone())` as a new last parameter (approximately `app.rs:1017` and `app.rs:1107`).

After both bundles land their changes, a `git merge` of the two worktrees produces no conflicts because no two regions overlap or are adjacent. If a conflict marker appears: take both edits verbatim. Verifier check: `git diff --stat main..merged -- src/app.rs` should show one consolidated diff.

## Teammate involvement (this plan's own)

- **`research-bot`** — Skipped. The spec already incorporated context7 / web research for ashpd, GSR, and PipeWire. No new research expected.
- **`devils-advocate-critic`** — Used in Phase 3 + the spec-review loop already passed it at round 4.
- **`project-tester`** — Used in Phase 0 (discriminator) and Phase 5 (manual verification).
- **`qa-code-auditor`** — Used in Phase 2 (synthesis).
- **`security-audit-sentinel`** — Skipped. No auth / secrets / public endpoints changed.

## Self-review checklist (run after writing the plan, fix inline)

1. **Spec coverage:** Issue 1 (Tasks A5, A7), Issue 2 (Tasks A2, A3, A4), Issue 3 (Tasks A1, A8, A9, A10, A11), Issue 4 (Tasks B1-B10). ✓
2. **Placeholder scan:** No "TBD", no "implement later", every code step has runnable code. ✓
3. **Type consistency:** `MicPreference` defined in A2, used in A3/A4. `ClipsSectionWidgets` defined in B6, used in B8/B9. `BufferState::Paused` added in B1, used in B6/B8. ✓
4. **Cross-bundle invariants:** `src/app.rs` four regions disjoint, named with line anchors, mutually exclusive between A and B. ✓
