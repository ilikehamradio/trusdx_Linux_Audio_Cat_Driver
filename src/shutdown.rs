use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tracing::{error, warn};

use crate::state::AppState;

pub fn shutdown(
    state: Arc<AppState>,
    ser: Arc<Mutex<Box<dyn serialport::SerialPort + Send>>>,
) {
    state.shutting_down.store(true, Ordering::Relaxed);
    state.stop_audio.store(true, Ordering::Relaxed);

    if let Ok(mut s) = ser.lock() {
        if let Err(e) = crate::trusdx::enable_streaming_speaker_on(&mut **s) {
            warn!(error = %e, "shutdown: restore speaker streaming failed");
        }
        if let Err(e) = s.flush() {
            warn!(error = %e, "shutdown: serial flush failed");
        }
    } else {
        error!("shutdown: could not lock serial port");
    }
}
