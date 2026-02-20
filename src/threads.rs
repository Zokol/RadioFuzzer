//! RX and TX thread implementations

use crossbeam_channel::{Receiver, Sender};
use num_complex::Complex;
use soapysdr::{RxStream, TxStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::autotest::{evaluate_burst, generate_burst, summarise_results, AutotestConfig};
use crate::detector::{DetectorConfig, Transmission, TransmissionDetector};
use crate::device::IqSample;
use crate::fuzzer::{Fuzzer, FuzzerAction};
use crate::recording::{Recorder, Replayer};
use crate::stats::{analyze_samples, generate_test_tone, DebugStats};

/// Standard RX thread - receives and forwards to channel
pub fn rx_thread(
    mut rx_stream: RxStream<IqSample>,
    tx_channel: Sender<Vec<IqSample>>,
    buffer_size: usize,
    running: Arc<AtomicBool>,
    name: &str,
) {
    let name = name.to_string();
    println!("[{}] RX thread started", name);

    rx_stream
        .activate(None)
        .expect("Failed to activate RX stream");

    while running.load(Ordering::Relaxed) {
        let mut buffer = vec![Complex::new(0.0f32, 0.0f32); buffer_size];

        match rx_stream.read(&mut [&mut buffer], 1_000_000) {
            Ok(len) => {
                buffer.truncate(len);
                if tx_channel.send(buffer).is_err() {
                    break;
                }
            }
            Err(e) => {
                if running.load(Ordering::Relaxed) {
                    eprintln!("[{}] RX error: {}", name, e);
                }
            }
        }
    }

    let _ = rx_stream.deactivate(None);
    println!("[{}] RX thread stopped", name);
}

/// Standard TX thread - receives from channel and transmits
pub fn tx_thread(
    mut tx_stream: TxStream<IqSample>,
    rx_channel: Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    name: &str,
) {
    let name = name.to_string();
    println!("[{}] TX thread started", name);

    tx_stream
        .activate(None)
        .expect("Failed to activate TX stream");

    while running.load(Ordering::Relaxed) {
        match rx_channel.recv_timeout(Duration::from_millis(100)) {
            Ok(buffer) => {
                if let Err(e) = tx_stream.write(&[&buffer], None, false, 1_000_000) {
                    if running.load(Ordering::Relaxed) {
                        eprintln!("[{}] TX error: {}", name, e);
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = tx_stream.deactivate(None);
    println!("[{}] TX thread stopped", name);
}

/// Debug-aware RX thread that collects statistics
pub fn rx_thread_debug(
    mut rx_stream: RxStream<IqSample>,
    tx_channel: Sender<Vec<IqSample>>,
    buffer_size: usize,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    name: &str,
    log_samples: bool,
) {
    let name = name.to_string();
    println!("[{}] RX thread started (debug mode)", name);

    rx_stream
        .activate(None)
        .expect("Failed to activate RX stream");

    while running.load(Ordering::Relaxed) {
        let mut buffer = vec![Complex::new(0.0f32, 0.0f32); buffer_size];

        match rx_stream.read(&mut [&mut buffer], 1_000_000) {
            Ok(len) => {
                buffer.truncate(len);

                // Update statistics
                stats
                    .rx_samples
                    .fetch_add(len as u64, Ordering::Relaxed);
                stats.rx_buffers.fetch_add(1, Ordering::Relaxed);

                // Analyze and update peak
                let (peak, avg, _rms) = analyze_samples(&buffer);
                stats.update_rx_peak(peak);
                stats.update_rx_avg(avg);

                // Log samples if requested
                if log_samples && !buffer.is_empty() {
                    println!(
                        "[{}] Received {} samples, peak={:.4}, avg={:.4}, first 4: {:?}",
                        name,
                        len,
                        peak,
                        avg,
                        &buffer[..buffer.len().min(4)]
                    );
                }

                if tx_channel.send(buffer).is_err() {
                    break;
                }
            }
            Err(e) => {
                stats.rx_errors.fetch_add(1, Ordering::Relaxed);
                if running.load(Ordering::Relaxed) {
                    eprintln!("[{}] RX error: {}", name, e);
                }
            }
        }
    }

    let _ = rx_stream.deactivate(None);
    println!("[{}] RX thread stopped", name);
}

/// Debug-aware TX thread that collects statistics
pub fn tx_thread_debug(
    mut tx_stream: TxStream<IqSample>,
    rx_channel: Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    name: &str,
    log_samples: bool,
) {
    let name = name.to_string();
    println!("[{}] TX thread started (debug mode)", name);

    tx_stream
        .activate(None)
        .expect("Failed to activate TX stream");

    while running.load(Ordering::Relaxed) {
        match rx_channel.recv_timeout(Duration::from_millis(100)) {
            Ok(buffer) => {
                let len = buffer.len();

                // Analyze and update peak
                let (peak, avg, _rms) = analyze_samples(&buffer);
                stats.update_tx_peak(peak);

                // Log samples if requested
                if log_samples && !buffer.is_empty() {
                    println!(
                        "[{}] Transmitting {} samples, peak={:.4}, avg={:.4}, first 4: {:?}",
                        name,
                        len,
                        peak,
                        avg,
                        &buffer[..buffer.len().min(4)]
                    );
                }

                match tx_stream.write(&[&buffer], None, false, 1_000_000) {
                    Ok(_) => {
                        stats
                            .tx_samples
                            .fetch_add(len as u64, Ordering::Relaxed);
                        stats.tx_buffers.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        stats.tx_errors.fetch_add(1, Ordering::Relaxed);
                        if running.load(Ordering::Relaxed) {
                            eprintln!("[{}] TX error: {}", name, e);
                        }
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = tx_stream.deactivate(None);
    println!("[{}] TX thread stopped", name);
}

/// TX thread that generates test tones instead of relaying
pub fn tx_test_tone_thread(
    mut tx_stream: TxStream<IqSample>,
    buffer_size: usize,
    sample_rate: f64,
    tone_freq: f64,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    name: &str,
) {
    let name = name.to_string();
    println!("[{}] TX test tone thread started ({}Hz tone)", name, tone_freq);

    tx_stream
        .activate(None)
        .expect("Failed to activate TX stream");

    let mut phase = 0.0f64;

    while running.load(Ordering::Relaxed) {
        let mut buffer = vec![Complex::new(0.0f32, 0.0f32); buffer_size];
        generate_test_tone(&mut buffer, sample_rate, tone_freq, &mut phase);

        match tx_stream.write(&[&buffer], None, false, 1_000_000) {
            Ok(_) => {
                stats
                    .tx_samples
                    .fetch_add(buffer_size as u64, Ordering::Relaxed);
                stats.tx_buffers.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                stats.tx_errors.fetch_add(1, Ordering::Relaxed);
                if running.load(Ordering::Relaxed) {
                    eprintln!("[{}] TX error: {}", name, e);
                }
            }
        }
    }

    let _ = tx_stream.deactivate(None);
    println!("[{}] TX test tone thread stopped", name);
}

/// RX-only thread for testing reception
pub fn rx_analyze_thread(
    mut rx_stream: RxStream<IqSample>,
    buffer_size: usize,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    name: &str,
    log_samples: bool,
) {
    let name = name.to_string();
    println!("[{}] RX analyze thread started", name);

    rx_stream
        .activate(None)
        .expect("Failed to activate RX stream");

    while running.load(Ordering::Relaxed) {
        let mut buffer = vec![Complex::new(0.0f32, 0.0f32); buffer_size];

        match rx_stream.read(&mut [&mut buffer], 1_000_000) {
            Ok(len) => {
                buffer.truncate(len);

                // Update statistics
                stats
                    .rx_samples
                    .fetch_add(len as u64, Ordering::Relaxed);
                stats.rx_buffers.fetch_add(1, Ordering::Relaxed);

                // Analyze and update peak and average
                let (peak, avg, _rms) = analyze_samples(&buffer);
                stats.update_rx_peak(peak);
                stats.update_rx_avg(avg);

                // Log samples if requested
                if log_samples && !buffer.is_empty() {
                    println!(
                        "[{}] Received {} samples, peak={:.4}, avg={:.4}",
                        name, len, peak, avg
                    );
                }
            }
            Err(e) => {
                stats.rx_errors.fetch_add(1, Ordering::Relaxed);
                if running.load(Ordering::Relaxed) {
                    eprintln!("[{}] RX error: {}", name, e);
                }
            }
        }
    }

    let _ = rx_stream.deactivate(None);
    println!("[{}] RX analyze thread stopped", name);
}

/// RX thread with transmission detection and fuzzing
pub fn rx_fuzz_thread(
    mut rx_stream: RxStream<IqSample>,
    tx_channel: Sender<Transmission>,
    buffer_size: usize,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    detector_config: DetectorConfig,
    fuzzer: Arc<std::sync::Mutex<Fuzzer>>,
    name: &str,
    log_transmissions: bool,
) {
    let name = name.to_string();
    println!("[{}] RX fuzzer thread started", name);

    rx_stream
        .activate(None)
        .expect("Failed to activate RX stream");

    let squelch = detector_config.squelch_threshold;
    let mut detector = TransmissionDetector::new(detector_config);
    let mut buffer = vec![Complex::new(0.0f32, 0.0f32); buffer_size];

    while running.load(Ordering::Relaxed) {
        buffer.resize(buffer_size, Complex::new(0.0f32, 0.0f32));

        match rx_stream.read(&mut [&mut buffer], 1_000_000) {
            Ok(len) => {
                buffer.truncate(len);
                stats.rx_samples.fetch_add(len as u64, Ordering::Relaxed);
                stats.rx_buffers.fetch_add(1, Ordering::Relaxed);

                // Analyze and update power levels for monitoring
                let (peak, avg, _rms) = analyze_samples(&buffer);
                stats.update_rx_peak(peak);
                stats.update_rx_avg(avg);

                // Process samples through detector
                let transmissions = detector.process(&buffer);

                // Process each detected transmission through fuzzer
                for tx in transmissions {
                    // Always print detected transmissions with power info
                    let peak_db = if tx.peak_level > 0.0 { 20.0 * tx.peak_level.log10() } else { -100.0 };
                    let avg_db = if tx.avg_level > 0.0 { 20.0 * tx.avg_level.log10() } else { -100.0 };
                    println!(
                        "[{}] Detected transmission #{}: {} samples, peak={:.4} ({:.1} dB), avg={:.4} ({:.1} dB), squelch={:.4}",
                        name,
                        tx.id,
                        tx.samples.len(),
                        tx.peak_level,
                        peak_db,
                        tx.avg_level,
                        avg_db,
                        squelch
                    );
                    stats.record_transmission_a(tx.samples.len());

                    // Lock scope limited - get action and pending delayed items
                    let (action, delayed) = {
                        let mut fuzzer_guard = fuzzer.lock().unwrap();
                        let action = fuzzer_guard.process(tx);
                        let delayed = fuzzer_guard.poll_delayed();
                        (action, delayed)
                    }; // mutex released here

                    // Now send without holding the mutex
                    match action {
                        FuzzerAction::Pass(tx) | FuzzerAction::Modified(tx) => {
                            if log_transmissions {
                                println!("[{}] Passing transmission #{}", name, tx.id);
                            }
                            if tx_channel.send(tx).is_err() {
                                break;
                            }
                        }
                        FuzzerAction::Drop => {
                            if log_transmissions {
                                println!("[{}] Dropping transmission", name);
                            }
                        }
                        FuzzerAction::Delayed => {
                            if log_transmissions {
                                println!("[{}] Delaying transmission", name);
                            }
                        }
                    }

                    // Send delayed transmissions without holding the mutex
                    for delayed_tx in delayed {
                        if log_transmissions {
                            println!("[{}] Releasing delayed transmission #{}", name, delayed_tx.id);
                        }
                        if tx_channel.send(delayed_tx).is_err() {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                stats.rx_errors.fetch_add(1, Ordering::Relaxed);
                if running.load(Ordering::Relaxed) {
                    eprintln!("[{}] RX error: {}", name, e);
                }
            }
        }
    }

    // Flush any remaining transmission
    if let Some(tx) = detector.flush() {
        let action = {
            let mut fuzzer_guard = fuzzer.lock().unwrap();
            fuzzer_guard.process(tx)
        };
        if let FuzzerAction::Pass(tx) | FuzzerAction::Modified(tx) = action {
            let _ = tx_channel.send(tx);
        }
    }

    let _ = rx_stream.deactivate(None);
    println!("[{}] RX fuzzer thread stopped", name);
}

/// TX thread that transmits complete transmissions (frames)
/// Only activates TX when there are frames to transmit, deactivates after idle timeout
pub fn tx_fuzz_thread(
    mut tx_stream: TxStream<IqSample>,
    rx_channel: Receiver<Transmission>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    name: &str,
    log_transmissions: bool,
    sample_rate: f64,
) {
    let name = name.to_string();
    println!("[{}] TX fuzzer thread started", name);

    // Don't activate stream yet - only activate when we have data to transmit
    let mut stream_active = false;
    let mut last_tx_time: Option<Instant> = None;
    let idle_deactivate = Duration::from_millis(200);

    while running.load(Ordering::Relaxed) {
        match rx_channel.recv_timeout(Duration::from_millis(50)) {
            Ok(transmission) => {
                let len = transmission.samples.len();

                // Activate stream if not already active
                if !stream_active {
                    if let Err(e) = tx_stream.activate(None) {
                        eprintln!("[{}] Failed to activate TX stream: {}", name, e);
                        continue;
                    }
                    stream_active = true;
                    println!("[{}] TX stream activated", name);
                }

                println!(
                    "[{}] Transmitting frame #{}: {} samples ({:.1} ms)",
                    name, transmission.id, len,
                    len as f64 / sample_rate * 1000.0
                );

                // Transmit with retry loop for partial writes.
                // end_of_burst must be false on every intermediate write; only the
                // final chunk of the frame gets true so UHD keeps a continuous RF
                // carrier across all ~65 k-sample chunks.  Setting it true every
                // iteration causes UHD to treat each chunk as a separate burst,
                // producing ~33 ms RF gaps at 2 Msps that make the signal
                // undecodable on a receiver.
                let mut offset = 0;
                let mut chunks = 0usize;
                while offset < len && running.load(Ordering::Relaxed) {
                    // Limit each write to at most 65 536 samples so that is_last
                    // is computed correctly.  Passing the entire remaining slice
                    // lets SoapySDR accept all samples in one call, which would
                    // set end_of_burst=false on the only write → UHD underrun.
                    let chunk_end = (offset + 65_536).min(len);
                    let is_last = chunk_end == len;
                    match tx_stream.write(&[&transmission.samples[offset..chunk_end]], None, is_last, 1_000_000) {
                        Ok(written) => {
                            stats.tx_samples.fetch_add(written as u64, Ordering::Relaxed);
                            offset += written;
                            chunks += 1;
                            if written == 0 {
                                eprintln!("[{}] TX stalled (0 bytes written)", name);
                                break;
                            }
                        }
                        Err(e) => {
                            stats.tx_errors.fetch_add(1, Ordering::Relaxed);
                            if running.load(Ordering::Relaxed) {
                                eprintln!("[{}] TX error: {}", name, e);
                            }
                            break;
                        }
                    }
                }
                stats.tx_buffers.fetch_add(1, Ordering::Relaxed);
                println!(
                    "[{}] Wrote {}/{} samples in {} chunks (EOB on final chunk)",
                    name, offset, len, chunks
                );

                last_tx_time = Some(Instant::now());
                // Do NOT deactivate here - keep stream active for back-to-back transmissions
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // Deactivate stream after idle timeout to avoid transmitting silence/noise
                if stream_active {
                    if last_tx_time.map_or(true, |t| t.elapsed() >= idle_deactivate) {
                        if let Err(e) = tx_stream.deactivate(None) {
                            eprintln!("[{}] Failed to deactivate TX stream: {}", name, e);
                        } else {
                            println!("[{}] TX stream deactivated (idle)", name);
                        }
                        stream_active = false;
                        last_tx_time = None;
                    }
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Ensure stream is deactivated on exit
    if stream_active {
        tx_stream.deactivate(None).ok();
    }
    println!("[{}] TX fuzzer thread stopped", name);
}

/// Delayed transmission poller thread - checks for delayed transmissions and sends them
pub fn delayed_poller_thread(
    fuzzer: Arc<std::sync::Mutex<Fuzzer>>,
    tx_channel: Sender<Transmission>,
    running: Arc<AtomicBool>,
    name: &str,
) {
    let name = name.to_string();
    println!("[{}] Delayed poller thread started", name);

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));

        let mut fuzzer_guard = fuzzer.lock().unwrap();
        for tx in fuzzer_guard.poll_delayed() {
            println!(
                "[{}] Releasing delayed tx #{}: {} samples ({:.1} ms)",
                name, tx.id, tx.samples.len(),
                tx.samples.len() as f64 / 2_000_000.0 * 1000.0
            );
            if tx_channel.send(tx).is_err() {
                break;
            }
        }
    }

    println!("[{}] Delayed poller thread stopped", name);
}

/// RX thread with transmission detection, fuzzing, and recording
#[allow(clippy::too_many_arguments)]
pub fn rx_fuzz_record_thread(
    mut rx_stream: RxStream<IqSample>,
    tx_channel: Sender<Transmission>,
    buffer_size: usize,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    detector_config: DetectorConfig,
    fuzzer: Arc<std::sync::Mutex<Fuzzer>>,
    record_dir: PathBuf,
    name: &str,
    log_transmissions: bool,
) {
    let name = name.to_string();
    println!("[{}] RX fuzzer+recorder thread started", name);
    println!("[{}] Recording to: {:?}", name, record_dir);

    rx_stream
        .activate(None)
        .expect("Failed to activate RX stream");

    let squelch = detector_config.squelch_threshold;
    let mut detector = TransmissionDetector::new(detector_config);
    let mut recorder = Recorder::new(record_dir);
    let mut buffer = vec![Complex::new(0.0f32, 0.0f32); buffer_size];

    while running.load(Ordering::Relaxed) {
        buffer.resize(buffer_size, Complex::new(0.0f32, 0.0f32));

        match rx_stream.read(&mut [&mut buffer], 1_000_000) {
            Ok(len) => {
                buffer.truncate(len);
                stats.rx_samples.fetch_add(len as u64, Ordering::Relaxed);
                stats.rx_buffers.fetch_add(1, Ordering::Relaxed);

                // Analyze and update power levels for monitoring
                let (peak, avg, _rms) = analyze_samples(&buffer);
                stats.update_rx_peak(peak);
                stats.update_rx_avg(avg);

                // Process samples through detector
                let transmissions = detector.process(&buffer);

                // Process each detected transmission
                for tx in transmissions {
                    // Always print detected transmissions with power info
                    let peak_db = if tx.peak_level > 0.0 { 20.0 * tx.peak_level.log10() } else { -100.0 };
                    let avg_db = if tx.avg_level > 0.0 { 20.0 * tx.avg_level.log10() } else { -100.0 };
                    println!(
                        "[{}] Detected transmission #{}: {} samples, peak={:.4} ({:.1} dB), avg={:.4} ({:.1} dB), squelch={:.4}",
                        name,
                        tx.id,
                        tx.samples.len(),
                        tx.peak_level,
                        peak_db,
                        tx.avg_level,
                        avg_db,
                        squelch
                    );

                    // Record the transmission before fuzzing
                    match recorder.record(&tx) {
                        Ok(path) => {
                            if log_transmissions {
                                println!("[{}] Recorded to: {:?}", name, path);
                            }
                        }
                        Err(e) => {
                            eprintln!("[{}] Recording error: {}", name, e);
                        }
                    }

                    // Lock scope limited - get action and pending delayed items
                    let (action, delayed) = {
                        let mut fuzzer_guard = fuzzer.lock().unwrap();
                        let action = fuzzer_guard.process(tx);
                        let delayed = fuzzer_guard.poll_delayed();
                        (action, delayed)
                    }; // mutex released here

                    // Now send without holding the mutex
                    match action {
                        FuzzerAction::Pass(tx) | FuzzerAction::Modified(tx) => {
                            if log_transmissions {
                                println!("[{}] Passing transmission #{}", name, tx.id);
                            }
                            if tx_channel.send(tx).is_err() {
                                break;
                            }
                        }
                        FuzzerAction::Drop => {
                            if log_transmissions {
                                println!("[{}] Dropping transmission", name);
                            }
                        }
                        FuzzerAction::Delayed => {
                            if log_transmissions {
                                println!("[{}] Delaying transmission", name);
                            }
                        }
                    }

                    // Send delayed transmissions without holding the mutex
                    for delayed_tx in delayed {
                        if log_transmissions {
                            println!("[{}] Releasing delayed transmission #{}", name, delayed_tx.id);
                        }
                        if tx_channel.send(delayed_tx).is_err() {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                stats.rx_errors.fetch_add(1, Ordering::Relaxed);
                if running.load(Ordering::Relaxed) {
                    eprintln!("[{}] RX error: {}", name, e);
                }
            }
        }
    }

    // Flush any remaining transmission
    if let Some(tx) = detector.flush() {
        let _ = recorder.record(&tx);
        let action = {
            let mut fuzzer_guard = fuzzer.lock().unwrap();
            fuzzer_guard.process(tx)
        };
        if let FuzzerAction::Pass(tx) | FuzzerAction::Modified(tx) = action {
            let _ = tx_channel.send(tx);
        }
    }

    println!("[{}] Recorded {} transmissions total", name, recorder.count());
    let _ = rx_stream.deactivate(None);
    println!("[{}] RX fuzzer+recorder thread stopped", name);
}

/// Replay thread - replays recorded transmissions through fuzzer to TX
#[allow(clippy::too_many_arguments)]
pub fn replay_thread(
    tx_channel: Sender<Transmission>,
    replay_dir: PathBuf,
    delay_ms: u64,
    loop_replay: bool,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    fuzzer: Arc<std::sync::Mutex<Fuzzer>>,
    name: &str,
    log_transmissions: bool,
) {
    let name = name.to_string();
    println!("[{}] Replay thread started", name);
    println!("[{}] Replaying from: {:?}", name, replay_dir);

    let mut replayer = match Replayer::new(&replay_dir, loop_replay) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] Failed to initialize replayer: {}", name, e);
            return;
        }
    };

    println!("[{}] Found {} recordings", name, replayer.count());

    let delay = Duration::from_millis(delay_ms);

    while running.load(Ordering::Relaxed) && !replayer.is_complete() {
        match replayer.next() {
            Some(Ok(tx)) => {
                if log_transmissions {
                    println!(
                        "[{}] Replaying transmission #{}: {} samples",
                        name,
                        tx.id,
                        tx.samples.len()
                    );
                }

                stats.rx_samples.fetch_add(tx.samples.len() as u64, Ordering::Relaxed);

                // Process through fuzzer
                let mut fuzzer_guard = fuzzer.lock().unwrap();
                match fuzzer_guard.process(tx) {
                    FuzzerAction::Pass(tx) | FuzzerAction::Modified(tx) => {
                        if tx_channel.send(tx).is_err() {
                            break;
                        }
                    }
                    FuzzerAction::Drop => {
                        if log_transmissions {
                            println!("[{}] Dropping replayed transmission", name);
                        }
                    }
                    FuzzerAction::Delayed => {
                        if log_transmissions {
                            println!("[{}] Delaying replayed transmission", name);
                        }
                    }
                }
                drop(fuzzer_guard);

                // Delay between transmissions
                std::thread::sleep(delay);
            }
            Some(Err(e)) => {
                eprintln!("[{}] Error loading recording: {}", name, e);
            }
            None => {
                if !loop_replay {
                    println!("[{}] Replay complete", name);
                    break;
                }
            }
        }
    }

    println!("[{}] Replay thread stopped", name);
}

/// TX thread that transmits transmissions and optionally records them
/// Only activates TX when there are frames to transmit, deactivates after idle timeout
#[allow(clippy::too_many_arguments)]
pub fn tx_fuzz_record_thread(
    mut tx_stream: TxStream<IqSample>,
    rx_channel: Receiver<Transmission>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    record_dir: Option<PathBuf>,
    name: &str,
    log_transmissions: bool,
    sample_rate: f64,
) {
    let name = name.to_string();
    println!("[{}] TX fuzzer thread started", name);

    // Don't activate stream yet - only activate when we have data to transmit
    let mut stream_active = false;
    let mut last_tx_time: Option<Instant> = None;
    let idle_deactivate = Duration::from_millis(200);
    let mut recorder = record_dir.map(Recorder::new);

    while running.load(Ordering::Relaxed) {
        match rx_channel.recv_timeout(Duration::from_millis(50)) {
            Ok(transmission) => {
                let len = transmission.samples.len();

                // Activate stream if not already active
                if !stream_active {
                    if let Err(e) = tx_stream.activate(None) {
                        eprintln!("[{}] Failed to activate TX stream: {}", name, e);
                        continue;
                    }
                    stream_active = true;
                    println!("[{}] TX stream activated", name);
                }

                println!(
                    "[{}] Transmitting frame #{}: {} samples ({:.1} ms)",
                    name, transmission.id, len,
                    len as f64 / sample_rate * 1000.0
                );

                // Optionally record before transmitting
                if let Some(ref mut rec) = recorder {
                    if let Err(e) = rec.record(&transmission) {
                        eprintln!("[{}] Recording error: {}", name, e);
                    }
                }

                // Transmit with retry loop for partial writes.
                // See tx_fuzz_thread for explanation of end_of_burst handling.
                let mut offset = 0;
                while offset < len && running.load(Ordering::Relaxed) {
                    let chunk_end = (offset + 65_536).min(len);
                    let is_last = chunk_end == len;
                    match tx_stream.write(&[&transmission.samples[offset..chunk_end]], None, is_last, 1_000_000) {
                        Ok(written) => {
                            stats.tx_samples.fetch_add(written as u64, Ordering::Relaxed);
                            offset += written;
                            if written == 0 {
                                eprintln!("[{}] TX stalled (0 bytes written)", name);
                                break;
                            }
                        }
                        Err(e) => {
                            stats.tx_errors.fetch_add(1, Ordering::Relaxed);
                            if running.load(Ordering::Relaxed) {
                                eprintln!("[{}] TX error: {}", name, e);
                            }
                            break;
                        }
                    }
                }
                stats.tx_buffers.fetch_add(1, Ordering::Relaxed);
                if log_transmissions {
                    println!("[{}] Wrote {} of {} samples to TX", name, offset, len);
                }

                last_tx_time = Some(Instant::now());
                // Do NOT deactivate here - keep stream active for back-to-back transmissions
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // Deactivate stream after idle timeout to avoid transmitting silence/noise
                if stream_active {
                    if last_tx_time.map_or(true, |t| t.elapsed() >= idle_deactivate) {
                        if let Err(e) = tx_stream.deactivate(None) {
                            eprintln!("[{}] Failed to deactivate TX stream: {}", name, e);
                        } else {
                            println!("[{}] TX stream deactivated (idle)", name);
                        }
                        stream_active = false;
                        last_tx_time = None;
                    }
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Some(rec) = recorder {
        println!("[{}] Recorded {} transmissions", name, rec.count());
    }

    // Ensure stream is deactivated on exit
    if stream_active {
        tx_stream.deactivate(None).ok();
    }
    println!("[{}] TX fuzzer thread stopped", name);
}

/// RX thread with transmission detection that passes samples through in real-time
/// This provides low-latency relay while also detecting transmissions for statistics
#[allow(clippy::too_many_arguments)]
pub fn rx_thread_with_detection(
    mut rx_stream: RxStream<IqSample>,
    tx_channel: Sender<Vec<IqSample>>,
    buffer_size: usize,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    detector_config: DetectorConfig,
    is_channel_a: bool,
    name: &str,
    log_transmissions: bool,
) {
    let name = name.to_string();
    println!("[{}] RX thread started (with transmission detection)", name);

    rx_stream
        .activate(None)
        .expect("Failed to activate RX stream");

    let squelch = detector_config.squelch_threshold;
    let mut detector = TransmissionDetector::new(detector_config);

    while running.load(Ordering::Relaxed) {
        let mut buffer = vec![Complex::new(0.0f32, 0.0f32); buffer_size];

        match rx_stream.read(&mut [&mut buffer], 1_000_000) {
            Ok(len) => {
                buffer.truncate(len);

                // Update statistics
                stats.rx_samples.fetch_add(len as u64, Ordering::Relaxed);
                stats.rx_buffers.fetch_add(1, Ordering::Relaxed);

                // Analyze and update peak and average
                let (peak, avg, _rms) = analyze_samples(&buffer);
                stats.update_rx_peak(peak);
                stats.update_rx_avg(avg);

                // Run transmission detection (non-blocking, just for stats)
                let transmissions = detector.process(&buffer);

                // Record detected transmissions
                for tx in &transmissions {
                    if is_channel_a {
                        stats.record_transmission_a(tx.samples.len());
                    } else {
                        stats.record_transmission_b(tx.samples.len());
                    }

                    if log_transmissions {
                        let peak_db = if tx.peak_level > 0.0 { 20.0 * tx.peak_level.log10() } else { -100.0 };
                        let avg_db = if tx.avg_level > 0.0 { 20.0 * tx.avg_level.log10() } else { -100.0 };
                        println!(
                            "[{}] Detected transmission #{}: {} samples, peak={:.4} ({:.1} dB), avg={:.4} ({:.1} dB), squelch={:.4}",
                            name,
                            tx.id,
                            tx.samples.len(),
                            tx.peak_level,
                            peak_db,
                            tx.avg_level,
                            avg_db,
                            squelch
                        );
                    }
                }

                // Forward samples immediately (low latency)
                if tx_channel.send(buffer).is_err() {
                    break;
                }
            }
            Err(e) => {
                stats.rx_errors.fetch_add(1, Ordering::Relaxed);
                if running.load(Ordering::Relaxed) {
                    eprintln!("[{}] RX error: {}", name, e);
                }
            }
        }
    }

    // Check for any final transmission
    if let Some(tx) = detector.flush() {
        if is_channel_a {
            stats.record_transmission_a(tx.samples.len());
        } else {
            stats.record_transmission_b(tx.samples.len());
        }
        println!(
            "[{}] Final transmission #{}: {} samples",
            name, tx.id, tx.samples.len()
        );
    }

    let _ = rx_stream.deactivate(None);
    println!("[{}] RX thread stopped", name);
}

/// Device3 automated test thread.
///
/// Runs `config.burst_count` TX→RX cycles:
///   1. Transmit a known CW burst at TX frequency (Device1 RX frequency).
///   2. Sleep for `switch_guard_ms` to allow the hardware TX→RX switch.
///   3. Capture `relay_delay_ms + tx_duration_ms` ms of samples (covering the
///      relayed burst which arrives after `relay_delay_ms`).
///   4. Evaluate SNR and cross-correlation against the reference waveform.
///
/// After all bursts, prints an aggregate report, sets `running = false`, and
/// stores `false` in `test_passed` if the overall result is a failure.
pub fn autotest_device3_thread(
    mut tx_stream3: TxStream<IqSample>,
    mut rx_stream3: RxStream<IqSample>,
    config: AutotestConfig,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    test_passed: Arc<std::sync::atomic::AtomicBool>,
) {
    println!(
        "[Dev3] Autotest thread started ({} bursts, {} ms TX, {} ms guard, {} ms relay delay)",
        config.burst_count, config.tx_duration_ms, config.switch_guard_ms, config.relay_delay_ms
    );

    let reference = generate_burst(config.sample_rate, config.tone_freq_hz, config.tx_duration_ms);

    // Capture window: from RX activation (after switch guard) through the end of the
    // relayed burst plus one burst-length of margin.
    let capture_ms = config.relay_delay_ms + config.tx_duration_ms;
    let capture_samples = (capture_ms as f64 * config.sample_rate / 1000.0) as usize;

    println!(
        "[Dev3] Reference: {} samples; capture target: {} samples ({} ms)",
        reference.len(), capture_samples, capture_ms
    );

    let mut results = Vec::with_capacity(config.burst_count as usize);

    for burst_id in 0..config.burst_count {
        if !running.load(Ordering::Relaxed) {
            println!("[Dev3] Aborted before burst {}", burst_id);
            break;
        }

        println!("[Dev3] Burst {}/{}: TX phase...", burst_id + 1, config.burst_count);

        // TX phase
        if let Err(e) = tx_stream3.activate(None) {
            eprintln!("[Dev3] Failed to activate TX stream: {}", e);
            break;
        }

        let mut offset = 0;
        while offset < reference.len() && running.load(Ordering::Relaxed) {
            match tx_stream3.write(&[&reference[offset..]], None, true, 1_000_000) {
                Ok(written) => {
                    stats.tx_samples.fetch_add(written as u64, Ordering::Relaxed);
                    offset += written;
                    if written == 0 {
                        eprintln!("[Dev3] TX stalled (0 bytes written)");
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[Dev3] TX error: {}", e);
                    break;
                }
            }
        }
        tx_stream3.deactivate(None).ok();

        // Switch guard: allow hardware TX→RX transition
        thread::sleep(Duration::from_millis(config.switch_guard_ms));

        // RX phase
        println!(
            "[Dev3] Burst {}/{}: RX phase ({} ms)...",
            burst_id + 1, config.burst_count, capture_ms
        );

        if let Err(e) = rx_stream3.activate(None) {
            eprintln!("[Dev3] Failed to activate RX stream: {}", e);
            break;
        }

        let mut captured: Vec<IqSample> = Vec::with_capacity(capture_samples);
        let mut rx_buf = vec![Complex::new(0.0f32, 0.0f32); config.buffer_size];

        while captured.len() < capture_samples && running.load(Ordering::Relaxed) {
            let need = capture_samples - captured.len();
            let read_size = need.min(config.buffer_size);
            rx_buf.resize(read_size, Complex::new(0.0f32, 0.0f32));

            match rx_stream3.read(&mut [&mut rx_buf], 1_000_000) {
                Ok(len) => {
                    stats.rx_samples.fetch_add(len as u64, Ordering::Relaxed);
                    captured.extend_from_slice(&rx_buf[..len]);
                }
                Err(e) => {
                    if running.load(Ordering::Relaxed) {
                        eprintln!("[Dev3] RX error: {}", e);
                    }
                    break;
                }
            }
        }
        rx_stream3.deactivate(None).ok();

        // Evaluate burst
        let result = evaluate_burst(&reference, &captured, &config, burst_id);
        let status = if result.passed { "PASS" } else { "FAIL" };
        println!(
            "[Dev3] Burst {:>2}: {} | SNR={:.1} dB, xcorr={:.3}, peak={:.4}, noise={:.4}",
            burst_id, status, result.snr_db, result.xcorr_peak, result.peak_level, result.noise_floor
        );
        results.push(result);
    }

    // Aggregate report
    let report = summarise_results(&results);
    println!("\n=== Autotest Report ===");
    println!("Bursts:     {}/{} passed", report.passed_bursts, report.total_bursts);
    println!("Mean SNR:   {:.1} dB", report.mean_snr_db);
    println!("Min SNR:    {:.1} dB", report.min_snr_db);
    println!("Mean xcorr: {:.3}", report.mean_xcorr);
    println!("Result:     {}", if report.overall_pass { "PASS" } else { "FAIL" });
    println!("=======================\n");

    if !report.overall_pass {
        test_passed.store(false, Ordering::Relaxed);
    }

    // Signal relay threads to stop
    running.store(false, Ordering::Relaxed);
}
