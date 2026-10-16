//! PulseAudio null-sink setup and the RX/TX audio bridge thread.
//!
//! Serial CAT/US demux lives in [`crate::serial_io`]; this module owns Pulse
//! capture/playback and sample conversion.

use std::io::Read;
use std::process::Command;
use std::sync::{atomic::Ordering, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use libpulse_binding as pulse;
use libpulse_binding::def::BufferAttr;
use libpulse_simple_binding as psimple;
use tracing::{error, warn};

use crate::cat_codec::{
    bytes_to_i16_le, calculate_rms, encode_tx_samples, AUDIO_SAMPLE_SIZE, TX_GAIN,
};
use crate::serial_io::{self, SerialRxDemux};
use crate::state::AppState;

const TX_FALLING_DELAY_MS: u64 = 30;
const STREAMING_TIMEOUT_MS: u64 = 200;
const STREAMING_RETRY_TIMEOUT_MS: u64 = 100;
const POLL_INTERVAL_MS: u64 = 5;
const DRAIN_ATTEMPTS: usize = 10;
/// Display gain for RX meter (RMS). Kept modest so normal RX does not peg ~70%+.
const INPUT_LEVEL_SCALE: f32 = 1.35;
/// Per RX-loop decay when the new reading is quieter or no frame arrived (~10ms ticks).
const INPUT_LEVEL_DECAY: f32 = 0.88;
const U8_TO_F32_OFFSET: f32 = 128.0;
const U8_TO_F32_SCALE: f32 = 128.0;

pub fn cleanup_trusdx_audio_interfaces() {
    if let Ok(output) = Command::new("pactl")
        .arg("list")
        .arg("short")
        .arg("modules")
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains("TRUSDX") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(module_id) = parts.first() {
                    if let Err(e) = Command::new("pactl")
                        .arg("unload-module")
                        .arg(module_id)
                        .status()
                    {
                        warn!(module_id, error = %e, "failed to unload Pulse module");
                    }
                }
            }
        }
    }
}

pub fn create_trusdx_audio_interface(audio_rate: u32) -> Option<u32> {
    let rate_arg = format!("rate={}", audio_rate);
    let sink_output = Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            "sink_name=TRUSDX",
            "sink_properties=device.description=\"TRUSDX Audio\"",
            &rate_arg,
        ])
        .output();

    match sink_output {
        Ok(result) if result.status.success() => String::from_utf8_lossy(&result.stdout)
            .trim()
            .parse::<u32>()
            .ok(),
        Ok(result) => {
            error!(
                stderr = %String::from_utf8_lossy(&result.stderr),
                "pactl load-module failed"
            );
            None
        }
        Err(e) => {
            error!(error = %e, "failed to run pactl");
            None
        }
    }
}

#[derive(Clone)]
pub struct AudioHandles {
    pub pa_playback: Arc<psimple::Simple>,
    pub pa_record: Arc<psimple::Simple>,
}

fn create_buffer_attr(rate: u32, sample_size: usize) -> BufferAttr {
    let sample_size_u32 = sample_size as u32;
    BufferAttr {
        maxlength: (rate / 4) * sample_size_u32,
        tlength: (rate / 50) * sample_size_u32,
        prebuf: (rate / 100) * sample_size_u32,
        minreq: (rate / 200) * sample_size_u32,
        fragsize: (rate / 100) * sample_size_u32,
    }
}

pub fn setup_pulseaudio(audio_rx_rate: u32, audio_tx_rate: u32) -> anyhow::Result<AudioHandles> {
    let spec_rx = pulse::sample::Spec {
        format: pulse::sample::Format::F32le,
        channels: 1,
        rate: audio_rx_rate,
    };
    let spec_tx = pulse::sample::Spec {
        format: pulse::sample::Format::S16le,
        channels: 1,
        rate: audio_tx_rate,
    };

    let pb_attr = create_buffer_attr(audio_rx_rate, std::mem::size_of::<f32>());
    let rec_attr = create_buffer_attr(audio_tx_rate, std::mem::size_of::<i16>());

    let pa_playback = psimple::Simple::new(
        None,
        "trusdxAudio",
        pulse::stream::Direction::Playback,
        Some("TRUSDX"),
        "Radio RX Audio",
        &spec_rx,
        None,
        Some(&pb_attr),
    )?;

    let pa_record = psimple::Simple::new(
        None,
        "trusdxAudio",
        pulse::stream::Direction::Record,
        Some("TRUSDX.monitor"),
        "Radio TX Audio",
        &spec_tx,
        None,
        Some(&rec_attr),
    )?;

    Ok(AudioHandles {
        pa_playback: Arc::new(pa_playback),
        pa_record: Arc::new(pa_record),
    })
}

fn convert_wave_to_f32(wave_buf: &[u8], f32_buf: &mut Vec<f32>) {
    f32_buf.clear();
    f32_buf.reserve(wave_buf.len());
    for &sample in wave_buf {
        f32_buf.push((sample as f32 - U8_TO_F32_OFFSET) / U8_TO_F32_SCALE);
    }
}

/// RMS of u8 CAT audio samples (128-centered), 0.0–1.0.
fn calculate_wave_rms(wave_buf: &[u8]) -> f32 {
    if wave_buf.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = wave_buf
        .iter()
        .map(|&sample| {
            let n = (sample as f32 - U8_TO_F32_OFFSET) / U8_TO_F32_SCALE;
            n * n
        })
        .sum();
    (sum_sq / wave_buf.len() as f32).sqrt().min(1.0)
}

/// Instant attack, exponential decay so the RX bar does not stick on the last peak.
fn update_input_meter(state: &AppState, measured: f32) {
    let prev = state.input_level();
    let next = if measured > prev {
        measured
    } else {
        prev * INPUT_LEVEL_DECAY
    };
    state.set_input_level(next);
}

fn write_audio_buffer(audio: &psimple::Simple, f32_buf: &[f32]) {
    let bytes = unsafe {
        std::slice::from_raw_parts(
            f32_buf.as_ptr() as *const u8,
            std::mem::size_of_val(f32_buf),
        )
    };
    if let Err(e) = audio.write(bytes) {
        warn!(error = %e, "pulse playback write failed");
    }
}

fn process_and_write_wave(
    wave_buf: &[u8],
    f32_buf: &mut Vec<f32>,
    audio: &psimple::Simple,
    state: &AppState,
) {
    if wave_buf.is_empty() {
        return;
    }

    let measured = (calculate_wave_rms(wave_buf) * INPUT_LEVEL_SCALE).min(1.0);
    update_input_meter(state, measured);

    convert_wave_to_f32(wave_buf, f32_buf);
    write_audio_buffer(audio, f32_buf);
}

fn wait_for_streaming(state: &AppState, timeout_ms: u64) {
    let start = std::time::Instant::now();
    while !state.streaming_started.load(Ordering::Relaxed)
        && start.elapsed() < Duration::from_millis(timeout_ms)
    {
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

pub fn run_audio_bridge(
    ser: Arc<Mutex<Box<dyn serialport::SerialPort + Send>>>,
    audio: AudioHandles,
    state: Arc<AppState>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut demux = SerialRxDemux::new();
        let mut rx_tmp = [0u8; 512];
        let mut f32_buf: Vec<f32> = Vec::with_capacity(1024);

        // TX path: Pulse → i16 → u8 CAT samples.
        let mut tx_i16_buf = vec![0i16; AUDIO_SAMPLE_SIZE];
        let mut tx_byte_buf = vec![0u8; tx_i16_buf.len() * 2];
        let mut u8_buf = vec![0u8; AUDIO_SAMPLE_SIZE];
        let mut drain_buf = vec![0u8; 1024];

        // Soft/async CAT only (e.g. VFO poll). Control like set-freq/PTT is direct from rigctl.
        let drain_outbound = || {
            if let Ok(mut serial_port) = ser.lock() {
                serial_io::drain(&state, &mut **serial_port);
            }
        };

        let mut prev_tx = false;

        loop {
            if state.stop_audio.load(Ordering::Relaxed) {
                break;
            }

            let is_tx = state.tx.load(Ordering::Relaxed);
            let is_rx = !is_tx;
            let starting_tx = is_tx && !prev_tx;
            let starting_rx = !is_tx && prev_tx;

            // Entering TX: discard stale Pulse capture so we don't blast old audio.
            if starting_tx {
                for _ in 0..DRAIN_ATTEMPTS {
                    if audio.pa_record.read(&mut drain_buf).is_err() {
                        break;
                    }
                }
            }

            // Entering RX: reset demux and ask the radio to start US streaming again.
            if starting_rx {
                demux.reset();
                state.streaming_started.store(false, Ordering::Relaxed);

                thread::sleep(Duration::from_millis(TX_FALLING_DELAY_MS));

                if let Ok(mut serial_port) = ser.lock() {
                    if let Err(e) =
                        crate::trusdx::enable_streaming_speaker_off(&mut **serial_port)
                    {
                        warn!(error = %e, "enable streaming failed");
                    }
                }

                wait_for_streaming(&state, STREAMING_TIMEOUT_MS);

                // Retry once if we never saw a US opener.
                if !state.streaming_started.load(Ordering::Relaxed) {
                    if let Ok(mut serial_port) = ser.lock() {
                        let _ = crate::trusdx::enable_streaming_speaker_off(&mut **serial_port);
                    }
                    wait_for_streaming(&state, STREAMING_RETRY_TIMEOUT_MS);
                }
            }

            prev_tx = is_tx;

            // --- TX: app audio (Pulse TRUSDX.monitor) → serial samples ---
            if is_tx {
                if serial_io::has_pending(&state) {
                    drain_outbound();
                }

                state.set_input_level(0.0);

                match audio.pa_record.read(&mut tx_byte_buf) {
                    Ok(_) => {
                        bytes_to_i16_le(&tx_byte_buf, &mut tx_i16_buf);
                        let rms = calculate_rms(&tx_i16_buf);
                        state.set_output_level(rms);

                        encode_tx_samples(&tx_i16_buf, TX_GAIN, &mut u8_buf);

                        if let Ok(mut serial_port) = ser.lock() {
                            let _ =
                                crate::trusdx::send_audio_stream_raw(&mut **serial_port, &u8_buf);
                        }
                    }
                    Err(_) => {
                        continue;
                    }
                }
            }

            // --- RX: serial bytes → demux → Pulse playback for audio chunks ---
            if is_rx {
                state.set_output_level(0.0);

                // Soft CAT must be sent even mid-US: trUSDX often never emits a
                // terminating ';' until the host interrupts with a CAT command
                // (e.g. ";FA;"). Waiting for !in_audio_frame() deadlocks the poller.
                if serial_io::has_pending(&state) {
                    drain_outbound();
                }

                let bytes_read = {
                    if let Ok(mut serial_port) = ser.lock() {
                        serial_port.read(&mut rx_tmp).unwrap_or(0)
                    } else {
                        0
                    }
                };

                let mut saw_audio_chunk = false;
                for &byte in &rx_tmp[..bytes_read] {
                    let drain = demux.push_byte(byte, &state, |wave| {
                        saw_audio_chunk = true;
                        process_and_write_wave(
                            wave,
                            &mut f32_buf,
                            &audio.pa_playback,
                            &state,
                        );
                    });
                    if drain {
                        drain_outbound();
                    }
                }

                // No new US chunk this tick → decay so the meter does not hang.
                if !saw_audio_chunk {
                    update_input_meter(&state, 0.0);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculate_wave_rms_silence_is_zero() {
        // Arrange
        let samples = [128u8; 16];

        // Act
        let rms = calculate_wave_rms(&samples);

        // Assert
        assert!(rms < 1e-6);
    }

    #[test]
    fn calculate_wave_rms_full_scale_near_one() {
        // Arrange — alternate min/max u8 around midscale
        let samples = [0u8, 255u8, 0u8, 255u8];

        // Act
        let rms = calculate_wave_rms(&samples);

        // Assert
        assert!(rms > 0.9);
        assert!(rms <= 1.0);
    }

    #[test]
    fn update_input_meter_decays_when_quieter() {
        // Arrange
        let state = AppState::new();
        state.set_input_level(0.8);

        // Act
        update_input_meter(&state, 0.0);

        // Assert
        let lvl = state.input_level();
        assert!(lvl < 0.8);
        assert!((lvl - 0.8 * INPUT_LEVEL_DECAY).abs() < 1e-5);
    }

    #[test]
    fn update_input_meter_attacks_instantly() {
        // Arrange
        let state = AppState::new();
        state.set_input_level(0.1);

        // Act
        update_input_meter(&state, 0.7);

        // Assert
        assert!((state.input_level() - 0.7).abs() < 1e-6);
    }
}
