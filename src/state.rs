use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::cat_codec::RadioMode;
use crate::serial_io::SerialOutbound;

/// Shared application state using atomics on hot paths.
pub struct AppState {
    pub freq_hz: AtomicU64,
    pub tx: AtomicBool,
    pub stop_audio: AtomicBool,
    pub shutting_down: AtomicBool,
    pub streaming_started: AtomicBool,
    input_level_bits: AtomicU32,
    output_level_bits: AtomicU32,
    pub mode: AtomicU32,
    outbound: Mutex<Vec<SerialOutbound>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            freq_hz: AtomicU64::new(0),
            tx: AtomicBool::new(false),
            stop_audio: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            streaming_started: AtomicBool::new(false),
            input_level_bits: AtomicU32::new(0f32.to_bits()),
            output_level_bits: AtomicU32::new(0f32.to_bits()),
            mode: AtomicU32::new(RadioMode::Usb as u32),
            outbound: Mutex::new(Vec::new()),
        })
    }

    pub fn input_level(&self) -> f32 {
        f32::from_bits(self.input_level_bits.load(Ordering::Relaxed))
    }

    pub fn set_input_level(&self, v: f32) {
        self.input_level_bits
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn output_level(&self) -> f32 {
        f32::from_bits(self.output_level_bits.load(Ordering::Relaxed))
    }

    pub fn set_output_level(&self, v: f32) {
        self.output_level_bits
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn mode(&self) -> RadioMode {
        RadioMode::from_u8(self.mode.load(Ordering::Relaxed) as u8)
    }

    pub fn set_mode(&self, mode: RadioMode) {
        self.mode.store(mode as u32, Ordering::Relaxed);
    }

    pub fn lock_outbound(&self) -> std::sync::MutexGuard<'_, Vec<SerialOutbound>> {
        self.outbound.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Lock a mutex, recovering from poison so one panicking thread does not abort others.
pub fn lock_mutex<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_state_level_roundtrip() {
        // Arrange
        let state = AppState::new();

        // Act
        state.set_input_level(0.5);
        state.set_output_level(0.25);

        // Assert
        assert!((state.input_level() - 0.5).abs() < 1e-6);
        assert!((state.output_level() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn app_state_mode_roundtrip() {
        // Arrange
        let state = AppState::new();

        // Act
        state.set_mode(RadioMode::Lsb);

        // Assert
        assert_eq!(state.mode(), RadioMode::Lsb);
    }

    #[test]
    fn lock_mutex_recovers_from_poison() {
        // Arrange
        let m = Arc::new(Mutex::new(1u32));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();

        // Act
        let guard = lock_mutex(&m);

        // Assert
        assert_eq!(*guard, 1);
    }
}
