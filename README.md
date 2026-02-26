# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
# Build
cargo build

# Run tests (requires `tempfile` dev-dependency — already in Cargo.toml)
cargo test

# Run a single test
cargo test test_detector_transmission

# Run with hardware (typical invocation for two USRP B206mini devices)
cargo run -- --unidirectional --device1 "driver=uhd,serial=34CD2C3" --device2 "driver=uhd,serial=3504A2C" --freq1 433000000 --freq2 435000000 --sample-rate 2000000 --squelch 0.002 --log-samples

# List available devices and channels
cargo run -- --list-channels
```

## Project Target

RadioFuzz aims to be reliable, robust and modular radio fuzzing platform.
It is intended to receive radio transmissions, pass them to fuzzer-module, and then transmit them to the other side.

Radio Fuzz utilizes either one or two SDR devices.
If single SDR contains two TX/RX channels, single device can be used.
If single SDR contains only one Tx&RX channel, two SDR devices are required.

SDR device is communicated with SoapySDR library.

## Architecture

RadioFuzz is a multithreaded SDR relay/fuzzer. `main.rs` opens devices, configures channels, and creates SoapySDR streams. It then calls `relay::run_relay()`, which orchestrates all threads and the stats loop.

### Data flow

Two operation modes dispatch from `relay.rs`:

**Normal mode** (`--fuzz` absent): RX threads forward raw `Vec<IqSample>` buffers over `crossbeam_channel::bounded` channels to TX threads. Transmission detection runs as a side-channel for statistics only.

**Fuzz mode** (`--fuzz`): RX threads run `TransmissionDetector` to collect complete transmissions, pass them through `Fuzzer`, and send `Transmission` structs (complete captured frames) over bounded channels. TX threads only activate the radio when a frame arrives, then deactivate after an idle timeout.

### Thread naming convention

Threads are named by their role and direction: `ChA-RX`, `ChB-TX`, `ChA-RX-Fuzz`, `Delay-A2B`, etc. All thread functions live in `threads.rs` and are spawned in `relay.rs`.

### Key types

- `IqSample` = `Complex<f32>` (defined in `device.rs`, aliased from `num_complex`)
- `Transmission` — a complete detected frame: `samples: Vec<IqSample>`, power metadata, sequential `id`
- `FuzzerAction` — `Pass`, `Modified`, `Drop`, or `Delayed`
- `FuzzerStrategy` — 11 variants parsed from CLI strings (e.g. `drop:50`, `delay:1000`, `bitflip:0.001`)

### Module responsibilities

| Module | Responsibility |
|---|---|
| `config.rs` | All CLI args via `clap` derive; helper methods for converting ms/samples |
| `device.rs` | `open_device`, `configure_channel/device`, `list_devices`, `print_device_channels` |
| `detector.rs` | `TransmissionDetector` state machine (Idle → Receiving → Hangtime); has unit tests |
| `fuzzer.rs` | `Fuzzer` struct + `parse_strategy()`; delayed queue polled by a separate `delayed_poller_thread` |
| `stats.rs` | `DebugStats` (atomic counters), `analyze_samples`, `print_stats`, `generate_test_tone` |
| `recording.rs` | `.iq32` file save/load (interleaved f32 I/Q); `Recorder`/`Replayer` structs; has unit tests |
| `threads.rs` | All RX/TX thread implementations; both raw-buffer and fuzz-mode variants |
| `relay.rs` | Thread spawning dispatch; stats loop; graceful Ctrl+C via `ctrlc` |

### Fuzzer delay architecture

Delayed transmissions are held in `Fuzzer::delayed_queue`. A dedicated `delayed_poller_thread` polls `fuzzer.poll_delayed()` every 10ms and forwards ready transmissions to the TX channel. The `Fuzzer` is shared via `Arc<Mutex<Fuzzer>>`. **Important**: in `rx_fuzz_thread`, the mutex must be released before sending on the channel to avoid holding the lock during a potentially blocking send.

### Recording file format

`.iq32`: interleaved little-endian `f32` I then Q, 8 bytes/sample. A companion `.meta` text file stores `id`, `samples`, `peak`, `avg`, `timestamp`. Files are named `tx_{id:06}_{timestamp_ms}_{samples}samples.iq32`.

## Known Bugs

See `.claude/agent-memory/lead-rust-sdr-engineer/code-review-findings.md` for the full list and fix status.

**Open items** (as of 2026-02-20):

- **All 4 streams created unconditionally** (`main.rs`): even in unidirectional mode, both unused streams are opened. On UHD, DMA buffers are allocated at `activate()` not creation, so the actual waste is small. Deferred to the Device3 integration step, which restructures stream ownership in `main.rs` anyway.
- **Unnecessary sqrt in detector hot path** (`detector.rs`): `magnitude()` calls `sqrt` per sample; could compare magnitude-squared vs threshold². Low priority — not blocking correctness.
- **Autotest mode implemented** — see `--autotest` invocation below. Remaining step: hardware integration test (step 7) to tune timing parameters for the specific Device3 model.

### Automated Test Mode (`--autotest`)

**Concept**: Device3 (a half-duplex SDR — e.g. HackRF, ADALM Pluto, or spare B206mini) transmits a known CW tone burst at `freq1`, the relay receives it on Device1 RX, holds it in the fuzzer delay queue, then transmits on Device2 TX. Device3 switches to RX at `freq2` and captures the relayed signal, then measures SNR and cross-correlation against the known reference waveform.

**Invocation example**:
```bash
cargo run -- \
  --device1 "driver=uhd,serial=34CD2C3" --freq1 433000000 \
  --device2 "driver=uhd,serial=3504A2C" --freq2 435000000 \
  --sample-rate 2000000 --squelch 0.002 \
  --autotest \
  --device3 "driver=hackrf" \
  --autotest-tx-ms 200 \
  --autotest-switch-guard-ms 100 \
  --autotest-relay-delay-ms 600 \
  --autotest-bursts 10 \
  --autotest-snr-threshold-db 10.0
```

**Timing budget (B206mini + HackRF at 2 Msps)**:
- T=0: Device3 starts TX burst (200 ms)
- T=200ms: Device3 deactivates TX, sleeps 100ms switch guard
- T=300ms: Device3 activates RX, captures for 800ms (= relay_delay_ms + tx_ms)
- T=600ms: Device2 TX transmits the relayed burst (after 600ms fixed delay)
- T=800ms: Device3 deactivates RX; evaluates SNR and xcorr

**Pass criteria**: ≥80% of bursts meet `--autotest-snr-threshold-db`; exits with code 0 (PASS) or 1 (FAIL).

**Key implementation details**:
- `open_and_configure_half_duplex` (`device.rs`): pre-configures Device3 TX at `freq1` and RX at `freq2`; the hardware retunes automatically between phases
- `autotest_device3_thread` (`threads.rs`): TX→guard→RX→evaluate loop; sets `test_passed=false` and `running=false` on completion
- `spawn_autotest_threads` (`relay.rs`): uses `FuzzerStrategy::FixedDelay(relay_delay_ms)` ignoring `--fuzz-strategy`; passes `test_passed: Arc<AtomicBool>` to the Device3 thread; main exits with `process::exit(1)` on failure
- Single-device mode does not support autotest (emits a clear error)

## Hardware Notes

- Target hardware: two USRP B206mini (single-channel each, UHD driver)
- Serials: `34CD2C3` (device1), `3504A2C` (device2)
- Gain element: `PGA` only; frequency range 42–6008 MHz
- At 2 Msps, noise floor is ~−56 dB peak vs default squelch of −54 dB — adjust `--squelch` carefully to avoid false triggers
