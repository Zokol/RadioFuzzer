//! Debug statistics and signal analysis

use num_complex::Complex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use crate::device::IqSample;

/// Statistics for debug mode
#[derive(Default)]
pub struct DebugStats {
    pub rx_samples: AtomicU64,
    pub tx_samples: AtomicU64,
    pub rx_buffers: AtomicU64,
    pub tx_buffers: AtomicU64,
    pub rx_errors: AtomicU64,
    pub tx_errors: AtomicU64,
    rx_peak: AtomicU32, // stored as bits
    tx_peak: AtomicU32,
    rx_avg: AtomicU32,  // current average level (stored as bits)
    tx_avg: AtomicU32,
    // Transmission detection stats
    pub transmissions_detected_a: AtomicU64,
    pub transmissions_detected_b: AtomicU64,
    pub transmission_samples_a: AtomicU64,
    pub transmission_samples_b: AtomicU64,
}

impl DebugStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update_rx_peak(&self, value: f32) {
        if value.is_finite() {
            let bits = value.to_bits();
            self.rx_peak.fetch_max(bits, Ordering::Relaxed);
        }
    }

    pub fn update_tx_peak(&self, value: f32) {
        if value.is_finite() {
            let bits = value.to_bits();
            self.tx_peak.fetch_max(bits, Ordering::Relaxed);
        }
    }

    pub fn update_rx_avg(&self, value: f32) {
        self.rx_avg.store(value.to_bits(), Ordering::Relaxed);
    }

    pub fn update_tx_avg(&self, value: f32) {
        self.tx_avg.store(value.to_bits(), Ordering::Relaxed);
    }

    pub fn get_rx_peak(&self) -> f32 {
        f32::from_bits(self.rx_peak.load(Ordering::Relaxed))
    }

    pub fn get_tx_peak(&self) -> f32 {
        f32::from_bits(self.tx_peak.load(Ordering::Relaxed))
    }

    pub fn get_rx_avg(&self) -> f32 {
        f32::from_bits(self.rx_avg.load(Ordering::Relaxed))
    }

    pub fn get_tx_avg(&self) -> f32 {
        f32::from_bits(self.tx_avg.load(Ordering::Relaxed))
    }

    pub fn reset_peaks(&self) {
        self.rx_peak.store(0, Ordering::Relaxed);
        self.tx_peak.store(0, Ordering::Relaxed);
    }

    pub fn record_transmission_a(&self, sample_count: usize) {
        self.transmissions_detected_a.fetch_add(1, Ordering::Relaxed);
        self.transmission_samples_a.fetch_add(sample_count as u64, Ordering::Relaxed);
    }

    pub fn record_transmission_b(&self, sample_count: usize) {
        self.transmissions_detected_b.fetch_add(1, Ordering::Relaxed);
        self.transmission_samples_b.fetch_add(sample_count as u64, Ordering::Relaxed);
    }

    pub fn total_transmissions(&self) -> u64 {
        self.transmissions_detected_a.load(Ordering::Relaxed)
            + self.transmissions_detected_b.load(Ordering::Relaxed)
    }
}

/// Generate a test tone (complex sinusoid) for debug mode
pub fn generate_test_tone(
    buffer: &mut [IqSample],
    sample_rate: f64,
    tone_freq: f64,
    phase: &mut f64,
) {
    let phase_increment = 2.0 * std::f64::consts::PI * tone_freq / sample_rate;

    for sample in buffer.iter_mut() {
        let i = (*phase).cos() as f32;
        let q = (*phase).sin() as f32;
        *sample = Complex::new(i * 0.7, q * 0.7); // 70% amplitude to avoid clipping
        *phase += phase_increment;
        if *phase > 2.0 * std::f64::consts::PI {
            *phase -= 2.0 * std::f64::consts::PI;
        }
    }
}

/// Analyze IQ samples and return (peak, average, rms)
pub fn analyze_samples(buffer: &[IqSample]) -> (f32, f32, f32) {
    if buffer.is_empty() {
        return (0.0, 0.0, 0.0);
    }

    let mut peak = 0.0f32;
    let mut sum_magnitude = 0.0f32;
    let mut sum_squared = 0.0f32;

    for sample in buffer {
        let magnitude = (sample.re * sample.re + sample.im * sample.im).sqrt();
        peak = peak.max(magnitude);
        sum_magnitude += magnitude;
        sum_squared += magnitude * magnitude;
    }

    let avg_magnitude = sum_magnitude / buffer.len() as f32;
    let rms = (sum_squared / buffer.len() as f32).sqrt();

    (peak, avg_magnitude, rms)
}

/// Print debug statistics
pub fn print_stats(stats: &DebugStats, elapsed: Duration, sample_rate: f64, squelch_threshold: f32) {
    let rx_samples = stats.rx_samples.load(Ordering::Relaxed);
    let tx_samples = stats.tx_samples.load(Ordering::Relaxed);
    let rx_buffers = stats.rx_buffers.load(Ordering::Relaxed);
    let tx_buffers = stats.tx_buffers.load(Ordering::Relaxed);
    let rx_errors = stats.rx_errors.load(Ordering::Relaxed);
    let tx_errors = stats.tx_errors.load(Ordering::Relaxed);
    let rx_peak = stats.get_rx_peak();
    let tx_peak = stats.get_tx_peak();
    let rx_avg = stats.get_rx_avg();
    let tx_avg = stats.get_tx_avg();

    // Transmission stats
    let tx_detected_a = stats.transmissions_detected_a.load(Ordering::Relaxed);
    let tx_detected_b = stats.transmissions_detected_b.load(Ordering::Relaxed);
    let tx_samples_a = stats.transmission_samples_a.load(Ordering::Relaxed);
    let tx_samples_b = stats.transmission_samples_b.load(Ordering::Relaxed);

    let elapsed_secs = elapsed.as_secs_f64();
    let expected_samples = (sample_rate * elapsed_secs) as u64;

    let rx_rate = if elapsed_secs > 0.0 {
        rx_samples as f64 / elapsed_secs / 1e6
    } else {
        0.0
    };
    let tx_rate = if elapsed_secs > 0.0 {
        tx_samples as f64 / elapsed_secs / 1e6
    } else {
        0.0
    };

    // Convert to dB for better readability
    let rx_peak_db = if rx_peak > 0.0 { 20.0 * rx_peak.log10() } else { -100.0 };
    let rx_avg_db = if rx_avg > 0.0 { 20.0 * rx_avg.log10() } else { -100.0 };
    let squelch_db = if squelch_threshold > 0.0 { 20.0 * squelch_threshold.log10() } else { -100.0 };

    println!("─────────────────────────────────────────────────────────");
    println!("  Time: {:.1}s", elapsed_secs);
    println!(
        "  RX: {} samples ({:.2} Msps), {} buffers, {} errors",
        rx_samples, rx_rate, rx_buffers, rx_errors
    );
    println!(
        "  RX Power: avg={:.4} ({:.1} dB), peak={:.4} ({:.1} dB), squelch={:.4} ({:.1} dB)",
        rx_avg, rx_avg_db, rx_peak, rx_peak_db, squelch_threshold, squelch_db
    );

    // Show if signal is above/below squelch
    if rx_avg > squelch_threshold {
        println!("  Signal Status: ABOVE SQUELCH (should detect transmissions)");
    } else if rx_peak > squelch_threshold {
        println!("  Signal Status: Peak above squelch, avg below (intermittent signal)");
    } else {
        println!("  Signal Status: BELOW SQUELCH (no transmissions will be detected)");
    }

    println!(
        "  TX: {} samples ({:.2} Msps), {} buffers, {} errors, peak={:.4}",
        tx_samples, tx_rate, tx_buffers, tx_errors, tx_peak
    );

    // Show transmission detection stats
    let total_tx = tx_detected_a + tx_detected_b;
    if total_tx > 0 || tx_samples_a > 0 || tx_samples_b > 0 {
        println!(
            "  Transmissions: ChA={} ({} samples), ChB={} ({} samples)",
            tx_detected_a, tx_samples_a, tx_detected_b, tx_samples_b
        );
    } else {
        println!("  Transmissions: None detected");
    }

    if expected_samples > 0 {
        let rx_efficiency = (rx_samples as f64 / expected_samples as f64) * 100.0;
        println!("  RX efficiency: {:.1}% of expected throughput", rx_efficiency);
    }
    println!("─────────────────────────────────────────────────────────");
}
