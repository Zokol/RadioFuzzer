//! RadioFuzz - Bidirectional SDR IQ Relay
//!
//! This application reads IQ data from one SDR's RX and transmits it on
//! another SDR's TX, and vice versa. Supports both dual-device and
//! single-device (dual-channel) modes.

mod autotest;
mod config;
mod detector;
mod device;
mod fuzzer;
mod recording;
mod relay;
mod stats;
mod threads;
#[cfg(test)]
mod integration_tests;

use clap::Parser;
use soapysdr::Direction;

use config::Args;
use device::{
    configure_channel, configure_device, list_devices, open_and_configure_half_duplex, open_device,
    print_device_channels, IqSample,
};
use relay::{run_autotest, run_relay};
use soapysdr::{RxStream, TxStream};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.autotest {
        if args.device3.is_empty() {
            eprintln!("Error: --autotest requires --device3 to be specified");
            std::process::exit(1);
        }
        let min_delay = args.autotest_tx_ms + args.autotest_switch_guard_ms;
        if args.autotest_relay_delay_ms < min_delay {
            eprintln!(
                "Error: --autotest-relay-delay-ms ({}) must be >= --autotest-tx-ms ({}) + --autotest-switch-guard-ms ({})",
                args.autotest_relay_delay_ms, args.autotest_tx_ms, args.autotest_switch_guard_ms
            );
            std::process::exit(1);
        }
    }

    println!("RadioFuzz - Bidirectional SDR IQ Relay");
    println!("======================================\n");

    // List available devices
    list_devices();
    println!();

    // Handle --list-channels mode
    if args.list_channels {
        return handle_list_channels(&args);
    }

    // Run in appropriate mode
    if args.single_device {
        run_single_device_mode(args)
    } else {
        run_dual_device_mode(args)
    }
}

/// Handle --list-channels flag
fn handle_list_channels(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("Opening device to enumerate channels...\n");

    if let Ok(device) = open_device(&args.device1) {
        print_device_channels(&device, &format!("Device ({})", args.device1));
    }

    if !args.single_device {
        if let Ok(device2) = open_device(&args.device2) {
            print_device_channels(&device2, &format!("Device 2 ({})", args.device2));
        }
    }

    println!("\nUse --rx-chan1, --tx-chan1, --rx-chan2, --tx-chan2 to select specific channels.");
    println!("Use --single-device for dual-channel SDRs (LimeSDR, BladeRF, etc.)");
    Ok(())
}

/// Run with a single dual-channel device (LimeSDR, BladeRF, etc.)
fn run_single_device_mode(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("Opening device: {} (single-device mode)", args.device1);
    let device = open_device(&args.device1)?;

    // Verify device has enough channels
    let rx_channels = device.num_channels(Direction::Rx)?;
    let tx_channels = device.num_channels(Direction::Tx)?;

    if rx_channels < 2 || tx_channels < 2 {
        return Err(format!(
            "Single-device mode requires at least 2 RX and 2 TX channels. \
            Device has {} RX and {} TX channels.",
            rx_channels, tx_channels
        )
        .into());
    }

    let rx_chan2 = args.rx_chan2();
    let tx_chan2 = args.tx_chan2();

    // Configure Channel A
    println!("\nConfiguring Channel A (path 1):");
    println!(
        "  RX Channel: {}, TX Channel: {}",
        args.rx_chan1, args.tx_chan1
    );
    configure_channel(
        &device,
        Direction::Rx,
        args.rx_chan1,
        args.sample_rate,
        args.freq1,
        args.rx_gain1,
    )?;
    configure_channel(
        &device,
        Direction::Tx,
        args.tx_chan1,
        args.sample_rate,
        args.freq1,
        args.tx_gain1,
    )?;

    // Configure Channel B
    println!("\nConfiguring Channel B (path 2):");
    println!("  RX Channel: {}, TX Channel: {}", rx_chan2, tx_chan2);
    configure_channel(
        &device,
        Direction::Rx,
        rx_chan2,
        args.sample_rate,
        args.freq2,
        args.rx_gain2,
    )?;
    configure_channel(
        &device,
        Direction::Tx,
        tx_chan2,
        args.sample_rate,
        args.freq2,
        args.tx_gain2,
    )?;

    // Create streams
    println!("\nCreating streams...");
    let rx_stream1: RxStream<IqSample> = device.rx_stream(&[args.rx_chan1])?;
    let tx_stream1: TxStream<IqSample> = device.tx_stream(&[args.tx_chan1])?;
    let rx_stream2: RxStream<IqSample> = device.rx_stream(&[rx_chan2])?;
    let tx_stream2: TxStream<IqSample> = device.tx_stream(&[tx_chan2])?;

    run_relay(args, rx_stream1, tx_stream1, rx_stream2, tx_stream2)
}

/// Run with two separate SDR devices
fn run_dual_device_mode(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("Opening device 1: {}", args.device1);
    let device1 = open_device(&args.device1)?;
    println!("Opening device 2: {}", args.device2);
    let device2 = open_device(&args.device2)?;

    let rx_chan2 = args.rx_chan2();
    let tx_chan2 = args.tx_chan2();

    // Configure devices
    println!("\nConfiguring device 1:");
    configure_device(
        &device1,
        args.sample_rate,
        args.freq1,
        args.freq1,
        args.rx_gain1,
        args.tx_gain1,
        args.rx_chan1,
        args.tx_chan1,
    )?;

    println!("\nConfiguring device 2:");
    configure_device(
        &device2,
        args.sample_rate,
        args.freq2,
        args.freq2,
        args.rx_gain2,
        args.tx_gain2,
        rx_chan2,
        tx_chan2,
    )?;

    // Create streams
    println!("\nCreating streams...");
    let rx_stream1: RxStream<IqSample> = device1.rx_stream(&[args.rx_chan1])?;
    let tx_stream1: TxStream<IqSample> = device1.tx_stream(&[args.tx_chan1])?;
    let rx_stream2: RxStream<IqSample> = device2.rx_stream(&[rx_chan2])?;
    let tx_stream2: TxStream<IqSample> = device2.tx_stream(&[tx_chan2])?;

    if args.autotest {
        // Device3 TX must be on freq1 (Device1 RX frequency).
        // Device3 RX must be on freq2 (Device2 TX frequency).
        println!("\nOpening Device3 (autotest): {}", args.device3);
        let device3 = open_and_configure_half_duplex(
            &args.device3,
            args.sample_rate,
            args.freq1,
            args.freq2,
            args.autotest_rx_gain,
            args.autotest_tx_gain,
            args.autotest_channel,
        )?;

        println!("\nCreating Device3 streams...");
        let rx_stream3: RxStream<IqSample> = device3.rx_stream(&[args.autotest_channel])?;
        let tx_stream3: TxStream<IqSample> = device3.tx_stream(&[args.autotest_channel])?;

        return run_autotest(args, rx_stream1, tx_stream1, rx_stream2, tx_stream2, rx_stream3, tx_stream3);
    }

    run_relay(args, rx_stream1, tx_stream1, rx_stream2, tx_stream2)
}
