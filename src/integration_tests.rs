//! Hardware integration tests
//!
//! These tests require physical SDR hardware and are skipped by default.
//! USRP B206mini devices load firmware over USB on first open; running two
//! tests in parallel causes a UHD ihex firmware-upload race.  Always use
//! `--test-threads=1` when running hardware tests.
//!
//! Run all hardware tests (single-threaded):
//!   cargo test -- --include-ignored --test-threads=1
//!
//! Run a specific hardware test:
//!   cargo test test_device1_can_receive -- --include-ignored --test-threads=1

#[cfg(test)]
mod tests {
    use crate::detector::DetectorConfig;
    use crate::device::{configure_device, open_device, IqSample};
    use crate::fuzzer::{Fuzzer, FuzzerStrategy};
    use crate::stats::{generate_test_tone, DebugStats};
    use crate::threads::{delayed_poller_thread, rx_fuzz_thread, tx_fuzz_thread};
    use crossbeam_channel::bounded;
    use num_complex::Complex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    const DEVICE1: &str = "driver=uhd,serial=34CD2C3";
    const DEVICE2: &str = "driver=uhd,serial=3504A2C";
    const FREQ_HZ: f64 = 433_000_000.0;
    const SAMPLE_RATE: f64 = 2_000_000.0;
    const RX_GAIN: f64 = 30.0;
    const TX_GAIN: f64 = 30.0;
    const BUFFER_SIZE: usize = 65_536;

    /// Verify that Device1 can be opened, configured, and produce a non-empty RX buffer.
    ///
    /// This does not assert anything about signal content — it only checks that the
    /// hardware driver responds without error and delivers at least one sample.
    #[test]
    #[ignore = "requires USRP B206mini (serial=34CD2C3) connected via USB 3"]
    fn test_device1_can_receive() {
        let device = open_device(DEVICE1).expect("Failed to open Device1");
        configure_device(
            &device,
            SAMPLE_RATE,
            FREQ_HZ,
            FREQ_HZ,
            RX_GAIN,
            TX_GAIN,
            0,
            0,
        )
        .expect("Failed to configure Device1");

        let mut rx_stream = device
            .rx_stream::<IqSample>(&[0])
            .expect("Failed to create RX stream on Device1");
        rx_stream
            .activate(None)
            .expect("Failed to activate RX stream");

        let mut buffer = vec![Complex::new(0.0f32, 0.0f32); BUFFER_SIZE];
        let len = rx_stream
            .read(&mut [&mut buffer], 1_000_000)
            .expect("Failed to read samples from Device1 RX");

        rx_stream.deactivate(None).ok();

        assert!(len > 0, "Expected at least 1 sample from Device1, got 0");
        println!("[Device1 RX] Received {len} samples at {FREQ_HZ:.0} Hz");
    }

    /// Verify that Device2 can be opened, configured, and transmit an audible test tone.
    ///
    /// Transmits a 500 ms, +5 kHz tone at 433 MHz (i.e. a CW carrier at 433.005 MHz).
    /// A receiver on 433 MHz should hear a clear tone for roughly half a second.
    ///
    /// Three pitfalls fixed vs. a naive implementation:
    ///  1. Long enough burst (500 ms) to be reliably detected by a scanning receiver.
    ///  2. `end_of_burst` flag set only on the FINAL write; setting it on every chunk
    ///     causes UHD to treat each chunk as a separate burst, producing gaps.
    ///  3. A 200 ms drain sleep before `deactivate()` so the UHD TX pipeline
    ///     (USB → FPGA FIFO → DAC) has time to flush before the stream is torn down.
    #[test]
    #[ignore = "requires USRP B206mini (serial=3504A2C) connected via USB 3"]
    fn test_device2_can_transmit() {

        const TX_DURATION_MS: f64 = 500.0;
        const TONE_OFFSET_HZ: f64 = 5_000.0;
        const DRAIN_MS: u64 = 200;

        let n_samples = (SAMPLE_RATE * TX_DURATION_MS / 1000.0) as usize;

        let device = open_device(DEVICE2).expect("Failed to open Device2");
        configure_device(
            &device,
            SAMPLE_RATE,
            FREQ_HZ,
            FREQ_HZ,
            RX_GAIN,
            TX_GAIN,
            0,
            0,
        )
        .expect("Failed to configure Device2");

        let mut tx_stream = device
            .tx_stream::<IqSample>(&[0])
            .expect("Failed to create TX stream on Device2");
        tx_stream
            .activate(None)
            .expect("Failed to activate TX stream");

        let mut buffer = vec![Complex::new(0.0f32, 0.0f32); n_samples];
        let mut phase = 0.0f64;
        generate_test_tone(&mut buffer, SAMPLE_RATE, TONE_OFFSET_HZ, &mut phase);

        // Write in chunks; set end_of_burst only on the final write so UHD
        // treats the whole sequence as one continuous burst (no inter-chunk gaps).
        // The write slice is capped to BUFFER_SIZE so that is_last is computed
        // correctly even when SoapySDR could accept all remaining samples in one call.
        let mut offset = 0;
        while offset < buffer.len() {
            let chunk_end = (offset + BUFFER_SIZE).min(buffer.len());
            let is_last = chunk_end == buffer.len();
            let written = tx_stream
                .write(&[&buffer[offset..chunk_end]], None, is_last, 1_000_000)
                .expect("TX write failed on Device2");
            assert!(written > 0, "TX stream stalled (0 bytes written)");
            offset += written;
        }

        // Allow the UHD TX pipeline (USB transfer → FPGA FIFO → DAC) to drain
        // before tearing down the stream.
        std::thread::sleep(Duration::from_millis(DRAIN_MS));

        tx_stream.deactivate(None).ok();
        println!(
            "[Device2 TX] Transmitted {n_samples} samples ({TX_DURATION_MS} ms) \
             at {FREQ_HZ:.0} Hz + {TONE_OFFSET_HZ:.0} Hz offset"
        );
    }

    /// Live relay bridge: Device1 RX 433 MHz → 1000 ms delay → Device2 TX 432.5 MHz.
    ///
    /// Runs for 15 seconds.  Transmit anything on 433 MHz during that window and your
    /// receiver on 432.5 MHz should hear it ~1 second later.
    ///
    /// Automated assertions (no third radio needed):
    ///   - RX sample counter > 0  (both driver paths are alive)
    ///   - TX error counter == 0  (no underruns / stream errors on Device2)
    ///
    /// Pipeline:
    ///   Device1 RX 433 MHz
    ///     └─ rx_fuzz_thread  (squelch 0.002, FixedDelay 1000 ms)
    ///         └─ delayed_poller_thread  (polls every 10 ms)
    ///             └─ tx_fuzz_thread
    ///                 └─ Device2 TX 432.5 MHz
    #[test]
    #[ignore = "requires both USRP B206mini devices; transmit on 433 MHz to exercise the relay"]
    fn test_relay_bridge_433_to_432_5_mhz_1000ms_delay() {
        const RX_FREQ: f64 = 433_000_000.0;
        const TX_FREQ: f64 = 432_500_000.0;
        const RELAY_DELAY_MS: u64 = 1000;
        const RUN_SECS: u64 = 15;
        const SQUELCH: f32 = 0.002;
        const CHANNEL_DEPTH: usize = 16;

        let running = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(DebugStats::new());

        // Open and configure devices.
        // RX gain 20 dB: 30 dB caused ADC saturation (peak > 1.0) when a nearby
        // handheld transmits.  20 dB gives ~6 dB headroom at close range.
        const RX_GAIN_DB: f64 = 20.0;
        const TX_GAIN_DB: f64 = 60.0;

        let dev1 = open_device(DEVICE1).expect("Failed to open Device1");
        configure_device(&dev1, SAMPLE_RATE, RX_FREQ, RX_FREQ, RX_GAIN_DB, TX_GAIN_DB, 0, 0)
            .expect("Failed to configure Device1 for RX");

        let dev2 = open_device(DEVICE2).expect("Failed to open Device2");
        configure_device(&dev2, SAMPLE_RATE, TX_FREQ, TX_FREQ, RX_GAIN_DB, TX_GAIN_DB, 0, 0)
            .expect("Failed to configure Device2 for TX");

        // Create streams
        let rx_stream1 = dev1
            .rx_stream::<IqSample>(&[0])
            .expect("Failed to create RX stream on Device1");
        let tx_stream2 = dev2
            .tx_stream::<IqSample>(&[0])
            .expect("Failed to create TX stream on Device2");

        // Bounded channel: rx_fuzz_thread and delayed_poller both send here;
        // tx_fuzz_thread reads from here.
        let (tx_sender, tx_receiver) = bounded(CHANNEL_DEPTH);
        let poller_sender = tx_sender.clone();

        let fuzzer = Arc::new(std::sync::Mutex::new(Fuzzer::new(
            FuzzerStrategy::FixedDelay(Duration::from_millis(RELAY_DELAY_MS)),
        )));

        let detector_config = DetectorConfig {
            squelch_threshold: SQUELCH,
            hangtime_samples: (SAMPLE_RATE * 0.050) as usize,  // 50 ms
            min_transmission_samples: (SAMPLE_RATE * 0.010) as usize, // 10 ms
            max_transmission_samples: (SAMPLE_RATE * 10.0) as usize,  // 10 s cap
        };

        println!("\n[Bridge] === Relay bridge test ===");
        println!("[Bridge] RX  {:.1} MHz  on Device1 (serial={})", RX_FREQ / 1e6, &DEVICE1[DEVICE1.rfind('=').unwrap() + 1..]);
        println!("[Bridge] TX  {:.1} MHz  on Device2 (serial={})", TX_FREQ / 1e6, &DEVICE2[DEVICE2.rfind('=').unwrap() + 1..]);
        println!("[Bridge] Delay: {} ms   Run time: {} s", RELAY_DELAY_MS, RUN_SECS);
        println!("[Bridge] Transmit on {:.1} MHz now — your receiver should hear it on {:.1} MHz ~{} ms later.",
            RX_FREQ / 1e6, TX_FREQ / 1e6, RELAY_DELAY_MS);

        let (r1, r2, r3) = (running.clone(), running.clone(), running.clone());
        let (s1, s2) = (stats.clone(), stats.clone());
        let f1 = fuzzer.clone();

        let h_rx = thread::Builder::new()
            .name("Bridge-RX".into())
            .spawn(move || {
                rx_fuzz_thread(
                    rx_stream1, tx_sender, BUFFER_SIZE,
                    r1, s1, detector_config, f1,
                    "Bridge-RX", true,
                );
            })
            .expect("failed to spawn Bridge-RX");

        let h_delay = thread::Builder::new()
            .name("Bridge-Delay".into())
            .spawn(move || {
                delayed_poller_thread(fuzzer, poller_sender, r2, "Bridge-Delay");
            })
            .expect("failed to spawn Bridge-Delay");

        let h_tx = thread::Builder::new()
            .name("Bridge-TX".into())
            .spawn(move || {
                tx_fuzz_thread(
                    tx_stream2, tx_receiver,
                    r3, s2, "Bridge-TX", true, SAMPLE_RATE,
                );
            })
            .expect("failed to spawn Bridge-TX");

        // Run for the fixed window, then stop all threads
        thread::sleep(Duration::from_secs(RUN_SECS));
        running.store(false, Ordering::Relaxed);

        h_rx.join().expect("Bridge-RX panicked");
        h_delay.join().expect("Bridge-Delay panicked");
        h_tx.join().expect("Bridge-TX panicked");

        let rx_samples = stats.rx_samples.load(Ordering::Relaxed);
        let tx_errors  = stats.tx_errors.load(Ordering::Relaxed);
        let detections = stats.transmissions_detected_a.load(Ordering::Relaxed);

        println!(
            "[Bridge] Done — RX samples: {}, transmissions detected: {}, TX errors: {}",
            rx_samples, detections, tx_errors
        );

        assert!(rx_samples > 0, "Device1 produced no RX samples — driver failure");
        assert_eq!(tx_errors, 0, "TX errors on Device2: {tx_errors}");
    }
}
