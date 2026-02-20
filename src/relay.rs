//! Relay orchestration and main loop

use crossbeam_channel::bounded;
use soapysdr::{RxStream, TxStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::autotest::AutotestConfig;
use crate::config::Args;
use crate::detector::{DetectorConfig, Transmission};
use crate::device::IqSample;
use crate::fuzzer::{parse_strategy, Fuzzer, FuzzerStrategy};
use crate::stats::{print_stats, DebugStats};
use crate::threads::*;

/// Run the bidirectional relay with the given streams
pub fn run_relay(
    args: Args,
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Set up graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    ctrlc::set_handler(move || {
        println!("\nShutting down...");
        running_clone.store(false, Ordering::Relaxed);
    })?;

    // Print mode information
    print_mode_info(&args);

    let buffer_size = args.buffer_size;
    let sample_rate = args.sample_rate;
    let stats = Arc::new(DebugStats::new());
    let start_time = Instant::now();

    // Create detector config (used in all modes now)
    let detector_config = DetectorConfig {
        squelch_threshold: args.squelch,
        hangtime_samples: args.hangtime_samples(),
        min_transmission_samples: args.min_tx_samples(),
        max_transmission_samples: args.max_tx_samples(),
    };

    // Spawn threads based on mode
    let handles = if args.fuzz {
        // Fuzz mode: use transmission-based channels with fuzzing
        spawn_fuzz_threads(&args, rx_stream1, tx_stream1, rx_stream2, tx_stream2, running.clone(), stats.clone())
    } else {
        // Normal mode: use IQ sample channels with transmission detection for stats
        let (tx_chan_1to2, rx_chan_1to2) = bounded::<Vec<IqSample>>(args.channel_depth);
        let (tx_chan_2to1, rx_chan_2to1) = bounded::<Vec<IqSample>>(args.channel_depth);

        spawn_threads_with_detection(
            &args,
            rx_stream1,
            tx_stream1,
            rx_stream2,
            tx_stream2,
            tx_chan_1to2,
            rx_chan_1to2,
            tx_chan_2to1,
            rx_chan_2to1,
            running.clone(),
            stats.clone(),
            buffer_size,
            detector_config,
        )
    };

    // Always print statistics periodically (transmission detection is always active)
    run_stats_loop(&args, &running, &stats, start_time, sample_rate);

    // Wait for all threads to finish
    for (i, handle) in handles.into_iter().enumerate() {
        handle.join().unwrap_or_else(|_| eprintln!("Thread {} panicked", i));
    }

    println!("RadioFuzz stopped.");
    Ok(())
}

fn print_mode_info(args: &Args) {
    println!("\nStarting bidirectional relay...");
    if args.single_device {
        println!(
            "Mode: Single device, Channel {} <-> Channel {}",
            args.rx_chan1,
            args.rx_chan2.unwrap_or(1)
        );
    } else {
        println!("Mode: Dual device");
    }

    // Always show transmission detection settings
    println!("Transmission detection: ENABLED");
    println!("  Squelch threshold: {:.3}", args.squelch);
    println!("  Hangtime: {} ms", args.hangtime_ms);
    println!("  Min transmission: {} ms", args.min_tx_ms);
    println!("  Max transmission: {} ms", args.max_tx_ms);

    if args.fuzz {
        println!("Fuzzer mode: ENABLED");
        println!("  Strategy: {}", args.fuzz_strategy);
        if let Some(ref dir) = args.record_dir {
            println!("  Recording to: {}", dir);
        }
        if let Some(ref dir) = args.replay_dir {
            println!("  Replaying from: {}", dir);
        }
    } else if args.unidirectional {
        if args.reverse {
            println!("Direction: Unidirectional (Device2/ChB RX -> Device1/ChA TX)");
        } else {
            println!("Direction: Unidirectional (Device1/ChA RX -> Device2/ChB TX)");
        }
    } else {
        println!("Direction: Bidirectional");
    }

    if args.debug {
        println!("Debug mode: ENABLED");
        if args.test_tx_only {
            println!("  Test TX only: generating {}Hz test tone", args.test_tone_freq);
        } else if args.test_rx_only {
            println!("  Test RX only: receiving and analyzing");
        }
        if args.log_samples {
            println!("  Sample logging: ENABLED (verbose!)");
        }
        if args.debug_duration > 0 {
            println!("  Duration: {} seconds", args.debug_duration);
        }
    }
    println!("Press Ctrl+C to stop\n");
}

/// Spawn threads for fuzzer mode with transmission detection
#[allow(clippy::too_many_arguments)]
fn spawn_fuzz_threads(
    args: &Args,
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
) -> Vec<thread::JoinHandle<()>> {
    let buffer_size = args.buffer_size;
    let log_transmissions = args.log_samples;
    let sample_rate = args.sample_rate;

    // Parse fuzzer strategy
    let strategy = parse_strategy(&args.fuzz_strategy)
        .unwrap_or_else(|e| {
            eprintln!("Warning: {}, using passthrough", e);
            crate::fuzzer::FuzzerStrategy::Passthrough
        });

    // Create detector config from args
    let detector_config = DetectorConfig {
        squelch_threshold: args.squelch,
        hangtime_samples: args.hangtime_samples(),
        min_transmission_samples: args.min_tx_samples(),
        max_transmission_samples: args.max_tx_samples(),
    };

    // Create shared fuzzer instances
    let fuzzer1 = Arc::new(std::sync::Mutex::new(Fuzzer::new(strategy.clone())));
    let fuzzer2 = Arc::new(std::sync::Mutex::new(Fuzzer::new(strategy)));

    // Create transmission channels
    let (tx_chan_1to2, rx_chan_1to2) = bounded::<Transmission>(args.channel_depth);
    let (tx_chan_2to1, rx_chan_2to1) = bounded::<Transmission>(args.channel_depth);

    // Check for recording/replay options
    let record_dir = args.record_dir.clone().map(std::path::PathBuf::from);
    let replay_dir = args.replay_dir.clone().map(std::path::PathBuf::from);
    let replay_delay_ms = args.replay_delay_ms;
    let replay_loop = args.replay_loop;

    let mut handles = Vec::new();

    // Handle replay mode - replays recorded transmissions instead of live RX
    if let Some(ref replay_path) = replay_dir {
        // Replay mode: replay recorded transmissions to TX
        let r1 = running.clone();
        let r2 = running.clone();
        let r3 = running.clone();
        let s1 = stats.clone();
        let s2 = stats.clone();
        let f1 = fuzzer1.clone();
        let replay_path_clone = replay_path.clone();
        let tx_chan_for_replay = tx_chan_1to2.clone();
        let tx_chan_for_poller = tx_chan_1to2;

        handles.push(thread::spawn(move || {
            replay_thread(tx_chan_for_replay, replay_path_clone, replay_delay_ms, replay_loop, r1, s1, f1, "Replay", log_transmissions);
        }));

        handles.push(thread::spawn(move || {
            tx_fuzz_thread(tx_stream2, rx_chan_1to2, r2, s2, "ChB-TX-Fuzz", log_transmissions, sample_rate);
        }));

        // Delayed poller
        handles.push(thread::spawn(move || {
            delayed_poller_thread(fuzzer1, tx_chan_for_poller, r3, "Delay-Replay");
        }));

        return handles;
    }

    if args.unidirectional {
        if args.reverse {
            // ChB RX -> Fuzz -> ChA TX
            let r1 = running.clone();
            let r2 = running.clone();
            let r3 = running.clone();
            let s1 = stats.clone();
            let s2 = stats.clone();
            let dc = detector_config.clone();
            let f2 = fuzzer2.clone();
            let tx_chan_for_rx = tx_chan_2to1.clone();
            let tx_chan_for_poller = tx_chan_2to1;

            if let Some(ref rec_dir) = record_dir {
                let rec_path = rec_dir.clone();
                handles.push(thread::spawn(move || {
                    rx_fuzz_record_thread(rx_stream2, tx_chan_for_rx, buffer_size, r1, s1, dc, f2, rec_path, "ChB-RX-Fuzz", log_transmissions);
                }));
            } else {
                handles.push(thread::spawn(move || {
                    rx_fuzz_thread(rx_stream2, tx_chan_for_rx, buffer_size, r1, s1, dc, f2, "ChB-RX-Fuzz", log_transmissions);
                }));
            }

            handles.push(thread::spawn(move || {
                tx_fuzz_thread(tx_stream1, rx_chan_2to1, r2, s2, "ChA-TX-Fuzz", log_transmissions, sample_rate);
            }));

            // Delayed poller for ChB->ChA direction
            handles.push(thread::spawn(move || {
                delayed_poller_thread(fuzzer2, tx_chan_for_poller, r3, "Delay-B2A");
            }));
        } else {
            // ChA RX -> Fuzz -> ChB TX
            let r1 = running.clone();
            let r2 = running.clone();
            let r3 = running.clone();
            let s1 = stats.clone();
            let s2 = stats.clone();
            let dc = detector_config.clone();
            let f1 = fuzzer1.clone();
            let tx_chan_for_rx = tx_chan_1to2.clone();
            let tx_chan_for_poller = tx_chan_1to2;

            if let Some(ref rec_dir) = record_dir {
                let rec_path = rec_dir.clone();
                handles.push(thread::spawn(move || {
                    rx_fuzz_record_thread(rx_stream1, tx_chan_for_rx, buffer_size, r1, s1, dc, f1, rec_path, "ChA-RX-Fuzz", log_transmissions);
                }));
            } else {
                handles.push(thread::spawn(move || {
                    rx_fuzz_thread(rx_stream1, tx_chan_for_rx, buffer_size, r1, s1, dc, f1, "ChA-RX-Fuzz", log_transmissions);
                }));
            }

            handles.push(thread::spawn(move || {
                tx_fuzz_thread(tx_stream2, rx_chan_1to2, r2, s2, "ChB-TX-Fuzz", log_transmissions, sample_rate);
            }));

            // Delayed poller
            handles.push(thread::spawn(move || {
                delayed_poller_thread(fuzzer1, tx_chan_for_poller, r3, "Delay-A2B");
            }));
        }
    } else {
        // Bidirectional fuzzing
        let (r1, r2, r3, r4, r5, r6) = (
            running.clone(), running.clone(), running.clone(),
            running.clone(), running.clone(), running.clone(),
        );
        let (s1, s2, s3, s4) = (stats.clone(), stats.clone(), stats.clone(), stats.clone());
        let dc1 = detector_config.clone();
        let dc2 = detector_config;
        let f1 = fuzzer1.clone();
        let f2 = fuzzer2.clone();

        // Clone channels for pollers first
        let tx_chan_1to2_for_poller = tx_chan_1to2.clone();
        let tx_chan_2to1_for_poller = tx_chan_2to1.clone();

        // ChA RX -> Fuzz -> ChB TX
        if let Some(ref rec_dir) = record_dir {
            let rec_path_a = rec_dir.join("channel_a");
            handles.push(thread::spawn(move || {
                rx_fuzz_record_thread(rx_stream1, tx_chan_1to2, buffer_size, r1, s1, dc1, f1, rec_path_a, "ChA-RX-Fuzz", log_transmissions);
            }));
        } else {
            handles.push(thread::spawn(move || {
                rx_fuzz_thread(rx_stream1, tx_chan_1to2, buffer_size, r1, s1, dc1, f1, "ChA-RX-Fuzz", log_transmissions);
            }));
        }

        handles.push(thread::spawn(move || {
            tx_fuzz_thread(tx_stream2, rx_chan_1to2, r2, s2, "ChB-TX-Fuzz", log_transmissions, sample_rate);
        }));

        // ChB RX -> Fuzz -> ChA TX
        if let Some(ref rec_dir) = record_dir {
            let rec_path_b = rec_dir.join("channel_b");
            handles.push(thread::spawn(move || {
                rx_fuzz_record_thread(rx_stream2, tx_chan_2to1, buffer_size, r3, s3, dc2, f2, rec_path_b, "ChB-RX-Fuzz", log_transmissions);
            }));
        } else {
            handles.push(thread::spawn(move || {
                rx_fuzz_thread(rx_stream2, tx_chan_2to1, buffer_size, r3, s3, dc2, f2, "ChB-RX-Fuzz", log_transmissions);
            }));
        }

        handles.push(thread::spawn(move || {
            tx_fuzz_thread(tx_stream1, rx_chan_2to1, r4, s4, "ChA-TX-Fuzz", log_transmissions, sample_rate);
        }));

        // Delayed pollers for both directions
        handles.push(thread::spawn(move || {
            delayed_poller_thread(fuzzer1, tx_chan_1to2_for_poller, r5, "Delay-A2B");
        }));

        handles.push(thread::spawn(move || {
            delayed_poller_thread(fuzzer2, tx_chan_2to1_for_poller, r6, "Delay-B2A");
        }));
    }

    handles
}

#[allow(clippy::too_many_arguments)]
fn spawn_threads_with_detection(
    args: &Args,
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    tx_chan_1to2: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_1to2: crossbeam_channel::Receiver<Vec<IqSample>>,
    tx_chan_2to1: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_2to1: crossbeam_channel::Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    buffer_size: usize,
    detector_config: DetectorConfig,
) -> Vec<thread::JoinHandle<()>> {
    let log_transmissions = args.log_samples;

    if args.debug && args.test_tx_only {
        spawn_tx_only_threads(tx_stream1, tx_stream2, running, stats, buffer_size, args.sample_rate, args.test_tone_freq)
    } else if args.debug && args.test_rx_only {
        spawn_rx_only_threads_with_detection(rx_stream1, rx_stream2, running, stats, buffer_size, detector_config, log_transmissions)
    } else if args.unidirectional {
        spawn_unidirectional_with_detection(
            rx_stream1, tx_stream1, rx_stream2, tx_stream2,
            tx_chan_1to2, rx_chan_1to2, tx_chan_2to1, rx_chan_2to1,
            running, stats, buffer_size, detector_config, args.reverse, log_transmissions,
        )
    } else {
        spawn_bidirectional_with_detection(
            rx_stream1, tx_stream1, rx_stream2, tx_stream2,
            tx_chan_1to2, rx_chan_1to2, tx_chan_2to1, rx_chan_2to1,
            running, stats, buffer_size, detector_config, log_transmissions,
        )
    }
}

fn spawn_rx_only_threads_with_detection(
    rx_stream1: RxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    buffer_size: usize,
    detector_config: DetectorConfig,
    log_transmissions: bool,
) -> Vec<thread::JoinHandle<()>> {
    let running1 = running.clone();
    let running2 = running;
    let stats1 = stats.clone();
    let stats2 = stats;
    let dc1 = detector_config.clone();
    let dc2 = detector_config;

    // Create dummy channels that we won't use (RX only mode)
    let (tx_dummy1, _rx_dummy1) = bounded::<Vec<IqSample>>(1);
    let (tx_dummy2, _rx_dummy2) = bounded::<Vec<IqSample>>(1);

    vec![
        thread::spawn(move || {
            rx_thread_with_detection(rx_stream1, tx_dummy1, buffer_size, running1, stats1, dc1, true, "ChA-RX", log_transmissions);
        }),
        thread::spawn(move || {
            rx_thread_with_detection(rx_stream2, tx_dummy2, buffer_size, running2, stats2, dc2, false, "ChB-RX", log_transmissions);
        }),
    ]
}

#[allow(clippy::too_many_arguments)]
fn spawn_unidirectional_with_detection(
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    tx_chan_1to2: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_1to2: crossbeam_channel::Receiver<Vec<IqSample>>,
    tx_chan_2to1: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_2to1: crossbeam_channel::Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    buffer_size: usize,
    detector_config: DetectorConfig,
    reverse: bool,
    log_transmissions: bool,
) -> Vec<thread::JoinHandle<()>> {
    let r1 = running.clone();
    let r2 = running;
    let s1 = stats.clone();
    let s2 = stats;

    if reverse {
        vec![
            thread::spawn(move || {
                rx_thread_with_detection(rx_stream2, tx_chan_2to1, buffer_size, r1, s1, detector_config, false, "ChB-RX", log_transmissions);
            }),
            thread::spawn(move || {
                tx_thread_debug(tx_stream1, rx_chan_2to1, r2, s2, "ChA-TX", log_transmissions);
            }),
        ]
    } else {
        vec![
            thread::spawn(move || {
                rx_thread_with_detection(rx_stream1, tx_chan_1to2, buffer_size, r1, s1, detector_config, true, "ChA-RX", log_transmissions);
            }),
            thread::spawn(move || {
                tx_thread_debug(tx_stream2, rx_chan_1to2, r2, s2, "ChB-TX", log_transmissions);
            }),
        ]
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_bidirectional_with_detection(
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    tx_chan_1to2: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_1to2: crossbeam_channel::Receiver<Vec<IqSample>>,
    tx_chan_2to1: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_2to1: crossbeam_channel::Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    buffer_size: usize,
    detector_config: DetectorConfig,
    log_transmissions: bool,
) -> Vec<thread::JoinHandle<()>> {
    let (r1, r2, r3, r4) = (running.clone(), running.clone(), running.clone(), running);
    let (s1, s2, s3, s4) = (stats.clone(), stats.clone(), stats.clone(), stats);
    let dc1 = detector_config.clone();
    let dc2 = detector_config;

    vec![
        thread::spawn(move || {
            rx_thread_with_detection(rx_stream1, tx_chan_1to2, buffer_size, r1, s1, dc1, true, "ChA-RX", log_transmissions);
        }),
        thread::spawn(move || {
            tx_thread_debug(tx_stream2, rx_chan_1to2, r2, s2, "ChB-TX", log_transmissions);
        }),
        thread::spawn(move || {
            rx_thread_with_detection(rx_stream2, tx_chan_2to1, buffer_size, r3, s3, dc2, false, "ChB-RX", log_transmissions);
        }),
        thread::spawn(move || {
            tx_thread_debug(tx_stream1, rx_chan_2to1, r4, s4, "ChA-TX", log_transmissions);
        }),
    ]
}

fn spawn_tx_only_threads(
    tx_stream1: TxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    buffer_size: usize,
    sample_rate: f64,
    tone_freq: f64,
) -> Vec<thread::JoinHandle<()>> {
    let running1 = running.clone();
    let running2 = running;
    let stats1 = stats.clone();
    let stats2 = stats;

    vec![
        thread::spawn(move || {
            tx_test_tone_thread(tx_stream1, buffer_size, sample_rate, tone_freq, running1, stats1, "ChA-TX");
        }),
        thread::spawn(move || {
            tx_test_tone_thread(tx_stream2, buffer_size, sample_rate, tone_freq, running2, stats2, "ChB-TX");
        }),
    ]
}

fn spawn_rx_only_threads(
    rx_stream1: RxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    buffer_size: usize,
    log_samples: bool,
) -> Vec<thread::JoinHandle<()>> {
    let running1 = running.clone();
    let running2 = running;
    let stats1 = stats.clone();
    let stats2 = stats;

    vec![
        thread::spawn(move || {
            rx_analyze_thread(rx_stream1, buffer_size, running1, stats1, "ChA-RX", log_samples);
        }),
        thread::spawn(move || {
            rx_analyze_thread(rx_stream2, buffer_size, running2, stats2, "ChB-RX", log_samples);
        }),
    ]
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn spawn_debug_relay_threads(
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    tx_chan_1to2: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_1to2: crossbeam_channel::Receiver<Vec<IqSample>>,
    tx_chan_2to1: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_2to1: crossbeam_channel::Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    buffer_size: usize,
    log_samples: bool,
) -> Vec<thread::JoinHandle<()>> {
    let (r1, r2, r3, r4) = (running.clone(), running.clone(), running.clone(), running);
    let (s1, s2, s3, s4) = (stats.clone(), stats.clone(), stats.clone(), stats);

    vec![
        thread::spawn(move || {
            rx_thread_debug(rx_stream1, tx_chan_1to2, buffer_size, r1, s1, "ChA-RX", log_samples);
        }),
        thread::spawn(move || {
            tx_thread_debug(tx_stream2, rx_chan_1to2, r2, s2, "ChB-TX", log_samples);
        }),
        thread::spawn(move || {
            rx_thread_debug(rx_stream2, tx_chan_2to1, buffer_size, r3, s3, "ChB-RX", log_samples);
        }),
        thread::spawn(move || {
            tx_thread_debug(tx_stream1, rx_chan_2to1, r4, s4, "ChA-TX", log_samples);
        }),
    ]
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn spawn_unidirectional_debug_threads(
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    tx_chan_1to2: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_1to2: crossbeam_channel::Receiver<Vec<IqSample>>,
    tx_chan_2to1: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_2to1: crossbeam_channel::Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    buffer_size: usize,
    log_samples: bool,
    reverse: bool,
) -> Vec<thread::JoinHandle<()>> {
    let r1 = running.clone();
    let r2 = running;
    let s1 = stats.clone();
    let s2 = stats;

    if reverse {
        vec![
            thread::spawn(move || {
                rx_thread_debug(rx_stream2, tx_chan_2to1, buffer_size, r1, s1, "ChB-RX", log_samples);
            }),
            thread::spawn(move || {
                tx_thread_debug(tx_stream1, rx_chan_2to1, r2, s2, "ChA-TX", log_samples);
            }),
        ]
    } else {
        vec![
            thread::spawn(move || {
                rx_thread_debug(rx_stream1, tx_chan_1to2, buffer_size, r1, s1, "ChA-RX", log_samples);
            }),
            thread::spawn(move || {
                tx_thread_debug(tx_stream2, rx_chan_1to2, r2, s2, "ChB-TX", log_samples);
            }),
        ]
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_unidirectional_threads(
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    tx_chan_1to2: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_1to2: crossbeam_channel::Receiver<Vec<IqSample>>,
    tx_chan_2to1: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_2to1: crossbeam_channel::Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    buffer_size: usize,
    reverse: bool,
) -> Vec<thread::JoinHandle<()>> {
    let r1 = running.clone();
    let r2 = running;

    if reverse {
        vec![
            thread::spawn(move || {
                rx_thread(rx_stream2, tx_chan_2to1, buffer_size, r1, "ChB-RX");
            }),
            thread::spawn(move || {
                tx_thread(tx_stream1, rx_chan_2to1, r2, "ChA-TX");
            }),
        ]
    } else {
        vec![
            thread::spawn(move || {
                rx_thread(rx_stream1, tx_chan_1to2, buffer_size, r1, "ChA-RX");
            }),
            thread::spawn(move || {
                tx_thread(tx_stream2, rx_chan_1to2, r2, "ChB-TX");
            }),
        ]
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_normal_relay_threads(
    rx_stream1: RxStream<IqSample>,
    tx_stream1: TxStream<IqSample>,
    rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    tx_chan_1to2: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_1to2: crossbeam_channel::Receiver<Vec<IqSample>>,
    tx_chan_2to1: crossbeam_channel::Sender<Vec<IqSample>>,
    rx_chan_2to1: crossbeam_channel::Receiver<Vec<IqSample>>,
    running: Arc<AtomicBool>,
    buffer_size: usize,
) -> Vec<thread::JoinHandle<()>> {
    let (r1, r2, r3, r4) = (running.clone(), running.clone(), running.clone(), running);

    vec![
        thread::spawn(move || {
            rx_thread(rx_stream1, tx_chan_1to2, buffer_size, r1, "ChA-RX");
        }),
        thread::spawn(move || {
            tx_thread(tx_stream2, rx_chan_1to2, r2, "ChB-TX");
        }),
        thread::spawn(move || {
            rx_thread(rx_stream2, tx_chan_2to1, buffer_size, r3, "ChB-RX");
        }),
        thread::spawn(move || {
            tx_thread(tx_stream1, rx_chan_2to1, r4, "ChA-TX");
        }),
    ]
}

fn run_stats_loop(
    args: &Args,
    running: &Arc<AtomicBool>,
    stats: &Arc<DebugStats>,
    start_time: Instant,
    sample_rate: f64,
) {
    let stats_interval = Duration::from_secs(args.stats_interval);
    let debug_duration = if args.debug_duration > 0 {
        Some(Duration::from_secs(args.debug_duration))
    } else {
        None
    };
    let squelch = args.squelch;

    let mut last_stats_time = Instant::now();

    while running.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(100));

        if last_stats_time.elapsed() >= stats_interval {
            print_stats(stats, start_time.elapsed(), sample_rate, squelch);
            stats.reset_peaks();
            last_stats_time = Instant::now();
        }

        if let Some(duration) = debug_duration {
            if start_time.elapsed() >= duration {
                println!(
                    "\nDebug duration reached ({} seconds), stopping...",
                    args.debug_duration
                );
                running.store(false, Ordering::Relaxed);
            }
        }
    }

    println!("\n=== Final Statistics ===");
    print_stats(stats, start_time.elapsed(), sample_rate, squelch);
}

// ---------------------------------------------------------------------------
// Automated test mode
// ---------------------------------------------------------------------------

/// Run the automated test relay.
///
/// Device1 RX receives the Device3 test burst and feeds it through a
/// `FixedDelay` fuzzer to Device2 TX.  Device3 (half-duplex) transmits the
/// burst then switches to RX to capture what comes back, then evaluates
/// signal quality.
///
/// `tx_stream1` and `rx_stream2` are unused in this mode (the relay is
/// unidirectional: Device1 RX → Device2 TX).  They are accepted by value so
/// the caller does not need to know the internal routing.
pub fn run_autotest(
    args: Args,
    rx_stream1: RxStream<IqSample>,
    _tx_stream1: TxStream<IqSample>,
    _rx_stream2: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    rx_stream3: RxStream<IqSample>,
    tx_stream3: TxStream<IqSample>,
) -> Result<(), Box<dyn std::error::Error>> {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    ctrlc::set_handler(move || {
        println!("\nShutting down autotest...");
        running_clone.store(false, Ordering::Relaxed);
    })?;

    println!("\nStarting automated test mode...");
    println!("  Bursts:        {}", args.autotest_bursts);
    println!("  TX duration:   {} ms", args.autotest_tx_ms);
    println!("  Switch guard:  {} ms", args.autotest_switch_guard_ms);
    println!("  Relay delay:   {} ms", args.autotest_relay_delay_ms);
    println!("  SNR threshold: {:.1} dB", args.autotest_snr_threshold_db);
    println!("  Tone offset:   {:.0} Hz", args.autotest_tone_freq_hz);
    println!("  Device3:       {}", args.device3);
    println!("Press Ctrl+C to abort\n");

    let stats = Arc::new(DebugStats::new());
    let start_time = Instant::now();
    let sample_rate = args.sample_rate;

    let detector_config = DetectorConfig {
        squelch_threshold: args.squelch,
        hangtime_samples: args.hangtime_samples(),
        min_transmission_samples: args.min_tx_samples(),
        max_transmission_samples: args.max_tx_samples(),
    };

    let autotest_config = AutotestConfig {
        tx_duration_ms: args.autotest_tx_ms,
        switch_guard_ms: args.autotest_switch_guard_ms,
        relay_delay_ms: args.autotest_relay_delay_ms,
        burst_count: args.autotest_bursts,
        snr_threshold_db: args.autotest_snr_threshold_db,
        tone_freq_hz: args.autotest_tone_freq_hz,
        sample_rate: args.sample_rate,
        buffer_size: args.buffer_size,
    };

    // Shared flag: autotest_device3_thread sets this to false on overall failure
    let test_passed = Arc::new(AtomicBool::new(true));

    let handles = spawn_autotest_threads(
        &args,
        rx_stream1,
        tx_stream2,
        rx_stream3,
        tx_stream3,
        running.clone(),
        stats.clone(),
        detector_config,
        autotest_config,
        test_passed.clone(),
    );

    // Stats loop runs until autotest_device3_thread sets running = false
    run_stats_loop(&args, &running, &stats, start_time, sample_rate);

    for (i, handle) in handles.into_iter().enumerate() {
        handle.join().unwrap_or_else(|_| eprintln!("Thread {} panicked", i));
    }

    println!("Autotest complete.");

    if !test_passed.load(Ordering::Relaxed) {
        std::process::exit(1);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_autotest_threads(
    args: &Args,
    rx_stream1: RxStream<IqSample>,
    tx_stream2: TxStream<IqSample>,
    rx_stream3: RxStream<IqSample>,
    tx_stream3: TxStream<IqSample>,
    running: Arc<AtomicBool>,
    stats: Arc<DebugStats>,
    detector_config: DetectorConfig,
    autotest_config: AutotestConfig,
    test_passed: Arc<AtomicBool>,
) -> Vec<thread::JoinHandle<()>> {
    let buffer_size = args.buffer_size;
    let sample_rate = args.sample_rate;
    let log_transmissions = args.log_samples;

    // Relay uses FixedDelay regardless of --fuzz-strategy
    let relay_delay = args.autotest_relay_delay();
    let fuzzer1 = Arc::new(std::sync::Mutex::new(
        Fuzzer::new(FuzzerStrategy::FixedDelay(relay_delay))
    ));

    // Transmission channel: Device1 RX → delayed_poller → Device2 TX
    let (tx_chan_1to2, rx_chan_1to2) = bounded::<Transmission>(args.channel_depth);
    let tx_chan_for_poller = tx_chan_1to2.clone();

    let (r1, r2, r3, r4) = (
        running.clone(), running.clone(), running.clone(), running,
    );
    // delayed_poller_thread does not take stats; we need 3 stats clones
    let s1 = stats.clone();   // rx_fuzz_thread (Device1 RX)
    let s2 = stats.clone();   // tx_fuzz_thread (Device2 TX)
    let s3 = stats;           // autotest_device3_thread (Device3)
    let f1 = fuzzer1.clone();

    vec![
        // Device1 RX: receive test burst, delay via fuzzer
        thread::Builder::new()
            .name("ChA-RX-Fuzz".into())
            .spawn(move || {
                rx_fuzz_thread(
                    rx_stream1, tx_chan_1to2, buffer_size,
                    r1, s1, detector_config, f1,
                    "ChA-RX-Fuzz", log_transmissions,
                );
            })
            .expect("failed to spawn ChA-RX-Fuzz"),

        // Device2 TX: transmit delayed burst
        thread::Builder::new()
            .name("ChB-TX-Fuzz".into())
            .spawn(move || {
                tx_fuzz_thread(
                    tx_stream2, rx_chan_1to2,
                    r2, s2, "ChB-TX-Fuzz", log_transmissions, sample_rate,
                );
            })
            .expect("failed to spawn ChB-TX-Fuzz"),

        // Delayed poller: releases transmissions after relay_delay_ms
        thread::Builder::new()
            .name("Delay-A2B".into())
            .spawn(move || {
                delayed_poller_thread(fuzzer1, tx_chan_for_poller, r3, "Delay-A2B");
            })
            .expect("failed to spawn Delay-A2B"),

        // Device3: transmit burst, capture relay output, evaluate
        thread::Builder::new()
            .name("Dev3-Autotest".into())
            .spawn(move || {
                autotest_device3_thread(
                    tx_stream3, rx_stream3, autotest_config,
                    r4, s3, test_passed,
                );
            })
            .expect("failed to spawn Dev3-Autotest"),
    ]
}
