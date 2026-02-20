//! Command-line argument parsing and configuration

use clap::Parser;

/// RadioFuzz - Bidirectional SDR IQ Relay
/// Reads IQ data from one SDR's RX and transmits it on the other SDR's TX, and vice versa.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// First SDR device filter (e.g., "driver=rtlsdr" or device index "0")
    #[arg(short = '1', long, default_value = "0")]
    pub device1: String,

    /// Second SDR device filter (e.g., "driver=hackrf" or device index "1")
    /// If not specified or set to "same", uses device1 with different channels
    #[arg(short = '2', long, default_value = "1")]
    pub device2: String,

    /// Use single device mode with dual channels (for LimeSDR, BladeRF, etc.)
    /// Channel A (0) will relay to/from Channel B (1)
    #[arg(long)]
    pub single_device: bool,

    /// Sample rate in Hz
    #[arg(short, long, default_value = "2000000")]
    pub sample_rate: f64,

    /// Center frequency in Hz for device 1 / channel A
    #[arg(long, default_value = "100000000")]
    pub freq1: f64,

    /// Center frequency in Hz for device 2 / channel B
    #[arg(long, default_value = "100000000")]
    pub freq2: f64,

    /// RX gain for device 1 / channel A in dB
    #[arg(long, default_value = "30.0")]
    pub rx_gain1: f64,

    /// TX gain for device 1 / channel A in dB
    #[arg(long, default_value = "30.0")]
    pub tx_gain1: f64,

    /// RX gain for device 2 / channel B in dB
    #[arg(long, default_value = "30.0")]
    pub rx_gain2: f64,

    /// TX gain for device 2 / channel B in dB
    #[arg(long, default_value = "30.0")]
    pub tx_gain2: f64,

    /// Buffer size (number of IQ samples per transfer)
    #[arg(short, long, default_value = "65536")]
    pub buffer_size: usize,

    /// Channel buffer depth (number of buffers to queue)
    #[arg(long, default_value = "16")]
    pub channel_depth: usize,

    /// RX channel index for device 1 / path A (default: 0)
    #[arg(long, default_value = "0")]
    pub rx_chan1: usize,

    /// TX channel index for device 1 / path A (default: 0)
    #[arg(long, default_value = "0")]
    pub tx_chan1: usize,

    /// RX channel index for device 2 / path B (default: 0, or 1 in single-device mode)
    #[arg(long)]
    pub rx_chan2: Option<usize>,

    /// TX channel index for device 2 / path B (default: 0, or 1 in single-device mode)
    #[arg(long)]
    pub tx_chan2: Option<usize>,

    /// List available devices and their channels, then exit
    #[arg(long)]
    pub list_channels: bool,

    /// Unidirectional mode: only relay from device1/channelA RX to device2/channelB TX
    #[arg(long)]
    pub unidirectional: bool,

    /// Reverse direction in unidirectional mode: relay from device2/channelB RX to device1/channelA TX
    #[arg(long)]
    pub reverse: bool,

    /// Enable fuzzer mode with transmission detection
    #[arg(long)]
    pub fuzz: bool,

    /// Fuzzer strategy (passthrough, drop:50, delay:1000, random_delay:100:2000,
    /// corrupt:10, replay:10, bitflip:0.001, attenuate:0.5, noise:0.1, truncate:50, reverse)
    #[arg(long, default_value = "passthrough")]
    pub fuzz_strategy: String,

    /// Squelch threshold for transmission detection (0.0 - 1.0)
    /// Default 0.003 corresponds to about -50 dB
    #[arg(long, default_value = "0.003")]
    pub squelch: f32,

    /// Hangtime in milliseconds after signal drops before ending transmission
    #[arg(long, default_value = "4")]
    pub hangtime_ms: u64,

    /// Minimum transmission length in milliseconds
    #[arg(long, default_value = "1")]
    pub min_tx_ms: u64,

    /// Maximum transmission length in milliseconds
    #[arg(long, default_value = "5000")]
    pub max_tx_ms: u64,

    /// Directory to record transmissions (IQ files saved as .iq32 files)
    #[arg(long)]
    pub record_dir: Option<String>,

    /// Replay transmissions from a directory instead of live RX
    #[arg(long)]
    pub replay_dir: Option<String>,

    /// Delay between replayed transmissions in milliseconds
    #[arg(long, default_value = "100")]
    pub replay_delay_ms: u64,

    /// Loop replay indefinitely
    #[arg(long)]
    pub replay_loop: bool,

    /// Enable debug mode with test signal generation and statistics
    #[arg(long)]
    pub debug: bool,

    /// Generate test tone instead of relaying (frequency offset in Hz)
    #[arg(long, default_value = "1000")]
    pub test_tone_freq: f64,

    /// Run for specified seconds then exit (0 = run indefinitely)
    #[arg(long, default_value = "0")]
    pub debug_duration: u64,

    /// Print statistics every N seconds
    #[arg(long, default_value = "1")]
    pub stats_interval: u64,

    /// Log sample values (very verbose!)
    #[arg(long)]
    pub log_samples: bool,

    /// Test TX only (generate test signal on TX, no RX relay)
    #[arg(long)]
    pub test_tx_only: bool,

    /// Test RX only (receive and analyze, no TX)
    #[arg(long)]
    pub test_rx_only: bool,

    /// Enable automated test mode (requires --device3)
    #[arg(long)]
    pub autotest: bool,

    /// Third SDR device filter for automated test mode (e.g., "driver=hackrf")
    #[arg(long, default_value = "")]
    pub device3: String,

    /// RX gain for Device3 in automated test mode (dB)
    #[arg(long, default_value = "30.0")]
    pub autotest_rx_gain: f64,

    /// TX gain for Device3 in automated test mode (dB)
    #[arg(long, default_value = "30.0")]
    pub autotest_tx_gain: f64,

    /// Duration of each test burst transmitted by Device3 (ms)
    #[arg(long, default_value = "200")]
    pub autotest_tx_ms: u64,

    /// Guard time after Device3 TX before starting RX, to allow hardware TX->RX switch (ms)
    #[arg(long, default_value = "50")]
    pub autotest_switch_guard_ms: u64,

    /// Fixed relay delay in autotest mode (ms). Must be >= autotest-tx-ms + autotest-switch-guard-ms.
    #[arg(long, default_value = "500")]
    pub autotest_relay_delay_ms: u64,

    /// Number of test bursts to send before reporting pass/fail
    #[arg(long, default_value = "10")]
    pub autotest_bursts: u32,

    /// Minimum SNR (dB) of the received relay signal for a burst to pass
    #[arg(long, default_value = "10.0")]
    pub autotest_snr_threshold_db: f32,

    /// Frequency offset of the test tone within the channel bandwidth (Hz)
    #[arg(long, default_value = "5000.0")]
    pub autotest_tone_freq_hz: f64,

    /// Channel index on Device3 (for both TX and RX)
    #[arg(long, default_value = "0")]
    pub autotest_channel: usize,
}

impl Args {
    /// Get the RX channel for device/path 2, accounting for single-device mode
    pub fn rx_chan2(&self) -> usize {
        self.rx_chan2.unwrap_or(if self.single_device { 1 } else { 0 })
    }

    /// Get the TX channel for device/path 2, accounting for single-device mode
    pub fn tx_chan2(&self) -> usize {
        self.tx_chan2.unwrap_or(if self.single_device { 1 } else { 0 })
    }

    /// Calculate hangtime in samples based on sample rate
    pub fn hangtime_samples(&self) -> usize {
        ((self.hangtime_ms as f64 / 1000.0) * self.sample_rate) as usize
    }

    /// Calculate minimum transmission samples based on sample rate
    pub fn min_tx_samples(&self) -> usize {
        ((self.min_tx_ms as f64 / 1000.0) * self.sample_rate) as usize
    }

    /// Calculate maximum transmission samples based on sample rate
    pub fn max_tx_samples(&self) -> usize {
        ((self.max_tx_ms as f64 / 1000.0) * self.sample_rate) as usize
    }

    /// Returns the relay delay as a Duration for use with FuzzerStrategy::FixedDelay
    pub fn autotest_relay_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.autotest_relay_delay_ms)
    }
}
