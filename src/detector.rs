//! Transmission detection module
//!
//! Detects individual transmissions (frames) from continuous IQ streams
//! based on signal energy thresholds.

use crate::device::IqSample;

/// Configuration for transmission detection
#[derive(Clone, Debug)]
pub struct DetectorConfig {
    /// Energy threshold to detect start of transmission (0.0 - 1.0)
    pub squelch_threshold: f32,
    /// Number of samples below threshold to consider transmission ended
    pub hangtime_samples: usize,
    /// Minimum number of samples for a valid transmission
    pub min_transmission_samples: usize,
    /// Maximum number of samples for a transmission (prevents runaway)
    pub max_transmission_samples: usize,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            squelch_threshold: 0.003,  // ~-50 dB - suitable for weak signals
            hangtime_samples: 8192,        // ~4ms at 2Msps
            min_transmission_samples: 1024, // ~0.5ms at 2Msps
            max_transmission_samples: 2_000_000, // ~1s at 2Msps
        }
    }
}

/// State of the transmission detector
#[derive(Clone, Debug, PartialEq)]
pub enum DetectorState {
    /// Waiting for signal above threshold
    Idle,
    /// Currently receiving a transmission
    Receiving,
    /// In hangtime period after signal dropped
    Hangtime,
}

/// A detected transmission (frame)
#[derive(Clone)]
pub struct Transmission {
    /// The IQ samples of this transmission
    pub samples: Vec<IqSample>,
    /// Timestamp when transmission started (sample count from start)
    pub start_sample: u64,
    /// Peak signal level during transmission
    pub peak_level: f32,
    /// Average signal level during transmission
    pub avg_level: f32,
    /// Transmission ID (sequential)
    pub id: u64,
}

impl Transmission {
    /// Duration in samples
    pub fn duration_samples(&self) -> usize {
        self.samples.len()
    }

    /// Duration in seconds given sample rate
    pub fn duration_secs(&self, sample_rate: f64) -> f64 {
        self.samples.len() as f64 / sample_rate
    }
}

/// Transmission detector that processes IQ streams and extracts individual transmissions
pub struct TransmissionDetector {
    config: DetectorConfig,
    state: DetectorState,
    current_buffer: Vec<IqSample>,
    hangtime_counter: usize,
    total_samples: u64,
    transmission_count: u64,
    current_peak: f32,
    current_sum: f32,
}

impl TransmissionDetector {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            config,
            state: DetectorState::Idle,
            current_buffer: Vec::new(),
            hangtime_counter: 0,
            total_samples: 0,
            transmission_count: 0,
            current_peak: 0.0,
            current_sum: 0.0,
        }
    }

    /// Calculate signal energy/magnitude for a sample
    #[inline]
    fn sample_magnitude(sample: &IqSample) -> f32 {
        (sample.re * sample.re + sample.im * sample.im).sqrt()
    }

    /// Calculate average magnitude of a buffer
    fn buffer_magnitude(buffer: &[IqSample]) -> f32 {
        if buffer.is_empty() {
            return 0.0;
        }
        let sum: f32 = buffer.iter().map(Self::sample_magnitude).sum();
        sum / buffer.len() as f32
    }

    /// Process incoming IQ samples and return any completed transmissions
    pub fn process(&mut self, samples: &[IqSample]) -> Vec<Transmission> {
        let mut completed = Vec::new();

        for sample in samples {
            let magnitude = Self::sample_magnitude(sample);
            self.total_samples += 1;

            match self.state {
                DetectorState::Idle => {
                    if magnitude > self.config.squelch_threshold {
                        // Start of transmission
                        self.state = DetectorState::Receiving;
                        self.current_buffer.clear();
                        self.current_buffer.push(*sample);
                        self.current_peak = magnitude;
                        self.current_sum = magnitude;
                    }
                }
                DetectorState::Receiving => {
                    self.current_buffer.push(*sample);
                    self.current_peak = self.current_peak.max(magnitude);
                    self.current_sum += magnitude;

                    if magnitude < self.config.squelch_threshold {
                        // Signal dropped, enter hangtime
                        self.state = DetectorState::Hangtime;
                        self.hangtime_counter = 0;
                    }

                    // Check max length
                    if self.current_buffer.len() >= self.config.max_transmission_samples {
                        if let Some(tx) = self.finalize_transmission() {
                            completed.push(tx);
                        }
                    }
                }
                DetectorState::Hangtime => {
                    self.current_buffer.push(*sample);
                    self.hangtime_counter += 1;

                    if magnitude > self.config.squelch_threshold {
                        // Signal returned, back to receiving
                        self.state = DetectorState::Receiving;
                        self.hangtime_counter = 0;
                    } else if self.hangtime_counter >= self.config.hangtime_samples {
                        // Hangtime expired, transmission complete
                        if let Some(tx) = self.finalize_transmission() {
                            completed.push(tx);
                        }
                    }
                }
            }
        }

        completed
    }

    /// Finalize current transmission and reset state
    fn finalize_transmission(&mut self) -> Option<Transmission> {
        self.state = DetectorState::Idle;

        // Check minimum length
        if self.current_buffer.len() < self.config.min_transmission_samples {
            self.current_buffer.clear();
            return None;
        }

        // Trim hangtime samples from end
        let trim_end = self.hangtime_counter.min(self.current_buffer.len());
        let end_idx = self.current_buffer.len() - trim_end;

        self.transmission_count += 1;
        let tx = Transmission {
            samples: self.current_buffer[..end_idx].to_vec(),
            start_sample: self.total_samples - self.current_buffer.len() as u64,
            peak_level: self.current_peak,
            avg_level: if end_idx > 0 { self.current_sum / end_idx as f32 } else { 0.0 },
            id: self.transmission_count,
        };

        self.current_buffer.clear();
        self.hangtime_counter = 0;
        self.current_peak = 0.0;
        self.current_sum = 0.0;

        Some(tx)
    }

    /// Force-complete any in-progress transmission
    pub fn flush(&mut self) -> Option<Transmission> {
        if self.state != DetectorState::Idle && !self.current_buffer.is_empty() {
            self.finalize_transmission()
        } else {
            None
        }
    }

    /// Get current detector state
    pub fn state(&self) -> &DetectorState {
        &self.state
    }

    /// Get total number of completed transmissions
    pub fn transmission_count(&self) -> u64 {
        self.transmission_count
    }

    /// Check if currently receiving
    pub fn is_receiving(&self) -> bool {
        matches!(self.state, DetectorState::Receiving | DetectorState::Hangtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex;

    #[test]
    fn test_detector_idle() {
        let config = DetectorConfig::default();
        let mut detector = TransmissionDetector::new(config);

        // Low signal should not trigger
        let samples: Vec<IqSample> = (0..1000)
            .map(|_| Complex::new(0.001, 0.001))
            .collect();

        let transmissions = detector.process(&samples);
        assert!(transmissions.is_empty());
        assert_eq!(*detector.state(), DetectorState::Idle);
    }

    #[test]
    fn test_detector_transmission() {
        let mut config = DetectorConfig::default();
        config.hangtime_samples = 100;
        config.min_transmission_samples = 50;

        let mut detector = TransmissionDetector::new(config);

        // Create a transmission: low -> high -> low
        let mut samples = Vec::new();

        // Silence before
        for _ in 0..100 {
            samples.push(Complex::new(0.001, 0.001));
        }

        // Transmission
        for _ in 0..500 {
            samples.push(Complex::new(0.5, 0.5));
        }

        // Silence after (hangtime + extra)
        for _ in 0..200 {
            samples.push(Complex::new(0.001, 0.001));
        }

        let transmissions = detector.process(&samples);
        assert_eq!(transmissions.len(), 1);
        assert!(transmissions[0].samples.len() >= 500);
    }
}
