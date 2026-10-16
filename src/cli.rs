use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use termios::{tcsetattr, Termios, ECHO, ICANON, TCSANOW, VMIN, VTIME};

use crate::state::AppState;

const METER_INTERVAL_MS: u64 = 50;

pub struct Args {
    pub device: Option<String>,
    pub no_gui: bool,
}

pub fn print_help() {
    println!(
        "\
trusdx_linux_driver — trUSDX Linux audio / CAT bridge

Usage:
  trusdx_linux_driver [OPTIONS]

Options:
  -d, --device <PATH>   Serial device (default: auto-detect CH340)
      --no-gui          CLI meters only (no Slint window)
  -h, --help            Show this help
"
    );
}

pub fn parse_args() -> anyhow::Result<Args> {
    let mut pargs = pico_args::Arguments::from_env();
    if pargs.contains(["-h", "--help"]) {
        print_help();
        std::process::exit(0);
    }
    let args = Args {
        device: pargs.opt_value_from_str(["-d", "--device"])?,
        no_gui: pargs.contains("--no-gui"),
    };
    let remaining = pargs.finish();
    if !remaining.is_empty() {
        anyhow::bail!(
            "unexpected arguments: {:?}\n\n{}",
            remaining,
            "Try --help for usage."
        );
    }
    Ok(args)
}

/// Ignore job-control signals so tcsetattr/read on the TTY do not stop the
/// process when it is not in the foreground (common under debuggers).
pub fn ignore_background_tty_signals() {
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::signal(libc::SIGTTIN, libc::SIG_IGN);
    }
}

pub fn is_tty_fd(fd: RawFd) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

struct TermiosGuard {
    fd: RawFd,
    orig: Termios,
}

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        let _ = tcsetattr(self.fd, TCSANOW, &self.orig);
    }
}

/// Fill a fixed 52-byte meter bar: `[` + 50 chars + `]`.
pub fn fill_level_bar(level: f32, buf: &mut [u8; 52]) {
    buf[0] = b'[';
    buf[51] = b']';
    let filled = ((level.clamp(0.0, 1.0)) * 50.0) as usize;
    for i in 0..50 {
        buf[1 + i] = if i < filled { b'#' } else { b'-' };
    }
}

pub fn spawn_esc_handler(
    state: Arc<AppState>,
    ser: Arc<Mutex<Box<dyn serialport::SerialPort + Send>>>,
) {
    thread::spawn(move || {
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/tty")
            .or_else(|_| std::fs::OpenOptions::new().read(true).open("/dev/stdin"));
        if let Ok(mut tty_file) = tty {
            let fd = tty_file.as_raw_fd();
            if !is_tty_fd(fd) {
                return;
            }
            ignore_background_tty_signals();
            if let Ok(mut term) = Termios::from_fd(fd) {
                let orig = term;
                term.c_lflag &= !(ICANON | ECHO);
                term.c_cc[VMIN] = 1;
                term.c_cc[VTIME] = 0;
                if tcsetattr(fd, TCSANOW, &term).is_err() {
                    return;
                }
                let _guard = TermiosGuard { fd, orig };
                let mut buf = [0u8; 1];
                loop {
                    if state.shutting_down.load(Ordering::Relaxed) {
                        break;
                    }
                    if tty_file.read(&mut buf).ok() == Some(1) && buf[0] == 0x1B {
                        crate::shutdown::shutdown(state.clone(), ser.clone());
                        break;
                    }
                }
            }
        }
    });
}

pub fn print_console_header() {
    // Clear screen and park the cursor; meters always redraw at fixed rows 1–4.
    print!("\x1B[2J\x1B[H");
    std::io::Write::flush(&mut std::io::stdout()).ok();
}

pub fn render_levels(
    input_level: f32,
    output_level: f32,
    freq_hz: u64,
    mode: &str,
    tx_now: bool,
) {
    let mut in_bar = [0u8; 52];
    let mut out_bar = [0u8; 52];
    fill_level_bar(input_level, &mut in_bar);
    fill_level_bar(output_level, &mut out_bar);
    let in_bar_s = std::str::from_utf8(&in_bar).unwrap_or("[----]");
    let out_bar_s = std::str::from_utf8(&out_bar).unwrap_or("[----]");
    let freq_mhz = (freq_hz as f64) / 1_000_000.0f64;
    let rts = if crate::trusdx::last_rts_state() {
        "H"
    } else {
        "L"
    };

    // Absolute cursor addressing — relative \x1B[nF breaks when logs/debug print.
    print!("\x1B[1;1H\x1B[2KINPUT  {} {:5.1}%\n", in_bar_s, input_level * 100.0);
    print!("\x1B[2;1H\x1B[2KOUTPUT {} {:5.1}%\n", out_bar_s, output_level * 100.0);
    print!(
        "\x1B[3;1H\x1B[2KMODE: {} FREQ: {:.5} MHz STATE: {} RTS:{}\n",
        mode,
        freq_mhz,
        if tx_now { "TX" } else { "RX" },
        rts
    );
    print!("\x1B[4;1H\x1B[2KPress ESC to exit\n");
    std::io::Write::flush(&mut std::io::stdout()).ok();
}

/// Terminal meter loop. Returns when `shutting_down` is set.
///
/// VFO/mode status polling lives in [`crate::serial_io::spawn_status_poller`].
pub fn run_meter_loop(state: Arc<AppState>) {
    print_console_header();

    loop {
        if state.shutting_down.load(Ordering::Relaxed) {
            break;
        }
        let in_lvl = state.input_level();
        let out_lvl = state.output_level();
        let freq = state.freq_hz.load(Ordering::Relaxed);
        let mode = state.mode().as_str().to_string();
        let tx_now = state.tx.load(Ordering::Relaxed);
        render_levels(in_lvl, out_lvl, freq, &mode, tx_now);
        thread::sleep(Duration::from_millis(METER_INTERVAL_MS));
    }
}

pub fn spawn_meter_loop(state: Arc<AppState>) {
    thread::spawn(move || {
        run_meter_loop(state);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn is_tty_fd_returns_false_for_dev_null() {
        // Arrange
        let file = File::open("/dev/null").expect("open /dev/null");
        let fd = file.as_raw_fd();

        // Act
        let result = is_tty_fd(fd);

        // Assert
        assert!(!result);
    }

    #[test]
    fn ignore_background_tty_signals_does_not_panic() {
        // Arrange / Act
        ignore_background_tty_signals();

        // Assert
        ignore_background_tty_signals();
    }

    #[test]
    fn fill_level_bar_empty_and_full() {
        // Arrange
        let mut buf = [0u8; 52];

        // Act
        fill_level_bar(0.0, &mut buf);
        let empty = buf;
        fill_level_bar(1.0, &mut buf);
        let full = buf;

        // Assert
        assert_eq!(empty[0], b'[');
        assert_eq!(empty[51], b']');
        assert!(empty[1..51].iter().all(|&c| c == b'-'));
        assert!(full[1..51].iter().all(|&c| c == b'#'));
    }

    #[test]
    fn fill_level_bar_half() {
        // Arrange
        let mut buf = [0u8; 52];

        // Act
        fill_level_bar(0.5, &mut buf);

        // Assert
        assert_eq!(&buf[1..26], &[b'#'; 25]);
        assert_eq!(&buf[26..51], &[b'-'; 25]);
    }
}
