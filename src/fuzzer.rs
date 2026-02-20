//! Fuzzer module
//!
//! Provides various fuzzing strategies to modify, delay, or drop transmissions
//! before relaying them.

use crate::detector::Transmission;
use num_complex::Complex;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Result of fuzzer processing
#[derive(Clone)]
pub enum FuzzerAction {
    /// Pass transmission through unchanged
    Pass(Transmission),
    /// Pass transmission with modified samples
    Modified(Transmission),
    /// Drop the transmission entirely
    Drop,
    /// Delay the transmission (will be returned later)
    Delayed,
}

/// Fuzzer strategy types
#[derive(Clone, Debug, PartialEq)]
pub enum FuzzerStrategy {
    /// Pass all transmissions unchanged
    Passthrough,
    /// Drop a percentage of transmissions (0-100)
    DropPercent(u8),
    /// Delay each transmission by a fixed duration
    FixedDelay(Duration),
    /// Random delay between min and max duration
    RandomDelay { min: Duration, max: Duration },
    /// Corrupt a percentage of samples in each transmission
    CorruptSamples(u8),
    /// Replay previous transmissions instead of current
    Replay { buffer_size: usize },
    /// Bit flip in IQ samples
    BitFlip { probability: f32 },
    /// Attenuate signal by factor (0.0 - 1.0)
    Attenuate(f32),
    /// Add noise to signal
    AddNoise { amplitude: f32 },
    /// Truncate transmission to percentage of original length
    Truncate(u8),
    /// Reverse the transmission samples
    Reverse,
    /// Custom chain of multiple strategies
    Chain(Vec<FuzzerStrategy>),
}

/// Delayed transmission waiting to be released
struct DelayedTransmission {
    transmission: Transmission,
    release_time: Instant,
}

/// Fuzzer that processes transmissions according to configured strategy
pub struct Fuzzer {
    strategy: FuzzerStrategy,
    drop_counter: u64,
    pass_counter: u64,
    delayed_queue: VecDeque<DelayedTransmission>,
    replay_buffer: VecDeque<Transmission>,
    rng_state: u64,
}

impl Fuzzer {
    pub fn new(strategy: FuzzerStrategy) -> Self {
        Self {
            strategy,
            drop_counter: 0,
            pass_counter: 0,
            delayed_queue: VecDeque::new(),
            replay_buffer: VecDeque::new(),
            rng_state: 0x12345678,
        }
    }

    /// Simple pseudo-random number generator
    fn next_random(&mut self) -> u64 {
        self.rng_state = self.rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.rng_state
    }

    /// Random float between 0.0 and 1.0
    fn random_float(&mut self) -> f32 {
        (self.next_random() & 0xFFFFFF) as f32 / 0xFFFFFF as f32
    }

    /// Process a transmission through the fuzzer
    pub fn process(&mut self, transmission: Transmission) -> FuzzerAction {
        self.process_with_strategy(&self.strategy.clone(), transmission)
    }

    fn process_with_strategy(&mut self, strategy: &FuzzerStrategy, transmission: Transmission) -> FuzzerAction {
        match strategy {
            FuzzerStrategy::Passthrough => {
                self.pass_counter += 1;
                FuzzerAction::Pass(transmission)
            }

            FuzzerStrategy::DropPercent(percent) => {
                let roll = (self.next_random() % 100) as u8;
                if roll < *percent {
                    self.drop_counter += 1;
                    FuzzerAction::Drop
                } else {
                    self.pass_counter += 1;
                    FuzzerAction::Pass(transmission)
                }
            }

            FuzzerStrategy::FixedDelay(duration) => {
                self.delayed_queue.push_back(DelayedTransmission {
                    transmission,
                    release_time: Instant::now() + *duration,
                });
                FuzzerAction::Delayed
            }

            FuzzerStrategy::RandomDelay { min, max } => {
                let range = max.as_millis() - min.as_millis();
                let delay_ms = min.as_millis() + (self.next_random() as u128 % range.max(1));
                let delay = Duration::from_millis(delay_ms as u64);

                self.delayed_queue.push_back(DelayedTransmission {
                    transmission,
                    release_time: Instant::now() + delay,
                });
                FuzzerAction::Delayed
            }

            FuzzerStrategy::CorruptSamples(percent) => {
                let mut modified = transmission;
                let num_corrupt = (modified.samples.len() * *percent as usize) / 100;

                for _ in 0..num_corrupt {
                    let idx = (self.next_random() as usize) % modified.samples.len();
                    modified.samples[idx] = Complex::new(
                        self.random_float() * 2.0 - 1.0,
                        self.random_float() * 2.0 - 1.0,
                    );
                }

                self.pass_counter += 1;
                FuzzerAction::Modified(modified)
            }

            FuzzerStrategy::Replay { buffer_size } => {
                // Store current transmission
                self.replay_buffer.push_back(transmission.clone());
                while self.replay_buffer.len() > *buffer_size {
                    self.replay_buffer.pop_front();
                }

                // Return a previous transmission if available
                if self.replay_buffer.len() > 1 {
                    let idx = (self.next_random() as usize) % (self.replay_buffer.len() - 1);
                    let replayed = self.replay_buffer[idx].clone();
                    self.pass_counter += 1;
                    FuzzerAction::Modified(replayed)
                } else {
                    self.pass_counter += 1;
                    FuzzerAction::Pass(transmission)
                }
            }

            FuzzerStrategy::BitFlip { probability } => {
                let mut modified = transmission;

                for sample in &mut modified.samples {
                    if self.random_float() < *probability {
                        // Flip bits in the float representation
                        let mut re_bits = sample.re.to_bits();
                        let mut im_bits = sample.im.to_bits();
                        let bit_pos = (self.next_random() % 32) as u32;
                        re_bits ^= 1 << bit_pos;
                        im_bits ^= 1 << bit_pos;
                        sample.re = f32::from_bits(re_bits);
                        sample.im = f32::from_bits(im_bits);
                    }
                }

                self.pass_counter += 1;
                FuzzerAction::Modified(modified)
            }

            FuzzerStrategy::Attenuate(factor) => {
                let mut modified = transmission;
                for sample in &mut modified.samples {
                    sample.re *= factor;
                    sample.im *= factor;
                }
                self.pass_counter += 1;
                FuzzerAction::Modified(modified)
            }

            FuzzerStrategy::AddNoise { amplitude } => {
                let mut modified = transmission;
                for sample in &mut modified.samples {
                    sample.re += (self.random_float() * 2.0 - 1.0) * amplitude;
                    sample.im += (self.random_float() * 2.0 - 1.0) * amplitude;
                }
                self.pass_counter += 1;
                FuzzerAction::Modified(modified)
            }

            FuzzerStrategy::Truncate(percent) => {
                let mut modified = transmission;
                let new_len = (modified.samples.len() * *percent as usize) / 100;
                modified.samples.truncate(new_len.max(1));
                self.pass_counter += 1;
                FuzzerAction::Modified(modified)
            }

            FuzzerStrategy::Reverse => {
                let mut modified = transmission;
                modified.samples.reverse();
                self.pass_counter += 1;
                FuzzerAction::Modified(modified)
            }

            FuzzerStrategy::Chain(strategies) => {
                let mut current = transmission;
                for strat in strategies {
                    match self.process_with_strategy(strat, current) {
                        FuzzerAction::Pass(tx) | FuzzerAction::Modified(tx) => {
                            current = tx;
                        }
                        FuzzerAction::Drop => return FuzzerAction::Drop,
                        FuzzerAction::Delayed => return FuzzerAction::Delayed,
                    }
                }
                FuzzerAction::Modified(current)
            }
        }
    }

    /// Check for any delayed transmissions that are ready to be released
    pub fn poll_delayed(&mut self) -> Vec<Transmission> {
        let now = Instant::now();
        let mut ready = Vec::new();

        while let Some(front) = self.delayed_queue.front() {
            if front.release_time <= now {
                if let Some(delayed) = self.delayed_queue.pop_front() {
                    self.pass_counter += 1;
                    ready.push(delayed.transmission);
                }
            } else {
                break;
            }
        }

        ready
    }

    /// Get number of transmissions currently in delay queue
    pub fn delayed_count(&self) -> usize {
        self.delayed_queue.len()
    }

    /// Get statistics
    pub fn stats(&self) -> FuzzerStats {
        FuzzerStats {
            passed: self.pass_counter,
            dropped: self.drop_counter,
            delayed: self.delayed_queue.len() as u64,
        }
    }

    /// Get the strategy name for display
    pub fn strategy_name(&self) -> String {
        format!("{:?}", self.strategy)
    }
}

/// Fuzzer statistics
#[derive(Clone, Debug)]
pub struct FuzzerStats {
    pub passed: u64,
    pub dropped: u64,
    pub delayed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::Transmission;
    use num_complex::Complex;

    fn make_tx(n: usize) -> Transmission {
        Transmission {
            samples: (0..n)
                .map(|i| Complex::new(i as f32 * 0.01 + 0.01, i as f32 * -0.01 - 0.01))
                .collect(),
            start_sample: 0,
            peak_level: 1.0,
            avg_level: 0.5,
            id: 1,
        }
    }

    #[test]
    fn test_fuzzer_passthrough() {
        let mut f = Fuzzer::new(FuzzerStrategy::Passthrough);
        let tx = make_tx(100);
        let original = tx.samples.clone();
        match f.process(tx) {
            FuzzerAction::Pass(t) => assert_eq!(t.samples, original),
            _ => panic!("expected Pass"),
        }
    }

    #[test]
    fn test_fuzzer_drop_100() {
        let mut f = Fuzzer::new(FuzzerStrategy::DropPercent(100));
        for _ in 0..20 {
            match f.process(make_tx(100)) {
                FuzzerAction::Drop => {}
                _ => panic!("expected Drop"),
            }
        }
        assert_eq!(f.stats().dropped, 20);
    }

    #[test]
    fn test_fuzzer_drop_0() {
        let mut f = Fuzzer::new(FuzzerStrategy::DropPercent(0));
        for _ in 0..20 {
            match f.process(make_tx(100)) {
                FuzzerAction::Pass(_) => {}
                _ => panic!("expected Pass"),
            }
        }
    }

    #[test]
    fn test_fuzzer_fixed_delay_queues_then_releases() {
        let mut f = Fuzzer::new(FuzzerStrategy::FixedDelay(Duration::from_millis(1)));
        match f.process(make_tx(100)) {
            FuzzerAction::Delayed => {}
            _ => panic!("expected Delayed"),
        }
        assert_eq!(f.delayed_count(), 1);
        // Before deadline: nothing ready
        let early = f.poll_delayed();
        // After 5 ms: must be ready
        std::thread::sleep(Duration::from_millis(5));
        let ready = f.poll_delayed();
        assert!(early.len() + ready.len() == 1, "expected exactly 1 released transmission");
        assert_eq!(f.delayed_count(), 0);
    }

    #[test]
    fn test_fuzzer_random_delay() {
        let mut f = Fuzzer::new(FuzzerStrategy::RandomDelay {
            min: Duration::from_millis(1),
            max: Duration::from_millis(10),
        });
        match f.process(make_tx(100)) {
            FuzzerAction::Delayed => {}
            _ => panic!("expected Delayed"),
        }
        assert_eq!(f.delayed_count(), 1);
        std::thread::sleep(Duration::from_millis(20));
        let ready = f.poll_delayed();
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn test_fuzzer_corrupt_samples() {
        let mut f = Fuzzer::new(FuzzerStrategy::CorruptSamples(100));
        let tx = make_tx(100);
        let original = tx.samples.clone();
        match f.process(tx) {
            FuzzerAction::Modified(t) => {
                let changed = t.samples.iter().zip(&original).filter(|(a, b)| *a != *b).count();
                assert!(changed > 0, "expected at least some samples to be corrupted");
            }
            _ => panic!("expected Modified"),
        }
    }

    #[test]
    fn test_fuzzer_replay_first_pass_then_modified() {
        let mut f = Fuzzer::new(FuzzerStrategy::Replay { buffer_size: 10 });
        // First call: only 1 in buffer → Pass (nothing to replay yet)
        match f.process(make_tx(100)) {
            FuzzerAction::Pass(_) => {}
            _ => panic!("expected Pass for first replay"),
        }
        // Second call: 2 in buffer → Modified (replays a previous tx)
        match f.process(make_tx(200)) {
            FuzzerAction::Modified(_) => {}
            _ => panic!("expected Modified for subsequent replay"),
        }
    }

    #[test]
    fn test_fuzzer_bitflip_probability_1() {
        let mut f = Fuzzer::new(FuzzerStrategy::BitFlip { probability: 1.0 });
        let tx = make_tx(100);
        let original = tx.samples.clone();
        match f.process(tx) {
            FuzzerAction::Modified(t) => {
                let changed = t.samples.iter().zip(&original).filter(|(a, b)| *a != *b).count();
                // At probability 1.0 all samples should be flipped (extremely rare to miss any)
                assert!(changed > 90, "expected nearly all samples to be bit-flipped, got {}", changed);
            }
            _ => panic!("expected Modified"),
        }
    }

    #[test]
    fn test_fuzzer_attenuate() {
        let mut f = Fuzzer::new(FuzzerStrategy::Attenuate(0.5));
        let tx = make_tx(100);
        let original = tx.samples.clone();
        match f.process(tx) {
            FuzzerAction::Modified(t) => {
                for (a, b) in t.samples.iter().zip(&original) {
                    assert!((a.re - b.re * 0.5).abs() < 1e-6, "re mismatch");
                    assert!((a.im - b.im * 0.5).abs() < 1e-6, "im mismatch");
                }
            }
            _ => panic!("expected Modified"),
        }
    }

    #[test]
    fn test_fuzzer_add_noise_bounded() {
        let amplitude = 0.1f32;
        let mut f = Fuzzer::new(FuzzerStrategy::AddNoise { amplitude });
        let tx = make_tx(200);
        let original = tx.samples.clone();
        match f.process(tx) {
            FuzzerAction::Modified(t) => {
                let max_delta = t
                    .samples
                    .iter()
                    .zip(&original)
                    .map(|(a, b)| (a.re - b.re).abs().max((a.im - b.im).abs()))
                    .fold(0.0f32, f32::max);
                assert!(
                    max_delta <= amplitude + 1e-5,
                    "noise exceeded amplitude bound: {max_delta}"
                );
                let changed = t.samples.iter().zip(&original).filter(|(a, b)| *a != *b).count();
                assert!(changed > 0, "expected at least one sample to have noise added");
            }
            _ => panic!("expected Modified"),
        }
    }

    #[test]
    fn test_fuzzer_truncate_50_percent() {
        let mut f = Fuzzer::new(FuzzerStrategy::Truncate(50));
        let tx = make_tx(100);
        match f.process(tx) {
            FuzzerAction::Modified(t) => assert_eq!(t.samples.len(), 50),
            _ => panic!("expected Modified"),
        }
    }

    #[test]
    fn test_fuzzer_truncate_0_clamps_to_1() {
        let mut f = Fuzzer::new(FuzzerStrategy::Truncate(0));
        let tx = make_tx(100);
        match f.process(tx) {
            FuzzerAction::Modified(t) => assert_eq!(t.samples.len(), 1),
            _ => panic!("expected Modified with 1 sample minimum"),
        }
    }

    #[test]
    fn test_fuzzer_reverse() {
        let mut f = Fuzzer::new(FuzzerStrategy::Reverse);
        let tx = make_tx(5);
        let original = tx.samples.clone();
        match f.process(tx) {
            FuzzerAction::Modified(t) => {
                let reversed: Vec<_> = original.into_iter().rev().collect();
                assert_eq!(t.samples, reversed);
            }
            _ => panic!("expected Modified"),
        }
    }

    #[test]
    fn test_fuzzer_chain_attenuate_twice() {
        let chain = FuzzerStrategy::Chain(vec![
            FuzzerStrategy::Attenuate(0.5),
            FuzzerStrategy::Attenuate(0.5),
        ]);
        let mut f = Fuzzer::new(chain);
        let tx = make_tx(4);
        let original = tx.samples.clone();
        match f.process(tx) {
            FuzzerAction::Modified(t) => {
                for (a, b) in t.samples.iter().zip(&original) {
                    assert!((a.re - b.re * 0.25).abs() < 1e-6, "re mismatch");
                    assert!((a.im - b.im * 0.25).abs() < 1e-6, "im mismatch");
                }
            }
            _ => panic!("expected Modified"),
        }
    }

    #[test]
    fn test_fuzzer_chain_drop_short_circuits() {
        let chain = FuzzerStrategy::Chain(vec![
            FuzzerStrategy::DropPercent(100),
            FuzzerStrategy::Attenuate(0.5), // must never execute
        ]);
        let mut f = Fuzzer::new(chain);
        match f.process(make_tx(100)) {
            FuzzerAction::Drop => {}
            _ => panic!("expected Drop"),
        }
    }

    #[test]
    fn test_parse_strategy_all_variants() {
        assert_eq!(parse_strategy("passthrough").unwrap(), FuzzerStrategy::Passthrough);
        assert_eq!(parse_strategy("pass").unwrap(), FuzzerStrategy::Passthrough);
        assert_eq!(parse_strategy("drop:75").unwrap(), FuzzerStrategy::DropPercent(75));
        assert_eq!(parse_strategy("drop").unwrap(), FuzzerStrategy::DropPercent(50));
        assert_eq!(
            parse_strategy("delay:1000").unwrap(),
            FuzzerStrategy::FixedDelay(Duration::from_millis(1000))
        );
        assert_eq!(
            parse_strategy("random_delay:100:2000").unwrap(),
            FuzzerStrategy::RandomDelay {
                min: Duration::from_millis(100),
                max: Duration::from_millis(2000),
            }
        );
        assert_eq!(parse_strategy("corrupt:30").unwrap(), FuzzerStrategy::CorruptSamples(30));
        assert_eq!(
            parse_strategy("replay:5").unwrap(),
            FuzzerStrategy::Replay { buffer_size: 5 }
        );
        assert_eq!(
            parse_strategy("bitflip:0.01").unwrap(),
            FuzzerStrategy::BitFlip { probability: 0.01 }
        );
        assert_eq!(parse_strategy("attenuate:0.5").unwrap(), FuzzerStrategy::Attenuate(0.5));
        assert_eq!(
            parse_strategy("noise:0.1").unwrap(),
            FuzzerStrategy::AddNoise { amplitude: 0.1 }
        );
        assert_eq!(parse_strategy("truncate:70").unwrap(), FuzzerStrategy::Truncate(70));
        assert_eq!(parse_strategy("reverse").unwrap(), FuzzerStrategy::Reverse);
        assert!(parse_strategy("unknown_strategy").is_err());
    }
}

/// Parse a fuzzer strategy from a string
pub fn parse_strategy(s: &str) -> Result<FuzzerStrategy, String> {
    let parts: Vec<&str> = s.split(':').collect();

    match parts[0].to_lowercase().as_str() {
        "passthrough" | "pass" => Ok(FuzzerStrategy::Passthrough),

        "drop" => {
            let percent = parts.get(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(50);
            Ok(FuzzerStrategy::DropPercent(percent))
        }

        "delay" => {
            let ms = parts.get(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(1000);
            Ok(FuzzerStrategy::FixedDelay(Duration::from_millis(ms)))
        }

        "random_delay" => {
            let min_ms = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(100);
            let max_ms = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(2000);
            Ok(FuzzerStrategy::RandomDelay {
                min: Duration::from_millis(min_ms),
                max: Duration::from_millis(max_ms),
            })
        }

        "corrupt" => {
            let percent = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(10);
            Ok(FuzzerStrategy::CorruptSamples(percent))
        }

        "replay" => {
            let size = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(10);
            Ok(FuzzerStrategy::Replay { buffer_size: size })
        }

        "bitflip" => {
            let prob = parts.get(1)
                .and_then(|p| p.parse::<f32>().ok())
                .unwrap_or(0.001);
            Ok(FuzzerStrategy::BitFlip { probability: prob })
        }

        "attenuate" => {
            let factor = parts.get(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(0.5);
            Ok(FuzzerStrategy::Attenuate(factor))
        }

        "noise" => {
            let amp = parts.get(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(0.1);
            Ok(FuzzerStrategy::AddNoise { amplitude: amp })
        }

        "truncate" => {
            let percent = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(50);
            Ok(FuzzerStrategy::Truncate(percent))
        }

        "reverse" => Ok(FuzzerStrategy::Reverse),

        _ => Err(format!("Unknown fuzzer strategy: {}", s)),
    }
}
