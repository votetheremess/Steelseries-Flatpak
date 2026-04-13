//! Audio EQ Cookbook biquad math for computing frequency response curves.
//! Pure math — no audio processing, no GTK dependencies.
//! Reference: Robert Bristow-Johnson's "Audio EQ Cookbook"

use std::f64::consts::PI;

use super::model::{Band, FilterType};

const SAMPLE_RATE: f64 = 48000.0;

struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a0: f64,
    a1: f64,
    a2: f64,
}

fn peaking_coeffs(freq: f64, gain_db: f64, q: f64) -> Coeffs {
    let a = 10.0_f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * freq / SAMPLE_RATE;
    let alpha = w0.sin() / (2.0 * q);
    Coeffs {
        b0: 1.0 + alpha * a,
        b1: -2.0 * w0.cos(),
        b2: 1.0 - alpha * a,
        a0: 1.0 + alpha / a,
        a1: -2.0 * w0.cos(),
        a2: 1.0 - alpha / a,
    }
}

fn low_shelf_coeffs(freq: f64, gain_db: f64, q: f64) -> Coeffs {
    let a = 10.0_f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * freq / SAMPLE_RATE;
    let alpha = w0.sin() / (2.0 * q);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
    Coeffs {
        b0: a * ((a + 1.0) - (a - 1.0) * w0.cos() + two_sqrt_a_alpha),
        b1: 2.0 * a * ((a - 1.0) - (a + 1.0) * w0.cos()),
        b2: a * ((a + 1.0) - (a - 1.0) * w0.cos() - two_sqrt_a_alpha),
        a0: (a + 1.0) + (a - 1.0) * w0.cos() + two_sqrt_a_alpha,
        a1: -2.0 * ((a - 1.0) + (a + 1.0) * w0.cos()),
        a2: (a + 1.0) + (a - 1.0) * w0.cos() - two_sqrt_a_alpha,
    }
}

fn high_shelf_coeffs(freq: f64, gain_db: f64, q: f64) -> Coeffs {
    let a = 10.0_f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * freq / SAMPLE_RATE;
    let alpha = w0.sin() / (2.0 * q);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
    Coeffs {
        b0: a * ((a + 1.0) + (a - 1.0) * w0.cos() + two_sqrt_a_alpha),
        b1: -2.0 * a * ((a - 1.0) + (a + 1.0) * w0.cos()),
        b2: a * ((a + 1.0) + (a - 1.0) * w0.cos() - two_sqrt_a_alpha),
        a0: (a + 1.0) - (a - 1.0) * w0.cos() + two_sqrt_a_alpha,
        a1: 2.0 * ((a - 1.0) - (a + 1.0) * w0.cos()),
        a2: (a + 1.0) - (a - 1.0) * w0.cos() - two_sqrt_a_alpha,
    }
}

fn low_pass_coeffs(freq: f64, q: f64) -> Coeffs {
    let w0 = 2.0 * PI * freq / SAMPLE_RATE;
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();
    Coeffs {
        b0: (1.0 - cos_w0) / 2.0,
        b1: 1.0 - cos_w0,
        b2: (1.0 - cos_w0) / 2.0,
        a0: 1.0 + alpha,
        a1: -2.0 * cos_w0,
        a2: 1.0 - alpha,
    }
}

fn high_pass_coeffs(freq: f64, q: f64) -> Coeffs {
    let w0 = 2.0 * PI * freq / SAMPLE_RATE;
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();
    Coeffs {
        b0: (1.0 + cos_w0) / 2.0,
        b1: -(1.0 + cos_w0),
        b2: (1.0 + cos_w0) / 2.0,
        a0: 1.0 + alpha,
        a1: -2.0 * cos_w0,
        a2: 1.0 - alpha,
    }
}

fn band_coeffs(band: &Band) -> Coeffs {
    match band.filter_type {
        FilterType::Peaking => peaking_coeffs(band.frequency, band.gain_db, band.q),
        FilterType::LowShelf => low_shelf_coeffs(band.frequency, band.gain_db, band.q),
        FilterType::HighShelf => high_shelf_coeffs(band.frequency, band.gain_db, band.q),
        FilterType::LowPass => low_pass_coeffs(band.frequency, band.q),
        FilterType::HighPass => high_pass_coeffs(band.frequency, band.q),
    }
}

/// Compute the magnitude response in dB of a single band at the given frequency.
pub fn magnitude_db(band: &Band, freq: f64) -> f64 {
    if !band.enabled {
        return 0.0;
    }
    let c = band_coeffs(band);
    let w = 2.0 * PI * freq / SAMPLE_RATE;
    let cos_w = w.cos();
    let cos_2w = (2.0 * w).cos();

    // |H(e^jw)|^2 using the polynomial form to avoid complex arithmetic
    let num = c.b0 * c.b0 + c.b1 * c.b1 + c.b2 * c.b2
        + 2.0 * (c.b0 * c.b1 + c.b1 * c.b2) * cos_w
        + 2.0 * c.b0 * c.b2 * cos_2w;
    let den = c.a0 * c.a0 + c.a1 * c.a1 + c.a2 * c.a2
        + 2.0 * (c.a0 * c.a1 + c.a1 * c.a2) * cos_w
        + 2.0 * c.a0 * c.a2 * cos_2w;

    if den <= 0.0 {
        return 0.0;
    }

    10.0 * (num / den).log10()
}

/// Compute the combined magnitude response of all bands at the given frequency.
/// Since biquads are cascaded, their dB responses sum.
pub fn combined_magnitude_db(bands: &[Band], freq: f64) -> f64 {
    bands.iter().map(|b| magnitude_db(b, freq)).sum()
}

/// Generate logarithmically spaced frequencies for curve plotting.
pub fn log_frequencies(n: usize) -> Vec<f64> {
    let log_min = 20.0_f64.log10();
    let log_max = 20000.0_f64.log10();
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            10.0_f64.powf(log_min + t * (log_max - log_min))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eq::model::{Band, FilterType};

    fn approx_eq(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() < tolerance
    }

    #[test]
    fn peaking_at_center_returns_gain() {
        let band = Band {
            frequency: 1000.0,
            gain_db: 6.0,
            q: 1.0,
            filter_type: FilterType::Peaking,
            enabled: true,
        };
        let db = magnitude_db(&band, 1000.0);
        assert!(approx_eq(db, 6.0, 0.1), "expected ~6 dB at center, got {db}");
    }

    #[test]
    fn peaking_zero_gain_is_flat() {
        let band = Band {
            frequency: 1000.0,
            gain_db: 0.0,
            q: 1.0,
            filter_type: FilterType::Peaking,
            enabled: true,
        };
        let db = magnitude_db(&band, 500.0);
        assert!(approx_eq(db, 0.0, 0.01), "expected ~0 dB, got {db}");
    }

    #[test]
    fn disabled_band_is_flat() {
        let band = Band {
            frequency: 1000.0,
            gain_db: 12.0,
            q: 1.0,
            filter_type: FilterType::Peaking,
            enabled: false,
        };
        let db = magnitude_db(&band, 1000.0);
        assert!(approx_eq(db, 0.0, 0.001), "disabled band should be 0 dB, got {db}");
    }

    #[test]
    fn low_shelf_boosts_below_freq() {
        let band = Band {
            frequency: 1000.0,
            gain_db: 6.0,
            q: 0.7,
            filter_type: FilterType::LowShelf,
            enabled: true,
        };
        let db_low = magnitude_db(&band, 100.0);
        let db_high = magnitude_db(&band, 10000.0);
        assert!(db_low > 3.0, "low shelf should boost lows, got {db_low} dB at 100 Hz");
        assert!(db_high.abs() < 1.0, "low shelf should be flat above, got {db_high} dB at 10kHz");
    }

    #[test]
    fn high_shelf_boosts_above_freq() {
        let band = Band {
            frequency: 1000.0,
            gain_db: 6.0,
            q: 0.7,
            filter_type: FilterType::HighShelf,
            enabled: true,
        };
        let db_low = magnitude_db(&band, 100.0);
        let db_high = magnitude_db(&band, 10000.0);
        assert!(db_low.abs() < 1.0, "high shelf should be flat below, got {db_low} dB at 100 Hz");
        assert!(db_high > 3.0, "high shelf should boost highs, got {db_high} dB at 10kHz");
    }

    #[test]
    fn low_pass_attenuates_above_cutoff() {
        let band = Band {
            frequency: 1000.0,
            gain_db: 0.0,
            q: 0.707,
            filter_type: FilterType::LowPass,
            enabled: true,
        };
        let db_low = magnitude_db(&band, 100.0);
        let db_high = magnitude_db(&band, 10000.0);
        assert!(db_low.abs() < 1.0, "low pass should pass lows, got {db_low} dB at 100 Hz");
        assert!(db_high < -10.0, "low pass should attenuate highs, got {db_high} dB at 10kHz");
    }

    #[test]
    fn high_pass_attenuates_below_cutoff() {
        let band = Band {
            frequency: 1000.0,
            gain_db: 0.0,
            q: 0.707,
            filter_type: FilterType::HighPass,
            enabled: true,
        };
        let db_low = magnitude_db(&band, 50.0);
        let db_high = magnitude_db(&band, 10000.0);
        assert!(db_low < -10.0, "high pass should attenuate lows, got {db_low} dB at 50 Hz");
        assert!(db_high.abs() < 1.0, "high pass should pass highs, got {db_high} dB at 10kHz");
    }

    #[test]
    fn combined_response_sums() {
        let bands = vec![
            Band { frequency: 200.0, gain_db: 3.0, q: 1.0, filter_type: FilterType::Peaking, enabled: true },
            Band { frequency: 200.0, gain_db: 3.0, q: 1.0, filter_type: FilterType::Peaking, enabled: true },
        ];
        let individual = magnitude_db(&bands[0], 200.0);
        let combined = combined_magnitude_db(&bands, 200.0);
        assert!(approx_eq(combined, individual * 2.0, 0.1), "combined should be ~2x individual at center");
    }

    #[test]
    fn log_frequencies_spans_range() {
        let freqs = log_frequencies(100);
        assert_eq!(freqs.len(), 100);
        assert!(approx_eq(freqs[0], 20.0, 0.01));
        assert!(approx_eq(freqs[99], 20000.0, 1.0));
        // Should be monotonically increasing
        for i in 1..freqs.len() {
            assert!(freqs[i] > freqs[i - 1]);
        }
    }
}
