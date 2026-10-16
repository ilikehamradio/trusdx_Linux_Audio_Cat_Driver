use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use trusdx_linux_driver::audio;
use trusdx_linux_driver::cli;
use trusdx_linux_driver::gui;
use trusdx_linux_driver::rigctl;
use trusdx_linux_driver::serial_io;
use trusdx_linux_driver::shutdown;
use trusdx_linux_driver::state::AppState;
use trusdx_linux_driver::trusdx;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = cli::parse_args()?;

    audio::cleanup_trusdx_audio_interfaces();

    const AUDIO_TX_RATE: u32 = 11520;
    const AUDIO_RX_RATE: u32 = 7812;

    if audio::create_trusdx_audio_interface(AUDIO_TX_RATE).is_none() {
        warn!("failed to create TRUSDX PulseAudio null-sink");
    }

    let mut port = trusdx::open_trusdx_serial(args.device.as_deref())?;

    if let Err(e) = trusdx::control_rts(&mut *port, false) {
        warn!(error = %e, "RTS low failed");
    }
    if let Err(e) = trusdx::control_dtr(&mut *port, true) {
        warn!(error = %e, "DTR high failed");
    }
    let ser: Arc<Mutex<Box<dyn serialport::SerialPort + Send>>> = Arc::new(Mutex::new(port));

    {
        let mut s = ser.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = trusdx::enable_streaming_speaker_off(&mut **s) {
            warn!(error = %e, "enable streaming failed");
        }
        if let Err(e) = trusdx::set_mode(&mut **s, 2) {
            warn!(error = %e, "set mode USB failed");
        }
    }

    let state = AppState::new();

    // Legacy pre-bridge wait (left in place by design).
    {
        let mut attempts = 0;
        loop {
            let start = std::time::Instant::now();
            while !state.streaming_started.load(std::sync::atomic::Ordering::Relaxed)
                && start.elapsed() < Duration::from_millis(250)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            if state
                .streaming_started
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                break;
            }
            if attempts >= 2 {
                break;
            }
            attempts += 1;
            if let Ok(mut s) = ser.lock() {
                let _ = trusdx::enable_streaming_speaker_off(&mut **s);
            }
        }
    }

    let audio_handles = audio::setup_pulseaudio(AUDIO_RX_RATE, AUDIO_TX_RATE)?;
    let audio_thread = audio::run_audio_bridge(ser.clone(), audio_handles, state.clone());
    let _status_poller = serial_io::spawn_status_poller(state.clone());

    rigctl::spawn_rigctl_server(ser.clone(), state.clone());

    {
        let state_c = state.clone();
        let ser_c = ser.clone();
        if let Err(e) = ctrlc::set_handler(move || {
            shutdown::shutdown(state_c.clone(), ser_c.clone());
        }) {
            warn!(error = %e, "could not install Ctrl-C handler");
        }
    }

    cli::spawn_esc_handler(state.clone(), ser.clone());

    if args.no_gui {
        info!("starting CLI-only meters (--no-gui)");
        cli::run_meter_loop(state.clone());
    } else {
        // GUI/tray mode: do not run CLI meters — relative/absolute redraw fights
        // tracing/debug output and garbles the terminal.
        match gui::run_gui(state.clone(), ser.clone()) {
            Ok(()) => {}
            Err(e) => {
                warn!(error = %e, "GUI unavailable; falling back to CLI meters");
                cli::run_meter_loop(state.clone());
            }
        }
    }

    state
        .stop_audio
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = audio_thread.join();

    print!("\x1B[2J\x1B[H");
    std::io::Write::flush(&mut std::io::stdout()).ok();
    audio::cleanup_trusdx_audio_interfaces();
    info!("shutdown complete");
    Ok(())
}
