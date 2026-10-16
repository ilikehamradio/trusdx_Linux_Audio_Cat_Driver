//! hamlib-compatible rigctl TCP server.
//!
//! Frequency / mode / PTT are written directly to the serial port (same as the
//! original T / control path). The audio-bridge outbound queue is not required
//! for those commands.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tracing::{error, warn};

use crate::cat_codec::RadioMode;
use crate::state::AppState;
use crate::trusdx;

/// Result of handling one rigctl protocol line (without I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RigctlAction {
    Reply(Vec<String>),
    Quit,
}

/// Serial side-effects that the TCP handler applies with the port lock held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RigctlSerialOp {
    None,
    SetFreq(u64),
    SetMode(RadioMode),
    SetPtt(bool),
}

/// Parse a frequency token (Hz as integer or float).
pub fn parse_freq_hz(token: &str) -> Option<u64> {
    if let Ok(hz_int) = token.parse::<u64>() {
        Some(hz_int)
    } else if let Ok(hz_f) = token.parse::<f64>() {
        Some(hz_f.round() as u64)
    } else {
        None
    }
}

fn freq_from_args(args: &[&str]) -> Option<u64> {
    for token in args {
        if let Some(hz) = parse_freq_hz(token) {
            return Some(hz);
        }
    }
    None
}

fn set_frequency(state: &AppState, hz: u64) -> (RigctlAction, RigctlSerialOp) {
    state.freq_hz.store(hz, Ordering::Relaxed);
    (
        RigctlAction::Reply(vec!["RPRT 0".into()]),
        RigctlSerialOp::SetFreq(hz),
    )
}

/// Parse and act on a single rigctl command line against shared state.
pub fn handle_rigctl_command(cmd: &str, state: &AppState) -> (RigctlAction, RigctlSerialOp) {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return (RigctlAction::Reply(vec![]), RigctlSerialOp::None);
    }

    match cmd.chars().next().unwrap_or('\0') {
        '\\' => {
            let meta = &cmd[1..];
            let mut parts = meta.split_whitespace();
            let verb = parts.next().unwrap_or("");
            match verb {
                "chk_vfo" => (RigctlAction::Reply(vec!["0".into()]), RigctlSerialOp::None),
                "get_powerstat" => (RigctlAction::Reply(vec!["1".into()]), RigctlSerialOp::None),
                "get_freq" => {
                    let hz = state.freq_hz.load(Ordering::Relaxed);
                    (
                        RigctlAction::Reply(vec![hz.to_string()]),
                        RigctlSerialOp::None,
                    )
                }
                "set_freq" => {
                    let args: Vec<&str> = parts.collect();
                    if let Some(hz) = freq_from_args(&args) {
                        set_frequency(state, hz)
                    } else {
                        (
                            RigctlAction::Reply(vec!["RPRT -1".into()]),
                            RigctlSerialOp::None,
                        )
                    }
                }
                "dump_state" => {
                    let lines = [
                        "0",
                        "0",
                        "0",
                        "0 0 0 0 0 0 0",
                        "0 0 0 0 0 0 0",
                        "0 0",
                        "0 0",
                        "0",
                        "0",
                        "0",
                        "0",
                        "0 0 0 0 0 0 0",
                        "0 0 0 0 0 0 0",
                        "0",
                        "0",
                        "0",
                        "0",
                        "0",
                        "0",
                    ];
                    (
                        RigctlAction::Reply(lines.iter().map(|s| (*s).to_string()).collect()),
                        RigctlSerialOp::None,
                    )
                }
                "dump_caps" => (
                    RigctlAction::Reply(vec!["RPRT 0".into()]),
                    RigctlSerialOp::None,
                ),
                _ => (
                    RigctlAction::Reply(vec!["RPRT 0".into()]),
                    RigctlSerialOp::None,
                ),
            }
        }
        'f' => {
            let hz = state.freq_hz.load(Ordering::Relaxed);
            (
                RigctlAction::Reply(vec![hz.to_string()]),
                RigctlSerialOp::None,
            )
        }
        'F' => {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if let Some(hz) = freq_from_args(&parts[1..]) {
                set_frequency(state, hz)
            } else {
                (
                    RigctlAction::Reply(vec!["RPRT -1".into()]),
                    RigctlSerialOp::None,
                )
            }
        }
        'm' => {
            let mode = state.mode();
            (
                RigctlAction::Reply(vec![
                    mode.as_str().to_string(),
                    mode.passband_hz().to_string(),
                ]),
                RigctlSerialOp::None,
            )
        }
        'M' => {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Some(mode) = RadioMode::from_hamlib_name(parts[1]) {
                    state.set_mode(mode);
                    return (
                        RigctlAction::Reply(vec!["RPRT 0".into()]),
                        RigctlSerialOp::SetMode(mode),
                    );
                }
                return (
                    RigctlAction::Reply(vec!["RPRT -1".into()]),
                    RigctlSerialOp::None,
                );
            }
            (
                RigctlAction::Reply(vec!["RPRT -1".into()]),
                RigctlSerialOp::None,
            )
        }
        'v' => (
            RigctlAction::Reply(vec!["VFOA".into()]),
            RigctlSerialOp::None,
        ),
        'V' => (
            RigctlAction::Reply(vec!["RPRT 0".into()]),
            RigctlSerialOp::None,
        ),
        't' => {
            let on = state.tx.load(Ordering::Relaxed);
            (
                RigctlAction::Reply(vec![if on { "1" } else { "0" }.into()]),
                RigctlSerialOp::None,
            )
        }
        'T' => {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if parts.len() >= 2 {
                let on = parts[1].parse::<i32>().map(|v| v != 0).unwrap_or(false);
                state.tx.store(on, Ordering::Relaxed);
                (
                    RigctlAction::Reply(vec!["RPRT 0".into()]),
                    RigctlSerialOp::SetPtt(on),
                )
            } else {
                (
                    RigctlAction::Reply(vec!["RPRT 0".into()]),
                    RigctlSerialOp::None,
                )
            }
        }
        'q' => (RigctlAction::Quit, RigctlSerialOp::None),
        _ => (
            RigctlAction::Reply(vec!["RPRT 0".into()]),
            RigctlSerialOp::None,
        ),
    }
}

fn apply_serial_op(
    ser: &Mutex<Box<dyn serialport::SerialPort + Send>>,
    op: RigctlSerialOp,
) {
    match op {
        RigctlSerialOp::None => {}
        RigctlSerialOp::SetFreq(hz) => {
            if let Ok(mut s) = ser.lock() {
                if let Err(e) = trusdx::set_vfo_a_frequency(&mut **s, hz) {
                    warn!(error = %e, hz, "rigctl: set frequency failed");
                }
            } else {
                warn!("rigctl: could not lock serial for set frequency");
            }
        }
        RigctlSerialOp::SetMode(mode) => {
            if let Ok(mut s) = ser.lock() {
                if let Err(e) = trusdx::send_cat_command(&mut **s, mode.cat_command()) {
                    warn!(error = %e, "rigctl: set mode failed");
                }
            } else {
                warn!("rigctl: could not lock serial for set mode");
            }
        }
        RigctlSerialOp::SetPtt(on) => {
            // Original behavior: PTT hits the port immediately, not via the CAT queue.
            if let Ok(mut s) = ser.lock() {
                let result = if on {
                    trusdx::start_transmit_baseband(&mut **s)
                } else {
                    trusdx::enable_streaming_speaker_off(&mut **s)
                };
                if let Err(e) = result {
                    warn!(error = %e, on, "rigctl: PTT serial command failed");
                }
            } else {
                warn!("rigctl: could not lock serial for PTT");
            }
        }
    }
}

fn handle_rigctl_client(
    mut stream: TcpStream,
    ser: Arc<Mutex<Box<dyn serialport::SerialPort + Send>>>,
    state: Arc<AppState>,
) {
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "rigctl: try_clone failed");
            return;
        }
    };
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).is_err() {
            break;
        }
        if line.is_empty() {
            break;
        }

        let (action, serial_op) = handle_rigctl_command(line.trim_end(), &state);
        apply_serial_op(&ser, serial_op);

        match action {
            RigctlAction::Reply(lines) => {
                for l in lines {
                    if let Err(e) = writeln!(stream, "{}", l) {
                        warn!(error = %e, "rigctl: write failed");
                        return;
                    }
                }
            }
            RigctlAction::Quit => {
                let _ = writeln!(stream, "RPRT 0");
                break;
            }
        }
    }
}

pub fn spawn_rigctl_server(
    ser: Arc<Mutex<Box<dyn serialport::SerialPort + Send>>>,
    state: Arc<AppState>,
) {
    let _ = std::process::Command::new("pkill")
        .args(["-f", "rigctl"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(|mut child| {
            let _ = child.try_wait();
        });

    let _ = std::process::Command::new("fuser")
        .args(["-k", "4532/tcp"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(|mut child| {
            let _ = child.try_wait();
        });

    if let Ok(output) = std::process::Command::new("lsof")
        .args(["-ti:4532"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .output()
    {
        if !output.stdout.is_empty() {
            let pid_str = String::from_utf8_lossy(&output.stdout);
            let pid = pid_str.trim();
            let current_pid = std::process::id().to_string();
            if pid != current_pid {
                let _ = std::process::Command::new("kill")
                    .args(["-9", pid])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .stdin(std::process::Stdio::null())
                    .spawn()
                    .map(|mut child| {
                        let _ = child.try_wait();
                    });
            }
        }
    }

    std::thread::spawn(move || {
        let addr = ("127.0.0.1", 4532);
        match TcpListener::bind(addr) {
            Ok(listener) => {
                for stream in listener.incoming().flatten() {
                    handle_rigctl_client(stream, ser.clone(), state.clone());
                }
            }
            Err(e) => {
                error!(error = %e, "rigctl: failed to bind 127.0.0.1:4532");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_get_freq() {
        // Arrange
        let state = AppState::new();
        state.freq_hz.store(14_074_000, Ordering::Relaxed);

        // Act
        let (action, op) = handle_rigctl_command("f", &state);

        // Assert
        assert_eq!(action, RigctlAction::Reply(vec!["14074000".into()]));
        assert_eq!(op, RigctlSerialOp::None);
    }

    #[test]
    fn handle_set_freq_returns_serial_op() {
        // Arrange
        let state = AppState::new();

        // Act
        let (action, op) = handle_rigctl_command("F 7074000", &state);

        // Assert
        assert_eq!(action, RigctlAction::Reply(vec!["RPRT 0".into()]));
        assert_eq!(state.freq_hz.load(Ordering::Relaxed), 7_074_000);
        assert_eq!(op, RigctlSerialOp::SetFreq(7_074_000));
    }

    #[test]
    fn handle_set_freq_float_rounds() {
        // Arrange
        let state = AppState::new();

        // Act
        let (action, op) = handle_rigctl_command("F 14074000.4", &state);

        // Assert
        assert_eq!(action, RigctlAction::Reply(vec!["RPRT 0".into()]));
        assert_eq!(op, RigctlSerialOp::SetFreq(14_074_000));
    }

    #[test]
    fn handle_set_freq_with_vfo_token() {
        // Arrange
        let state = AppState::new();

        // Act
        let (action, op) = handle_rigctl_command("F VFOA 14074000", &state);

        // Assert
        assert_eq!(action, RigctlAction::Reply(vec!["RPRT 0".into()]));
        assert_eq!(op, RigctlSerialOp::SetFreq(14_074_000));
    }

    #[test]
    fn handle_backslash_set_freq() {
        // Arrange
        let state = AppState::new();

        // Act
        let (action, op) = handle_rigctl_command("\\set_freq 7074000", &state);

        // Assert
        assert_eq!(action, RigctlAction::Reply(vec!["RPRT 0".into()]));
        assert_eq!(op, RigctlSerialOp::SetFreq(7_074_000));
    }

    #[test]
    fn handle_get_mode_uses_state() {
        // Arrange
        let state = AppState::new();
        state.set_mode(RadioMode::Lsb);

        // Act
        let (action, op) = handle_rigctl_command("m", &state);

        // Assert
        assert_eq!(
            action,
            RigctlAction::Reply(vec!["LSB".into(), "2400".into()])
        );
        assert_eq!(op, RigctlSerialOp::None);
    }

    #[test]
    fn handle_set_mode_returns_serial_op() {
        // Arrange
        let state = AppState::new();

        // Act
        let (action, op) = handle_rigctl_command("M CW 500", &state);

        // Assert
        assert_eq!(action, RigctlAction::Reply(vec!["RPRT 0".into()]));
        assert_eq!(state.mode(), RadioMode::Cw);
        assert_eq!(op, RigctlSerialOp::SetMode(RadioMode::Cw));
    }

    #[test]
    fn handle_ptt_on_returns_serial_op() {
        // Arrange
        let state = AppState::new();

        // Act
        let (action, op) = handle_rigctl_command("T 1", &state);

        // Assert
        assert_eq!(action, RigctlAction::Reply(vec!["RPRT 0".into()]));
        assert!(state.tx.load(Ordering::Relaxed));
        assert_eq!(op, RigctlSerialOp::SetPtt(true));
    }

    #[test]
    fn handle_quit() {
        // Arrange
        let state = AppState::new();

        // Act
        let (action, op) = handle_rigctl_command("q", &state);

        // Assert
        assert_eq!(action, RigctlAction::Quit);
        assert_eq!(op, RigctlSerialOp::None);
    }
}
