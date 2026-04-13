use std::fs;
use std::path::PathBuf;

use super::model::{Band, FilterType, SinkEq, EqTarget, NUM_BANDS};

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("arctis-chatmix"))
}

fn presets_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("eq_presets"))
}

fn eq_state_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("eq_state.txt"))
}

// ---------------------------------------------------------------------------
// Built-in presets
// ---------------------------------------------------------------------------

struct PresetDef {
    name: &'static str,
    /// (frequency, gain_db, q, filter_type) for each of the 10 bands
    bands: [(f64, f64, f64, FilterType); NUM_BANDS],
}

const P: FilterType = FilterType::Peaking;
const LS: FilterType = FilterType::LowShelf;
const HS: FilterType = FilterType::HighShelf;

static BUILT_IN: &[PresetDef] = &[
    PresetDef {
        name: "Flat",
        bands: [
            (31.0, 0.0, 1.0, P), (62.0, 0.0, 1.0, P), (125.0, 0.0, 1.0, P),
            (250.0, 0.0, 1.0, P), (500.0, 0.0, 1.0, P), (1000.0, 0.0, 1.0, P),
            (2000.0, 0.0, 1.0, P), (4000.0, 0.0, 1.0, P), (8000.0, 0.0, 1.0, P),
            (16000.0, 0.0, 1.0, P),
        ],
    },
    PresetDef {
        name: "Bass Boost",
        bands: [
            (31.0, 6.0, 0.7, LS), (62.0, 5.0, 1.0, P), (125.0, 3.5, 1.0, P),
            (250.0, 1.0, 1.0, P), (500.0, 0.0, 1.0, P), (1000.0, 0.0, 1.0, P),
            (2000.0, 0.0, 1.0, P), (4000.0, 0.0, 1.0, P), (8000.0, 0.0, 1.0, P),
            (16000.0, 0.0, 1.0, P),
        ],
    },
    PresetDef {
        name: "Treble Boost",
        bands: [
            (31.0, 0.0, 1.0, P), (62.0, 0.0, 1.0, P), (125.0, 0.0, 1.0, P),
            (250.0, 0.0, 1.0, P), (500.0, 0.0, 1.0, P), (1000.0, 0.0, 1.0, P),
            (2000.0, 1.0, 1.0, P), (4000.0, 3.5, 1.0, P), (8000.0, 5.0, 1.0, P),
            (16000.0, 6.0, 0.7, HS),
        ],
    },
    PresetDef {
        name: "V-Shape",
        bands: [
            (31.0, 5.0, 0.7, LS), (62.0, 4.0, 1.0, P), (125.0, 2.0, 1.0, P),
            (250.0, 0.0, 1.0, P), (500.0, -2.0, 1.0, P), (1000.0, -2.0, 1.0, P),
            (2000.0, 0.0, 1.0, P), (4000.0, 2.0, 1.0, P), (8000.0, 4.0, 1.0, P),
            (16000.0, 5.0, 0.7, HS),
        ],
    },
    PresetDef {
        name: "FPS Competitive",
        bands: [
            (31.0, -3.0, 1.0, P), (62.0, -2.0, 1.0, P), (125.0, -1.0, 1.0, P),
            (250.0, 0.0, 1.0, P), (500.0, 0.0, 1.0, P), (1000.0, 1.0, 1.0, P),
            (2000.0, 3.0, 1.2, P), (4000.0, 4.0, 1.2, P), (8000.0, 2.0, 1.0, P),
            (16000.0, 0.0, 1.0, P),
        ],
    },
    PresetDef {
        name: "Vocal Clarity",
        bands: [
            (31.0, -1.0, 1.0, P), (62.0, 0.0, 1.0, P), (125.0, 0.0, 1.0, P),
            (250.0, 1.0, 1.0, P), (500.0, 2.0, 1.0, P), (1000.0, 3.0, 1.0, P),
            (2000.0, 4.0, 1.2, P), (4000.0, 3.0, 1.2, P), (8000.0, 1.0, 1.0, P),
            (16000.0, 0.0, 1.0, P),
        ],
    },
    PresetDef {
        name: "Warm",
        bands: [
            (31.0, 4.0, 0.7, LS), (62.0, 3.0, 1.0, P), (125.0, 2.0, 1.0, P),
            (250.0, 1.0, 1.0, P), (500.0, 0.0, 1.0, P), (1000.0, 0.0, 1.0, P),
            (2000.0, -0.5, 1.0, P), (4000.0, -1.0, 1.0, P), (8000.0, -1.5, 1.0, P),
            (16000.0, -2.0, 0.7, HS),
        ],
    },
];

fn preset_def_to_bands(def: &PresetDef) -> [Band; NUM_BANDS] {
    std::array::from_fn(|i| {
        let (freq, gain, q, ft) = def.bands[i];
        Band {
            frequency: freq,
            gain_db: gain,
            q,
            filter_type: ft,
            enabled: true,
        }
    })
}

pub fn built_in_names() -> Vec<&'static str> {
    BUILT_IN.iter().map(|p| p.name).collect()
}

pub fn load_built_in(name: &str) -> Option<[Band; NUM_BANDS]> {
    BUILT_IN.iter().find(|p| p.name == name).map(preset_def_to_bands)
}

// ---------------------------------------------------------------------------
// Custom preset persistence
// ---------------------------------------------------------------------------

fn serialize_bands(bands: &[Band; NUM_BANDS]) -> String {
    let mut out = String::new();
    for band in bands {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            band.frequency,
            band.gain_db,
            band.q,
            band.filter_type.to_str(),
            band.enabled,
        ));
    }
    out
}

fn parse_bands(text: &str) -> Option<[Band; NUM_BANDS]> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    if lines.len() != NUM_BANDS {
        return None;
    }
    let mut bands = super::model::default_bands();
    for (i, line) in lines.iter().enumerate() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 5 {
            return None;
        }
        bands[i].frequency = parts[0].parse().ok()?;
        bands[i].gain_db = parts[1].parse().ok()?;
        bands[i].q = parts[2].parse().ok()?;
        bands[i].filter_type = FilterType::from_str(parts[3])?;
        bands[i].enabled = parts[4].parse().ok()?;
    }
    Some(bands)
}

pub fn list_custom_presets() -> Vec<String> {
    let Some(dir) = presets_dir() else { return vec![] };
    let Ok(entries) = fs::read_dir(&dir) else { return vec![] };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "txt") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names
}

pub fn save_custom_preset(name: &str, bands: &[Band; NUM_BANDS]) {
    let Some(dir) = presets_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        log::error!("Failed to create EQ presets dir: {dir:?}");
        return;
    }
    let path = dir.join(format!("{name}.txt"));
    let content = serialize_bands(bands);
    if let Err(e) = fs::write(&path, &content) {
        log::error!("Failed to save EQ preset {name}: {e}");
    }
}

pub fn load_custom_preset(name: &str) -> Option<[Band; NUM_BANDS]> {
    let path = presets_dir()?.join(format!("{name}.txt"));
    let text = fs::read_to_string(&path).ok()?;
    parse_bands(&text)
}

pub fn delete_custom_preset(name: &str) {
    if let Some(path) = presets_dir().map(|d| d.join(format!("{name}.txt"))) {
        let _ = fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// Per-sink EQ state persistence
// ---------------------------------------------------------------------------

/// Serialize a SinkEq's bands into a compact inline format for the state file.
fn inline_bands(bands: &[Band; NUM_BANDS]) -> String {
    bands
        .iter()
        .map(|b| {
            format!(
                "{},{},{},{},{}",
                b.frequency, b.gain_db, b.q, b.filter_type.to_str(), b.enabled
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn parse_inline_bands(s: &str) -> Option<[Band; NUM_BANDS]> {
    let parts: Vec<&str> = s.split('|').collect();
    if parts.len() != NUM_BANDS {
        return None;
    }
    let mut bands = super::model::default_bands();
    for (i, part) in parts.iter().enumerate() {
        let fields: Vec<&str> = part.split(',').collect();
        if fields.len() < 5 {
            return None;
        }
        bands[i].frequency = fields[0].parse().ok()?;
        bands[i].gain_db = fields[1].parse().ok()?;
        bands[i].q = fields[2].parse().ok()?;
        bands[i].filter_type = FilterType::from_str(fields[3])?;
        bands[i].enabled = fields[4].parse().ok()?;
    }
    Some(bands)
}

pub fn save_eq_state(sinks: &std::collections::HashMap<EqTarget, SinkEq>) {
    let Some(path) = eq_state_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut content = String::new();
    for target in EqTarget::ALL {
        if let Some(sink_eq) = sinks.get(target) {
            let preset_or_custom = sink_eq
                .preset_name
                .as_deref()
                .unwrap_or("custom");
            content.push_str(&format!(
                "{}\t{}\t{}\n",
                target.sink_name(),
                preset_or_custom,
                inline_bands(&sink_eq.bands),
            ));
        }
    }
    if let Err(e) = fs::write(&path, &content) {
        log::error!("Failed to save EQ state: {e}");
    }
}

pub fn load_eq_state() -> std::collections::HashMap<EqTarget, SinkEq> {
    let mut result = std::collections::HashMap::new();
    let Some(path) = eq_state_path() else { return result };
    let Ok(text) = fs::read_to_string(&path) else { return result };

    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let sink_name = parts[0];
        let preset_name_str = parts[1];
        let band_data = parts[2];

        let target = EqTarget::ALL
            .iter()
            .find(|t| t.sink_name() == sink_name)
            .copied();
        let Some(target) = target else { continue };

        let bands = match parse_inline_bands(band_data) {
            Some(b) => b,
            None => super::model::default_bands(),
        };

        let preset_name = if preset_name_str == "custom" {
            None
        } else {
            Some(preset_name_str.to_string())
        };

        result.insert(target, SinkEq { bands, preset_name });
    }

    result
}
