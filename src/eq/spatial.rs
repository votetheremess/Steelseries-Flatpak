//! HeSuVi-HRIR-based spatial audio support.
//!
//! Each `SpatialProfile` maps to one bundled 14-channel WAV impulse response
//! (from HeSuVi). On first use we extract the chosen IR to
//! `~/.cache/arctis-chatmix/hrtf/<key>.wav` so PipeWire's `convolver` filter
//! can load it by path. Unused profiles never get extracted.

use std::fs;
use std::path::PathBuf;

use super::model::{SpatialProfile, SpatialState, WET_MAX, WET_MIN};

// ---------------------------------------------------------------------------
// Bundled HeSuVi IR bytes
// ---------------------------------------------------------------------------

const IR_GSX: &[u8] = include_bytes!("../../data/hrtf/gsx.wav");
const IR_DH2: &[u8] = include_bytes!("../../data/hrtf/dh2.wav");
const IR_DTS: &[u8] = include_bytes!("../../data/hrtf/dts-hpx.wav");

fn embedded_ir_bytes(profile: SpatialProfile) -> &'static [u8] {
    match profile {
        SpatialProfile::Gsx => IR_GSX,
        SpatialProfile::Dh2 => IR_DH2,
        SpatialProfile::DtsHpx => IR_DTS,
    }
}

fn ir_filename(profile: SpatialProfile) -> &'static str {
    match profile {
        SpatialProfile::Gsx => "gsx.wav",
        SpatialProfile::Dh2 => "dh2.wav",
        SpatialProfile::DtsHpx => "dts-hpx.wav",
    }
}

fn hrtf_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("arctis-chatmix/hrtf"))
}

/// Extract the selected HeSuVi IR to the user cache dir and return its path.
/// Lazy and idempotent: only rewrites when the on-disk size differs (cheap
/// check — a full content compare would re-read the bundle every launch).
pub fn ensure_ir_file(profile: SpatialProfile) -> Option<PathBuf> {
    let dir = hrtf_dir()?;
    if let Err(e) = fs::create_dir_all(&dir) {
        log::warn!("Failed to create HRTF cache dir {}: {e}", dir.display());
        return None;
    }
    let path = dir.join(ir_filename(profile));
    let bytes = embedded_ir_bytes(profile);
    let needs_write = match fs::metadata(&path) {
        Ok(md) => md.len() as usize != bytes.len(),
        Err(_) => true,
    };
    if needs_write {
        if let Err(e) = fs::write(&path, bytes) {
            log::warn!("Failed to extract HRIR {}: {e}", path.display());
            return None;
        }
        log::info!(
            "Extracted HRIR to {} ({} bytes, profile {})",
            path.display(),
            bytes.len(),
            profile.key(),
        );
    }
    Some(path)
}

/// Quick availability check — true if every profile's IR can be extracted.
/// Used by the UI to grey out the enable toggle when extraction is broken.
pub fn hrtf_available() -> bool {
    SpatialProfile::ALL
        .iter()
        .all(|p| ensure_ir_file(*p).is_some())
}

// ---------------------------------------------------------------------------
// Serialization for eq_state.txt (4th tab-separated field)
// ---------------------------------------------------------------------------

/// Serialize as `enabled=1;profile=gsx;wet=0.700`.
pub fn serialize(state: &SpatialState) -> String {
    format!(
        "enabled={};profile={};wet={:.3}",
        if state.enabled { 1 } else { 0 },
        state.profile.key(),
        state.wet_mix,
    )
}

/// Parse the kv-blob. Legacy formats (`enabled=1`, profile-less,
/// `pi=…;d=…;fa=…` SOFA-era noise) still parse cleanly — unknown keys and
/// unknown profile values are silently ignored and the affected field falls
/// back to the type's default.
pub fn parse(blob: &str) -> SpatialState {
    let mut s = SpatialState::default();
    for kv in blob.split(';') {
        let Some((k, v)) = kv.split_once('=') else { continue };
        match k.trim() {
            "enabled" => s.enabled = v.trim() == "1",
            "profile" => {
                if let Some(p) = SpatialProfile::from_key(v.trim()) {
                    s.profile = p;
                }
            }
            "wet" => {
                if let Ok(n) = v.trim().parse::<f64>() {
                    s.wet_mix = n.clamp(WET_MIN, WET_MAX);
                }
            }
            _ => {}
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::eq::model::WET_DEFAULT;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn default_state_is_disabled_gsx_default_wet() {
        let s = SpatialState::default();
        assert!(!s.enabled);
        assert_eq!(s.profile, SpatialProfile::Gsx);
        assert!(near(s.wet_mix, WET_DEFAULT));
    }

    #[test]
    fn serialize_parse_round_trip() {
        for &p in SpatialProfile::ALL {
            let s = SpatialState { enabled: true, profile: p, wet_mix: 0.42 };
            let blob = serialize(&s);
            let parsed = parse(&blob);
            assert!(parsed.enabled);
            assert_eq!(parsed.profile, p, "profile round-trip failed for {}", p.key());
            assert!(near(parsed.wet_mix, 0.42), "wet_mix round-trip failed");
        }
    }

    #[test]
    fn parse_unknown_profile_falls_back_to_gsx() {
        let s = parse("enabled=1;profile=unknown_xyz;wet=0.5");
        assert!(s.enabled);
        assert_eq!(s.profile, SpatialProfile::Gsx);
        assert!(near(s.wet_mix, 0.5));
    }

    #[test]
    fn parse_legacy_formats_default_cleanly() {
        // Pre-profile blob: just `enabled=1`.
        let s = parse("enabled=1");
        assert!(s.enabled);
        assert_eq!(s.profile, SpatialProfile::Gsx);
        assert!(near(s.wet_mix, WET_DEFAULT));

        // Pre-wet blob: profile but no wet field.
        let s = parse("enabled=1;profile=dts");
        assert!(s.enabled);
        assert_eq!(s.profile, SpatialProfile::DtsHpx);
        assert!(near(s.wet_mix, WET_DEFAULT));

        // Old SOFA-era noise.
        let s = parse("enabled=1;pi=0.20;d=1.50;fa=45.00");
        assert!(s.enabled);
        assert_eq!(s.profile, SpatialProfile::Gsx);
        assert!(near(s.wet_mix, WET_DEFAULT));
    }

    #[test]
    fn parse_clamps_wet_out_of_range() {
        let s = parse("wet=2.5");
        assert!(near(s.wet_mix, 1.0));
        let s = parse("wet=-1.0");
        assert!(near(s.wet_mix, 0.0));
    }

    #[test]
    fn parse_tolerates_garbage() {
        let s = parse("garbage");
        assert!(!s.enabled);
        let s = parse("");
        assert!(!s.enabled);
    }

    #[test]
    fn profile_keys_are_unique_and_stable() {
        let keys: Vec<_> = SpatialProfile::ALL.iter().map(|p| p.key()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate profile key");
    }
}
