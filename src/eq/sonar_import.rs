//! Import a SteelSeries Sonar preset from a share URL or raw CDN URL.
//!
//! Sonar's "Share" button uploads the preset JSON to
//! `https://community-configs.steelseriescdn.com/v1/<uuid>` (anonymous GET),
//! wrapped in a deeplink of the form
//! `https://www.steelseries.com/deeplink/gg/sonar/config/v1/import?url=<base64>`.
//!
//! The JSON's `parametricEQ.filter1..10` structure maps 1:1 to our `Band`
//! model — see `parse_filter_type` for the type-name mapping.

use std::time::Duration;

use base64::Engine;
use serde::Deserialize;

use super::model::{Band, FilterType, NUM_BANDS, FREQ_MIN, FREQ_MAX, GAIN_MIN, GAIN_MAX, Q_MIN, Q_MAX};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub struct ImportedPreset {
    pub name: String,
    pub bands: [Band; NUM_BANDS],
}

/// Fetch and parse a Sonar preset from any of:
/// - a deeplink: `https://www.steelseries.com/deeplink/gg/sonar/config/v1/import?url=<base64>`
/// - a raw CDN URL: `https://community-configs.steelseriescdn.com/v1/<uuid>`
pub fn import_from_url(url: &str) -> Result<ImportedPreset, String> {
    let cdn_url = resolve_cdn_url(url)?;
    let json = http_get(&cdn_url)?;
    parse_sonar_json(&json)
}

/// Parse a Sonar preset JSON (as stored in the `configs.data` DB column or
/// returned by the CDN) into our Band model. Exposed for tests and for
/// loading files produced by `tools/dump-sonar-presets.ps1`.
pub fn parse_sonar_json(json: &str) -> Result<ImportedPreset, String> {
    let doc: SonarDoc = serde_json::from_str(json)
        .map_err(|e| format!("Sonar JSON parse failed: {e}"))?;
    let name = doc.name.unwrap_or_else(|| "Imported".to_string());
    let Some(eq) = doc.parametric_eq.or(doc.data.and_then(|d| d.parametric_eq)) else {
        return Err("Sonar JSON has no parametricEQ block".into());
    };
    let bands = bands_from_sonar(&eq)?;
    Ok(ImportedPreset { name, bands })
}

// ---------------------------------------------------------------------------
// URL resolution
// ---------------------------------------------------------------------------

fn resolve_cdn_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("Empty URL".into());
    }

    if trimmed.contains("community-configs.steelseriescdn.com/") {
        return Ok(trimmed.to_string());
    }

    if let Some(encoded) = extract_url_query_param(trimmed) {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(encoded.as_bytes()))
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(encoded.as_bytes()))
            .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded.as_bytes()))
            .map_err(|e| format!("Failed to base64-decode share URL payload: {e}"))?;
        let decoded_str = String::from_utf8(decoded)
            .map_err(|e| format!("Decoded share URL is not valid UTF-8: {e}"))?;
        if !decoded_str.contains("community-configs.steelseriescdn.com") {
            return Err(format!(
                "Decoded share URL does not point at the Sonar config CDN: {decoded_str}"
            ));
        }
        return Ok(decoded_str);
    }

    Err("URL is neither a Sonar share deeplink nor a community-configs CDN URL".into())
}

/// Extract the value of the `url=` query parameter without pulling in a URL crate.
fn extract_url_query_param(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some(rest) = pair.strip_prefix("url=") {
            return Some(rest.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn http_get(url: &str) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .build();
    let response = agent
        .get(url)
        .call()
        .map_err(|e| format!("HTTP GET {url} failed: {e}"))?;
    response
        .into_string()
        .map_err(|e| format!("Failed to read HTTP body: {e}"))
}

// ---------------------------------------------------------------------------
// JSON model
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SonarDoc {
    name: Option<String>,
    /// The CDN sometimes serves the config object directly, sometimes wrapped
    /// in `{ "data": { ... } }` (the shape stored in the DB's `configs.data`
    /// column). Accept both.
    #[serde(rename = "parametricEQ")]
    parametric_eq: Option<SonarParametricEq>,
    data: Option<SonarDataWrapper>,
}

#[derive(Deserialize)]
struct SonarDataWrapper {
    #[serde(rename = "parametricEQ")]
    parametric_eq: Option<SonarParametricEq>,
}

#[derive(Deserialize)]
struct SonarParametricEq {
    #[serde(default)]
    filter1: Option<SonarFilter>,
    #[serde(default)]
    filter2: Option<SonarFilter>,
    #[serde(default)]
    filter3: Option<SonarFilter>,
    #[serde(default)]
    filter4: Option<SonarFilter>,
    #[serde(default)]
    filter5: Option<SonarFilter>,
    #[serde(default)]
    filter6: Option<SonarFilter>,
    #[serde(default)]
    filter7: Option<SonarFilter>,
    #[serde(default)]
    filter8: Option<SonarFilter>,
    #[serde(default)]
    filter9: Option<SonarFilter>,
    #[serde(default)]
    filter10: Option<SonarFilter>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SonarFilter {
    enabled: Option<bool>,
    frequency: Option<f64>,
    gain: Option<f64>,
    q_factor: Option<f64>,
    #[serde(rename = "type")]
    filter_type: Option<String>,
}

fn bands_from_sonar(eq: &SonarParametricEq) -> Result<[Band; NUM_BANDS], String> {
    let filters = [
        &eq.filter1, &eq.filter2, &eq.filter3, &eq.filter4, &eq.filter5,
        &eq.filter6, &eq.filter7, &eq.filter8, &eq.filter9, &eq.filter10,
    ];
    let mut bands = super::model::default_bands();
    for (i, slot) in filters.iter().enumerate() {
        if let Some(f) = slot {
            bands[i] = band_from_sonar(f, bands[i].frequency)?;
        }
    }
    Ok(bands)
}

fn band_from_sonar(f: &SonarFilter, default_freq: f64) -> Result<Band, String> {
    let filter_type = match f.filter_type.as_deref() {
        Some(s) => parse_filter_type(s)?,
        None => FilterType::Peaking,
    };
    let frequency = f.frequency.unwrap_or(default_freq).clamp(FREQ_MIN, FREQ_MAX);
    let gain_db = f.gain.unwrap_or(0.0).clamp(GAIN_MIN, GAIN_MAX);
    let q = f.q_factor.unwrap_or(1.0).clamp(Q_MIN, Q_MAX);
    let enabled = f.enabled.unwrap_or(true);
    Ok(Band { frequency, gain_db, q, filter_type, enabled })
}

fn parse_filter_type(s: &str) -> Result<FilterType, String> {
    match s {
        "peakingEQ" | "peaking" => Ok(FilterType::Peaking),
        "lowShelving" | "lowShelf" | "low_shelf" => Ok(FilterType::LowShelf),
        "highShelving" | "highShelf" | "high_shelf" => Ok(FilterType::HighShelf),
        "lowPass" | "low_pass" => Ok(FilterType::LowPass),
        "highPass" | "high_pass" => Ok(FilterType::HighPass),
        "notchFilter" | "notch" => Ok(FilterType::Notch),
        other => Err(format!("Unknown Sonar filter type: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_JSON: &str = r#"{
        "name": "FPS Footsteps",
        "parametricEQ": {
            "enabled": true,
            "filter1": {"enabled": true, "frequency": 31.0, "gain": -3.0, "qFactor": 1.0, "type": "peakingEQ"},
            "filter2": {"enabled": true, "frequency": 62.0, "gain": -2.0, "qFactor": 1.0, "type": "peakingEQ"},
            "filter3": {"enabled": true, "frequency": 125.0, "gain": -1.0, "qFactor": 1.0, "type": "peakingEQ"},
            "filter4": {"enabled": true, "frequency": 250.0, "gain": 0.0, "qFactor": 1.0, "type": "peakingEQ"},
            "filter5": {"enabled": true, "frequency": 500.0, "gain": 0.0, "qFactor": 1.0, "type": "peakingEQ"},
            "filter6": {"enabled": true, "frequency": 1000.0, "gain": 1.0, "qFactor": 1.0, "type": "peakingEQ"},
            "filter7": {"enabled": true, "frequency": 2000.0, "gain": 3.0, "qFactor": 1.2, "type": "peakingEQ"},
            "filter8": {"enabled": true, "frequency": 4000.0, "gain": 4.0, "qFactor": 1.2, "type": "peakingEQ"},
            "filter9": {"enabled": true, "frequency": 8000.0, "gain": 2.0, "qFactor": 1.0, "type": "highShelving"},
            "filter10": {"enabled": false, "frequency": 16000.0, "gain": 0.0, "qFactor": 1.0, "type": "notchFilter"}
        }
    }"#;

    #[test]
    fn parses_sample_sonar_json() {
        let preset = parse_sonar_json(SAMPLE_JSON).unwrap();
        assert_eq!(preset.name, "FPS Footsteps");
        assert_eq!(preset.bands[0].frequency, 31.0);
        assert_eq!(preset.bands[0].gain_db, -3.0);
        assert_eq!(preset.bands[6].filter_type, FilterType::Peaking);
        assert_eq!(preset.bands[8].filter_type, FilterType::HighShelf);
        assert_eq!(preset.bands[9].filter_type, FilterType::Notch);
        assert!(!preset.bands[9].enabled);
    }

    #[test]
    fn accepts_db_wrapped_shape() {
        let wrapped = r#"{
            "name": "Wrapped",
            "data": {
                "parametricEQ": {
                    "filter1": {"enabled": true, "frequency": 100.0, "gain": 3.0, "qFactor": 1.0, "type": "peakingEQ"}
                }
            }
        }"#;
        let preset = parse_sonar_json(wrapped).unwrap();
        assert_eq!(preset.name, "Wrapped");
        assert_eq!(preset.bands[0].frequency, 100.0);
        assert_eq!(preset.bands[0].gain_db, 3.0);
    }

    #[test]
    fn clamps_out_of_bounds_values() {
        let extreme = r#"{
            "name": "Extreme",
            "parametricEQ": {
                "filter1": {"enabled": true, "frequency": 999999.0, "gain": 999.0, "qFactor": 999.0, "type": "peakingEQ"}
            }
        }"#;
        let preset = parse_sonar_json(extreme).unwrap();
        assert_eq!(preset.bands[0].frequency, FREQ_MAX);
        assert_eq!(preset.bands[0].gain_db, GAIN_MAX);
        assert_eq!(preset.bands[0].q, Q_MAX);
    }

    #[test]
    fn resolves_raw_cdn_url() {
        let url = "https://community-configs.steelseriescdn.com/v1/deadbeef-1234";
        assert_eq!(resolve_cdn_url(url).unwrap(), url);
    }

    #[test]
    fn decodes_deeplink_share_url() {
        let cdn = "https://community-configs.steelseriescdn.com/v1/abc-123";
        let b64 = base64::engine::general_purpose::STANDARD.encode(cdn);
        let deeplink = format!(
            "https://www.steelseries.com/deeplink/gg/sonar/config/v1/import?url={b64}"
        );
        assert_eq!(resolve_cdn_url(&deeplink).unwrap(), cdn);
    }

    #[test]
    fn rejects_unrelated_url() {
        assert!(resolve_cdn_url("https://example.com/preset").is_err());
    }

    #[test]
    fn rejects_deeplink_pointing_elsewhere() {
        let evil = "https://example.com/evil";
        let b64 = base64::engine::general_purpose::STANDARD.encode(evil);
        let deeplink = format!(
            "https://www.steelseries.com/deeplink/gg/sonar/config/v1/import?url={b64}"
        );
        assert!(resolve_cdn_url(&deeplink).is_err());
    }

    #[test]
    fn unknown_filter_type_errors() {
        let bad = r#"{
            "name": "Bad",
            "parametricEQ": {
                "filter1": {"enabled": true, "frequency": 100.0, "gain": 0.0, "qFactor": 1.0, "type": "somethingNew"}
            }
        }"#;
        assert!(parse_sonar_json(bad).is_err());
    }
}
