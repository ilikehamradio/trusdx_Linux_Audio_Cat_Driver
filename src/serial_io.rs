//! Serial I/O helpers: soft CAT outbound queue + RX demux (CAT text vs US audio).
//!
//! Urgent control (set-freq / PTT / mode) is written directly from `rigctl`.
//! This module handles the multiplexed serial byte stream and queued soft CAT
//! (e.g. periodic VFO / mode poll).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serialport::SerialPort;
use tracing::warn;

use crate::cat_codec::{parse_cat_text_frame, push_text_byte, CatFrame};
use crate::state::AppState;
use crate::trusdx;

const WAVE_BUF_MAX_SIZE: usize = 512;
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(2);
const STATUS_POLL_TX_COOLDOWN: Duration = Duration::from_millis(500);

/// Soft/async CAT work (not TX audio sample frames, not urgent rigctl control).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialOutbound {
    Bytes(Vec<u8>),
    StartTransmit,
    StreamingSpeakerOff,
    StreamingSpeakerOn,
    QueryVfo,
    QueryMode,
}

pub fn enqueue(state: &AppState, cmd: SerialOutbound) {
    state.lock_outbound().push(cmd);
}

pub fn enqueue_bytes(state: &AppState, bytes: Vec<u8>) {
    enqueue(state, SerialOutbound::Bytes(bytes));
}

/// Apply a batch of outbound commands on an already-locked port.
pub fn apply_all(port: &mut dyn SerialPort, cmds: impl IntoIterator<Item = SerialOutbound>) {
    for cmd in cmds {
        if let Err(e) = apply_one(port, cmd) {
            warn!(error = %e, "serial_io: outbound command failed");
        }
    }
    let _ = trusdx::flush_serial_line(port);
}

fn apply_one(port: &mut dyn SerialPort, cmd: SerialOutbound) -> std::io::Result<()> {
    match cmd {
        SerialOutbound::Bytes(data) => std::io::Write::write_all(port, &data),
        SerialOutbound::StartTransmit => trusdx::start_transmit_baseband(port),
        SerialOutbound::StreamingSpeakerOff => trusdx::enable_streaming_speaker_off(port),
        SerialOutbound::StreamingSpeakerOn => trusdx::enable_streaming_speaker_on(port),
        SerialOutbound::QueryVfo => trusdx::query_vfo_a(port),
        SerialOutbound::QueryMode => trusdx::query_mode(port),
    }
}

/// Whether a soft status poll (`FA;` / `MD;`) should run now.
pub fn status_poll_due(
    is_tx: bool,
    last_tx_end: Instant,
    last_poll: Instant,
    now: Instant,
) -> bool {
    if is_tx {
        return false;
    }
    if now.duration_since(last_poll) < STATUS_POLL_INTERVAL {
        return false;
    }
    now.duration_since(last_tx_end) >= STATUS_POLL_TX_COOLDOWN
}

fn enqueue_status_queries(state: &AppState) {
    enqueue(state, SerialOutbound::QueryVfo);
    enqueue(state, SerialOutbound::QueryMode);
}

/// Background soft CAT: immediate `;FA;`/`;MD;` then every ~2s while in RX.
///
/// Leading `;` interrupts an in-flight `US` audio stream so the radio will
/// answer; replies update [`AppState::freq_hz`] / mode via the RX demux.
pub fn spawn_status_poller(state: Arc<AppState>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Kick off a read as soon as the audio bridge can drain soft CAT.
        enqueue_status_queries(&state);

        let mut last_poll = Instant::now();
        let mut prev_tx = false;
        let mut last_tx_end = Instant::now() - STATUS_POLL_TX_COOLDOWN;

        loop {
            if state.shutting_down.load(Ordering::Relaxed)
                || state.stop_audio.load(Ordering::Relaxed)
            {
                break;
            }

            let tx_now = state.tx.load(Ordering::Relaxed);
            if prev_tx && !tx_now {
                last_tx_end = Instant::now();
            }
            prev_tx = tx_now;

            let now = Instant::now();
            if status_poll_due(tx_now, last_tx_end, last_poll, now) {
                enqueue_status_queries(&state);
                last_poll = now;
            }

            thread::sleep(Duration::from_millis(100));
        }
    })
}

/// Drain the shared queue and apply under `port`.
pub fn drain(state: &AppState, port: &mut dyn SerialPort) {
    let cmds: Vec<SerialOutbound> = {
        let mut q = state.lock_outbound();
        if q.is_empty() {
            return;
        }
        q.drain(..).collect()
    };
    apply_all(port, cmds);
}

pub fn has_pending(state: &AppState) -> bool {
    !state.lock_outbound().is_empty()
}

/// Demuxes the radio's multiplexed serial stream: CAT text vs `US` audio frames.
pub struct SerialRxDemux {
    inbound_serial_audio: bool,
    text_buf: Vec<u8>,
    wave_buf: Vec<u8>,
}

impl SerialRxDemux {
    pub fn new() -> Self {
        Self {
            inbound_serial_audio: false,
            text_buf: Vec::with_capacity(64),
            wave_buf: Vec::with_capacity(8192),
        }
    }

    /// Clear demux state (e.g. when leaving TX and restarting RX streaming).
    pub fn reset(&mut self) {
        self.inbound_serial_audio = false;
        self.text_buf.clear();
        self.wave_buf.clear();
    }

    /// True while collecting samples inside a `US`…`;` audio frame.
    pub fn in_audio_frame(&self) -> bool {
        self.inbound_serial_audio
    }

    /// Process one RX byte from the serial port.
    ///
    /// When a wave chunk is ready to play, `on_audio` is called with the samples.
    /// Returns `true` if soft CAT should be drained now (safe gap between frames / after CAT).
    pub fn push_byte(
        &mut self,
        byte: u8,
        state: &AppState,
        mut on_audio: impl FnMut(&[u8]),
    ) -> bool {
        // Inside a US audio frame: collect samples until ';' ends the frame.
        if self.inbound_serial_audio {
            if byte == b';' {
                on_audio(&self.wave_buf);
                self.wave_buf.clear();
                self.inbound_serial_audio = false;
                return true;
            }
            self.wave_buf.push(byte);
            if self.wave_buf.len() >= WAVE_BUF_MAX_SIZE {
                on_audio(&self.wave_buf);
                self.wave_buf.clear();
            }
            return false;
        }

        // Text mode: CAT replies and "US" audio openers share this line.
        push_text_byte(&mut self.text_buf, byte);

        // "US" means sound samples are about to follow.
        if self.text_buf.len() == 2 && self.text_buf[0] == b'U' && self.text_buf[1] == b'S' {
            self.text_buf.clear();
            self.inbound_serial_audio = true;
            state
                .streaming_started
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return false;
        }

        // ';' means a CAT text message finished (e.g. FA… / MD…).
        if byte == b';' {
            match parse_cat_text_frame(&self.text_buf) {
                CatFrame::Frequency(hz) => {
                    state
                        .freq_hz
                        .store(hz, std::sync::atomic::Ordering::Relaxed);
                }
                CatFrame::Mode(mode) => {
                    state.set_mode(mode);
                }
                _ => {}
            }
            self.text_buf.clear();
            return has_pending(state);
        }

        false
    }
}

impl Default for SerialRxDemux {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cat_codec::{format_fa_command, RadioMode};
    use std::sync::atomic::Ordering;

    #[test]
    fn enqueue_bytes_pushes_outbound() {
        // Arrange
        let state = AppState::new();

        // Act
        enqueue_bytes(&state, format_fa_command(14_074_000));

        // Assert
        let q = state.lock_outbound();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0], SerialOutbound::Bytes(b"FA00014074000;".to_vec()));
    }

    #[test]
    fn demux_us_delivers_audio_then_returns_to_text() {
        // Arrange
        let state = AppState::new();
        let mut demux = SerialRxDemux::new();
        let mut played: Vec<Vec<u8>> = Vec::new();

        // Act — US + three samples + ';'
        for &b in b"US\x01\x02\x03;" {
            let _ = demux.push_byte(b, &state, |wave| played.push(wave.to_vec()));
        }

        // Assert
        assert!(state.streaming_started.load(Ordering::Relaxed));
        assert_eq!(played.len(), 1);
        assert_eq!(played[0], vec![1, 2, 3]);
        assert!(!demux.inbound_serial_audio);
    }

    #[test]
    fn demux_fa_updates_frequency() {
        // Arrange
        let state = AppState::new();
        let mut demux = SerialRxDemux::new();

        // Act
        for &b in b"FA00014074000;" {
            let _ = demux.push_byte(b, &state, |_| {});
        }

        // Assert
        assert_eq!(state.freq_hz.load(Ordering::Relaxed), 14_074_000);
    }

    #[test]
    fn demux_fa_after_us_interrupt_updates_frequency() {
        // Arrange — continuous US (no ';') then interrupt framing from radio.
        let state = AppState::new();
        let mut demux = SerialRxDemux::new();
        for &b in b"US\x10\x20\x30" {
            let _ = demux.push_byte(b, &state, |_| {});
        }
        assert!(demux.in_audio_frame());

        // Act — ';' ends audio; FA reply; US resumes.
        for &b in b";FA00007000000;US\x40" {
            let _ = demux.push_byte(b, &state, |_| {});
        }

        // Assert
        assert_eq!(state.freq_hz.load(Ordering::Relaxed), 7_000_000);
        assert!(demux.in_audio_frame());
    }

    #[test]
    fn demux_md_updates_mode() {
        // Arrange
        let state = AppState::new();
        let mut demux = SerialRxDemux::new();

        // Act
        for &b in b"MD1;" {
            let _ = demux.push_byte(b, &state, |_| {});
        }

        // Assert
        assert_eq!(state.mode(), RadioMode::Lsb);
    }

    #[test]
    fn status_poll_due_skips_while_tx() {
        // Arrange
        let last_tx_end = Instant::now() - STATUS_POLL_TX_COOLDOWN;
        let last_poll = Instant::now() - STATUS_POLL_INTERVAL;
        let now = Instant::now();

        // Act
        let due = status_poll_due(true, last_tx_end, last_poll, now);

        // Assert
        assert!(!due);
    }

    #[test]
    fn status_poll_due_true_after_interval_in_rx() {
        // Arrange
        let last_tx_end = Instant::now() - STATUS_POLL_TX_COOLDOWN;
        let last_poll = Instant::now() - STATUS_POLL_INTERVAL;
        let now = Instant::now();

        // Act
        let due = status_poll_due(false, last_tx_end, last_poll, now);

        // Assert
        assert!(due);
    }

    #[test]
    fn status_poll_due_false_during_tx_cooldown() {
        // Arrange
        let last_tx_end = Instant::now();
        let last_poll = Instant::now() - STATUS_POLL_INTERVAL;
        let now = Instant::now();

        // Act
        let due = status_poll_due(false, last_tx_end, last_poll, now);

        // Assert
        assert!(!due);
    }

    #[test]
    fn demux_in_audio_frame_tracks_us() {
        // Arrange
        let state = AppState::new();
        let mut demux = SerialRxDemux::new();

        // Act
        let _ = demux.push_byte(b'U', &state, |_| {});
        let _ = demux.push_byte(b'S', &state, |_| {});

        // Assert
        assert!(demux.in_audio_frame());
    }
}
