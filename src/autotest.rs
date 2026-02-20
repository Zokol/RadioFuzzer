//! Automated test mode — pure signal-quality functions
//!
//! Provides burst generation, SNR measurement, cross-correlation, and
//! result aggregation for loopback / over-the-air self-test of the relay
//! chain.

use crate::device::IqSample;
use num_complex::Complex;
use std::f64::consts::TAU;

/// Configuration for automated test mode
pub struct AutotestConfig {
    /// Duration of each TX burst in milliseconds
    pub tx_duration_ms: u64,
    /// Guard time after TX/RX switch in milliseconds
    pub switch_guard_ms: u64,
    /// Expected relay delay in milliseconds
    pub relay_delay_ms: u64,
    /// Number of bursts to transmit
    pub burst_count: u32,
    /// Minimum SNR (dB) for a burst to be considered passing
    pub snr_threshold_db: f32,
    /// Tone frequency offset in Hz for the test sinusoid
    pub tone_freq_hz: f64,
    /// Sample rate in samples per second
    pub sample_rate: f64,
    /// Buffer size in samples for RX/TX operations
    pub buffer_size: usize,
}

/// Result of a single burst measurement
#[derive(Debug, Clone)]
pub struct BurstResult {
    /// Sequential burst identifier
    pub burst_id: u32,
    /// Number of samples in the transmitted reference
    pub transmitted_samples: usize,
    /// Number of samples in the received capture
    pub received_samples: usize,
    /// Peak magnitude in the received capture
    pub peak_level: f32,
    /// Estimated noise floor from the beginning of the capture
    pub noise_floor: f32,
    /// Signal-to-noise ratio in dB
    pub snr_db: f32,
    /// Normalised cross-correlation peak between reference and received
    pub xcorr_peak: f32,
    /// Whether this burst met the SNR threshold
    pub passed: bool,
}

/// Aggregate report over all bursts
#[derive(Debug, Clone)]
pub struct AutotestReport {
    /// Total number of bursts evaluated
    pub total_bursts: u32,
    /// Bursts that met the SNR threshold
    pub passed_bursts: u32,
    /// Bursts that did not meet the SNR threshold
    pub failed_bursts: u32,
    /// Arithmetic mean SNR across all bursts (dB)
    pub mean_snr_db: f32,
    /// Minimum SNR across all bursts (dB)
    pub min_snr_db: f32,
    /// Arithmetic mean of the cross-correlation peaks
    pub mean_xcorr: f32,
    /// True if >= 80% of bursts passed
    pub overall_pass: bool,
}

/// Generate a complex-sinusoid burst at the given tone offset.
///
/// Produces `(sample_rate * duration_ms / 1000)` samples of a single-tone
/// IQ signal at 70 % full scale (amplitude = 0.7).
///
/// The phase is computed directly per sample (no accumulator drift):
///   `phase_i = (TAU * tone_freq_hz * i / sample_rate) mod TAU`
pub fn generate_burst(sample_rate: f64, tone_freq_hz: f64, duration_ms: u64) -> Vec<IqSample> {
    let num_samples = (sample_rate * duration_ms as f64 / 1000.0) as usize;
    let mut output = Vec::with_capacity(num_samples);

    for i in 0..num_samples {
        let phase = (TAU * tone_freq_hz * i as f64 / sample_rate) % TAU;
        output.push(Complex::new(
            (phase.cos() * 0.7) as f32,
            (phase.sin() * 0.7) as f32,
        ));
    }

    output
}

/// Estimate the noise floor from the first 10 % of a sample buffer.
///
/// Returns the RMS magnitude: `sqrt(mean(re^2 + im^2))`.
/// Returns 0.0 for an empty slice.
pub fn estimate_noise_floor(samples: &[IqSample]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let window_len = (samples.len() / 10).max(1);
    let window = &samples[..window_len];

    let sum_sq: f32 = window
        .iter()
        .map(|s| s.re * s.re + s.im * s.im)
        .sum();

    (sum_sq / window_len as f32).sqrt()
}

/// Measure signal-to-noise ratio in dB.
///
/// Computes the RMS magnitude of `signal` and returns
/// `20 * log10(signal_rms / noise_floor)`.
///
/// Returns `f32::NEG_INFINITY` when `noise_floor <= 0.0` or `signal`
/// is empty.
pub fn measure_snr(signal: &[IqSample], noise_floor: f32) -> f32 {
    if signal.is_empty() || noise_floor <= 0.0 {
        return f32::NEG_INFINITY;
    }

    let sum_sq: f32 = signal
        .iter()
        .map(|s| s.re * s.re + s.im * s.im)
        .sum();
    let signal_rms = (sum_sq / signal.len() as f32).sqrt();

    20.0 * (signal_rms / noise_floor).log10()
}

/// Compute the peak normalised cross-correlation between `reference`
/// and `received`.
///
/// Slides `reference` over `received` at integer-sample offsets and
/// returns the maximum normalised dot-product, clamped to [0, 1].
///
/// For efficiency the search is sub-sampled to at most 1024 offsets
/// when the lag range exceeds 1024.
pub fn normalised_xcorr_peak(reference: &[IqSample], received: &[IqSample]) -> f32 {
    if reference.is_empty() || received.is_empty() || reference.len() > received.len() {
        return 0.0;
    }

    let ref_len = reference.len();
    let max_offset = received.len() - ref_len; // inclusive upper bound

    // Pre-compute reference L2 norm (constant across all offsets).
    let ref_norm_sq: f32 = reference
        .iter()
        .map(|s| s.re * s.re + s.im * s.im)
        .sum();
    let ref_norm = ref_norm_sq.sqrt();

    if ref_norm == 0.0 {
        return 0.0;
    }

    // Build the list of offsets to evaluate.
    let offsets: Vec<usize> = if max_offset + 1 <= 1024 {
        (0..=max_offset).collect()
    } else {
        // Evenly spaced, always including 0 and max_offset.
        (0..1024)
            .map(|i| (i as u64 * max_offset as u64 / 1023) as usize)
            .collect()
    };

    let mut best: f32 = 0.0;

    for d in offsets {
        let window = &received[d..d + ref_len];

        // Dot product (real-valued, treating IQ as 2-D real vectors).
        let dot: f32 = reference
            .iter()
            .zip(window.iter())
            .map(|(r, w)| r.re * w.re + r.im * w.im)
            .sum();

        // Window L2 norm.
        let win_norm_sq: f32 = window
            .iter()
            .map(|s| s.re * s.re + s.im * s.im)
            .sum();
        let win_norm = win_norm_sq.sqrt();

        if win_norm == 0.0 {
            continue;
        }

        let normalised = dot / (ref_norm * win_norm);
        if normalised > best {
            best = normalised;
        }
    }

    best.clamp(0.0, 1.0)
}

/// Evaluate a single burst against its reference waveform.
pub fn evaluate_burst(
    reference: &[IqSample],
    received: &[IqSample],
    config: &AutotestConfig,
    burst_id: u32,
) -> BurstResult {
    let noise_floor = estimate_noise_floor(received);
    let snr_db = measure_snr(received, noise_floor);
    let xcorr_peak = normalised_xcorr_peak(reference, received);

    let peak_level = received
        .iter()
        .map(|s| (s.re * s.re + s.im * s.im).sqrt())
        .fold(0.0_f32, f32::max);

    let passed = snr_db >= config.snr_threshold_db;

    BurstResult {
        burst_id,
        transmitted_samples: reference.len(),
        received_samples: received.len(),
        peak_level,
        noise_floor,
        snr_db,
        xcorr_peak,
        passed,
    }
}

/// Summarise a set of burst results into an aggregate report.
pub fn summarise_results(results: &[BurstResult]) -> AutotestReport {
    if results.is_empty() {
        return AutotestReport {
            total_bursts: 0,
            passed_bursts: 0,
            failed_bursts: 0,
            mean_snr_db: 0.0,
            min_snr_db: 0.0,
            mean_xcorr: 0.0,
            overall_pass: false,
        };
    }

    let total_bursts = results.len() as u32;
    let passed_bursts = results.iter().filter(|r| r.passed).count() as u32;
    let failed_bursts = total_bursts - passed_bursts;

    let snr_sum: f32 = results.iter().map(|r| r.snr_db).sum();
    let mean_snr_db = snr_sum / total_bursts as f32;

    let min_snr_db = results
        .iter()
        .map(|r| r.snr_db)
        .fold(f32::INFINITY, f32::min);

    let xcorr_sum: f32 = results.iter().map(|r| r.xcorr_peak).sum();
    let mean_xcorr = xcorr_sum / total_bursts as f32;

    // Pass if >= 80 % of bursts passed (integer arithmetic).
    let overall_pass = passed_bursts * 100 / total_bursts >= 80;

    AutotestReport {
        total_bursts,
        passed_bursts,
        failed_bursts,
        mean_snr_db,
        min_snr_db,
        mean_xcorr,
        overall_pass,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex;

    // -- generate_burst -----------------------------------------------------

    #[test]
    fn test_generate_burst_length() {
        let burst = generate_burst(2_000_000.0, 5000.0, 100);
        assert_eq!(burst.len(), 200_000);
    }

    #[test]
    fn test_generate_burst_amplitude() {
        let burst = generate_burst(1_000_000.0, 1000.0, 10);
        for (i, s) in burst.iter().enumerate() {
            let mag = (s.re * s.re + s.im * s.im).sqrt();
            assert!(
                (0.69..=0.71).contains(&mag),
                "sample {} magnitude {:.6} out of [0.69, 0.71]",
                i,
                mag
            );
        }
    }

    // -- estimate_noise_floor -----------------------------------------------

    #[test]
    fn test_estimate_noise_floor_basic() {
        let samples: Vec<IqSample> = vec![Complex::new(0.01_f32, 0.0); 1000];
        let nf = estimate_noise_floor(&samples);
        assert!(
            (nf - 0.01).abs() < 0.001,
            "noise floor {:.6} not within 0.001 of 0.01",
            nf
        );
    }

    // -- measure_snr --------------------------------------------------------

    #[test]
    fn test_measure_snr_known() {
        let signal: Vec<IqSample> = vec![Complex::new(0.7_f32, 0.0); 1000];
        let snr = measure_snr(&signal, 0.007);
        // Expected: 20 * log10(0.7 / 0.007) = 20 * log10(100) = 40 dB
        assert!(
            (snr - 40.0).abs() < 0.5,
            "SNR {:.2} dB not within 0.5 dB of 40.0",
            snr
        );
    }

    #[test]
    fn test_measure_snr_zero_noise() {
        let signal: Vec<IqSample> = vec![Complex::new(0.7_f32, 0.0); 100];
        let snr = measure_snr(&signal, 0.0);
        assert!(
            snr == f32::NEG_INFINITY,
            "expected NEG_INFINITY, got {}",
            snr
        );
    }

    // -- normalised_xcorr_peak ----------------------------------------------

    #[test]
    fn test_xcorr_identical() {
        let reference = generate_burst(1_000_000.0, 5000.0, 1);
        assert_eq!(reference.len(), 1000);
        let received = reference.clone();
        let peak = normalised_xcorr_peak(&reference, &received);
        assert!(
            peak >= 0.99,
            "xcorr peak {:.4} < 0.99 for identical signals",
            peak
        );
    }

    #[test]
    fn test_xcorr_shifted() {
        let ref_samples: Vec<IqSample> = (0..200)
            .map(|i| {
                let phase = (TAU * 5000.0 * i as f64 / 1_000_000.0) % TAU;
                Complex::new((phase.cos() * 0.7) as f32, (phase.sin() * 0.7) as f32)
            })
            .collect();

        // received = 100 zeros + reference
        let mut received: Vec<IqSample> = vec![Complex::new(0.0, 0.0); 100];
        received.extend_from_slice(&ref_samples);

        let peak = normalised_xcorr_peak(&ref_samples, &received);
        assert!(
            peak >= 0.95,
            "xcorr peak {:.4} < 0.95 for shifted signal",
            peak
        );
    }

    #[test]
    fn test_xcorr_noise_only() {
        let ref_samples: Vec<IqSample> = (0..200)
            .map(|i| {
                let phase = (TAU * 5000.0 * i as f64 / 1_000_000.0) % TAU;
                Complex::new((phase.cos() * 0.7) as f32, (phase.sin() * 0.7) as f32)
            })
            .collect();

        // LCG PRNG (same constants as fuzzer.rs)
        let mut state: u64 = 0xDEADBEEF;
        let mut next_random = || -> u64 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            state
        };

        let received: Vec<IqSample> = (0..1000)
            .map(|_| {
                let r = next_random();
                let re = ((r & 0xFFFF) as f32 / 0xFFFF as f32) * 0.2 - 0.1;
                let im = (((r >> 16) & 0xFFFF) as f32 / 0xFFFF as f32) * 0.2 - 0.1;
                Complex::new(re, im)
            })
            .collect();

        let peak = normalised_xcorr_peak(&ref_samples, &received);
        assert!(
            peak < 0.5,
            "xcorr peak {:.4} >= 0.5 for noise-only received",
            peak
        );
    }

    // -- evaluate_burst -----------------------------------------------------

    fn default_test_config() -> AutotestConfig {
        AutotestConfig {
            tx_duration_ms: 10,
            switch_guard_ms: 5,
            relay_delay_ms: 5,
            burst_count: 1,
            snr_threshold_db: 10.0,
            tone_freq_hz: 5000.0,
            sample_rate: 1_000_000.0,
            buffer_size: 1024,
        }
    }

    #[test]
    fn test_evaluate_burst_pass() {
        let ref_samples: Vec<IqSample> = (0..500)
            .map(|i| {
                let phase = (TAU * 5000.0 * i as f64 / 1_000_000.0) % TAU;
                Complex::new((phase.cos() * 0.7) as f32, (phase.sin() * 0.7) as f32)
            })
            .collect();

        // received = 500 samples of small noise + 500 samples of reference
        let mut received: Vec<IqSample> = (0..500)
            .map(|_| Complex::new(0.001_f32, 0.001_f32))
            .collect();
        received.extend_from_slice(&ref_samples);

        let config = default_test_config();
        let result = evaluate_burst(&ref_samples, &received, &config, 1);
        assert!(
            result.passed,
            "burst should pass; snr_db={:.2}, threshold={}",
            result.snr_db, config.snr_threshold_db
        );
    }

    #[test]
    fn test_evaluate_burst_fail() {
        let ref_samples: Vec<IqSample> = (0..500)
            .map(|i| {
                let phase = (TAU * 5000.0 * i as f64 / 1_000_000.0) % TAU;
                Complex::new((phase.cos() * 0.7) as f32, (phase.sin() * 0.7) as f32)
            })
            .collect();

        // received = pure noise at roughly the same level as noise floor
        let received: Vec<IqSample> = vec![Complex::new(0.001_f32, 0.001_f32); 500];

        let config = default_test_config();
        let result = evaluate_burst(&ref_samples, &received, &config, 2);
        assert!(
            !result.passed,
            "burst should fail; snr_db={:.2}",
            result.snr_db
        );
    }

    // -- summarise_results --------------------------------------------------

    #[test]
    fn test_summarise_results() {
        let make_result = |id: u32, passed: bool| BurstResult {
            burst_id: id,
            transmitted_samples: 500,
            received_samples: 1000,
            peak_level: 0.7,
            noise_floor: 0.001,
            snr_db: if passed { 40.0 } else { 5.0 },
            xcorr_peak: if passed { 0.99 } else { 0.2 },
            passed,
        };

        let mut results: Vec<BurstResult> = (0..8).map(|i| make_result(i, true)).collect();
        results.extend((8..10).map(|i| make_result(i, false)));

        let report = summarise_results(&results);
        assert_eq!(report.total_bursts, 10);
        assert_eq!(report.passed_bursts, 8);
        assert_eq!(report.failed_bursts, 2);
        assert!(
            report.overall_pass,
            "80% pass rate should yield overall_pass == true"
        );
    }
}
