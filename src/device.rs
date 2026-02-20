//! SDR device management and configuration

use num_complex::Complex;
use soapysdr::{Device, Direction};

pub type IqSample = Complex<f32>;

/// List all available SDR devices
pub fn list_devices() {
    println!("Available SDR devices:");
    match soapysdr::enumerate("") {
        Ok(devices) => {
            if devices.is_empty() {
                println!("  No devices found!");
            } else {
                for (i, dev) in devices.iter().enumerate() {
                    println!("  [{}] {}", i, dev);
                }
            }
        }
        Err(e) => println!("  Error enumerating devices: {}", e),
    }
}

/// Print detailed channel information for a device
pub fn print_device_channels(device: &Device, name: &str) {
    println!("\n{} Channel Information:", name);

    // Print RX channels
    let rx_channels = device.num_channels(Direction::Rx).unwrap_or(0);
    println!("  RX Channels: {}", rx_channels);
    for ch in 0..rx_channels {
        print!("    [{}]", ch);
        if let Ok(info) = device.channel_info(Direction::Rx, ch) {
            print!(" {}", info);
        }
        if let Ok(antennas) = device.antennas(Direction::Rx, ch) {
            print!(" | Antennas: {:?}", antennas);
        }
        if let Ok(ranges) = device.frequency_range(Direction::Rx, ch) {
            if !ranges.is_empty() {
                print!(
                    " | Freq: {:.1}-{:.1} MHz",
                    ranges[0].minimum / 1e6,
                    ranges.last().unwrap().maximum / 1e6
                );
            }
        }
        println!();
    }

    // Print TX channels
    let tx_channels = device.num_channels(Direction::Tx).unwrap_or(0);
    println!("  TX Channels: {}", tx_channels);
    for ch in 0..tx_channels {
        print!("    [{}]", ch);
        if let Ok(info) = device.channel_info(Direction::Tx, ch) {
            print!(" {}", info);
        }
        if let Ok(antennas) = device.antennas(Direction::Tx, ch) {
            print!(" | Antennas: {:?}", antennas);
        }
        if let Ok(ranges) = device.frequency_range(Direction::Tx, ch) {
            if !ranges.is_empty() {
                print!(
                    " | Freq: {:.1}-{:.1} MHz",
                    ranges[0].minimum / 1e6,
                    ranges.last().unwrap().maximum / 1e6
                );
            }
        }
        println!();
    }

    // Print gain elements
    println!("  RX Gain Elements:");
    for ch in 0..rx_channels {
        if let Ok(gains) = device.list_gains(Direction::Rx, ch) {
            println!("    Channel {}: {:?}", ch, gains);
        }
    }
    println!("  TX Gain Elements:");
    for ch in 0..tx_channels {
        if let Ok(gains) = device.list_gains(Direction::Tx, ch) {
            println!("    Channel {}: {:?}", ch, gains);
        }
    }
}

/// Open a device by index or filter string
pub fn open_device(filter: &str) -> Result<Device, soapysdr::Error> {
    // Try to parse as device index first
    if let Ok(index) = filter.parse::<usize>() {
        let devices = soapysdr::enumerate("")?;
        if index < devices.len() {
            let dev_str = devices[index].to_string();
            return Device::new(dev_str.as_str());
        }
    }
    // Otherwise use as filter string
    Device::new(filter)
}

/// Configure a device with RX and TX settings
pub fn configure_device(
    device: &Device,
    sample_rate: f64,
    rx_freq: f64,
    tx_freq: f64,
    rx_gain: f64,
    tx_gain: f64,
    rx_channel: usize,
    tx_channel: usize,
) -> Result<(), soapysdr::Error> {
    // Validate channels exist
    let rx_channels = device.num_channels(Direction::Rx)?;
    let tx_channels = device.num_channels(Direction::Tx)?;

    if rx_channel >= rx_channels {
        return Err(soapysdr::Error {
            code: soapysdr::ErrorCode::NotSupported,
            message: format!(
                "RX channel {} not available (device has {} RX channels)",
                rx_channel, rx_channels
            ),
        });
    }
    if tx_channel >= tx_channels {
        return Err(soapysdr::Error {
            code: soapysdr::ErrorCode::NotSupported,
            message: format!(
                "TX channel {} not available (device has {} TX channels)",
                tx_channel, tx_channels
            ),
        });
    }

    // Configure RX
    device.set_sample_rate(Direction::Rx, rx_channel, sample_rate)?;
    device.set_frequency(Direction::Rx, rx_channel, rx_freq, ())?;
    device.set_gain(Direction::Rx, rx_channel, rx_gain)?;

    // Configure TX
    device.set_sample_rate(Direction::Tx, tx_channel, sample_rate)?;
    device.set_frequency(Direction::Tx, tx_channel, tx_freq, ())?;
    device.set_gain(Direction::Tx, tx_channel, tx_gain)?;

    println!("  RX Channel: {}", rx_channel);
    println!("  TX Channel: {}", tx_channel);
    println!(
        "  Sample rate: {} MHz",
        device.sample_rate(Direction::Rx, rx_channel)? / 1e6
    );
    println!(
        "  RX Frequency: {} MHz",
        device.frequency(Direction::Rx, rx_channel)? / 1e6
    );
    println!(
        "  TX Frequency: {} MHz",
        device.frequency(Direction::Tx, tx_channel)? / 1e6
    );
    println!("  RX Gain: {} dB", device.gain(Direction::Rx, rx_channel)?);
    println!("  TX Gain: {} dB", device.gain(Direction::Tx, tx_channel)?);

    Ok(())
}

/// Open and configure a half-duplex device (single channel used for both TX and RX).
///
/// Intended for Device3 in automated test mode (e.g., HackRF, ADALM Pluto, spare B206mini).
/// Both directions are pre-configured with their respective frequencies; the hardware retunes
/// automatically when the stream direction changes between TX and RX phases.
///
/// `tx_freq` — center frequency during the TX phase (must match Device1 RX center frequency)
/// `rx_freq` — center frequency during the RX phase (must match Device2 TX center frequency)
pub fn open_and_configure_half_duplex(
    filter: &str,
    sample_rate: f64,
    tx_freq: f64,
    rx_freq: f64,
    rx_gain: f64,
    tx_gain: f64,
    channel: usize,
) -> Result<Device, soapysdr::Error> {
    let device = open_device(filter)?;
    println!(
        "  Configuring half-duplex channel {} (TX @ {:.3} MHz, RX @ {:.3} MHz):",
        channel,
        tx_freq / 1e6,
        rx_freq / 1e6
    );
    configure_channel(&device, Direction::Tx, channel, sample_rate, tx_freq, tx_gain)?;
    configure_channel(&device, Direction::Rx, channel, sample_rate, rx_freq, rx_gain)?;
    Ok(device)
}

/// Configure a single channel (RX or TX)
pub fn configure_channel(
    device: &Device,
    direction: Direction,
    channel: usize,
    sample_rate: f64,
    freq: f64,
    gain: f64,
) -> Result<(), soapysdr::Error> {
    device.set_sample_rate(direction, channel, sample_rate)?;
    device.set_frequency(direction, channel, freq, ())?;
    device.set_gain(direction, channel, gain)?;

    let dir_str = match direction {
        Direction::Rx => "RX",
        Direction::Tx => "TX",
    };
    println!(
        "    {} Ch{}: {:.2} MHz, {:.1} dB gain, {:.2} Msps",
        dir_str,
        channel,
        device.frequency(direction, channel)? / 1e6,
        device.gain(direction, channel)?,
        device.sample_rate(direction, channel)? / 1e6
    );
    Ok(())
}

