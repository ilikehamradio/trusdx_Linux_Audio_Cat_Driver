//! Slint GUI + system tray (start hidden; close hides to tray).

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{CloseRequestResponse, ComponentHandle, Timer, TimerMode};
use tracing::{error, info, warn};

use crate::shutdown;
use crate::state::AppState;

slint::include_modules!();

/// Hover text for the system tray icon (TX / RX / FREQ, one per line).
pub fn format_tray_tooltip(tx_level: f32, rx_level: f32, freq_hz: u64) -> String {
    let tx_pct = tx_level.clamp(0.0, 1.0) * 100.0;
    let rx_pct = rx_level.clamp(0.0, 1.0) * 100.0;
    let freq_mhz = (freq_hz as f64) / 1_000_000.0;
    format!(
        "RX: {:.1}%\nTX: {:.1}%\nFREQ: {:.5} MHz",
        rx_pct, tx_pct, freq_mhz
    )
}

/// Run the Slint UI on the calling thread (process main thread).
///
/// Starts minimized to the system tray. Closing the window hides it; use the
/// tray menu (or left-click) to restore, or Quit to shut down fully.
pub fn run_gui(
    state: Arc<AppState>,
    ser: Arc<Mutex<Box<dyn serialport::SerialPort + Send>>>,
) -> Result<(), String> {
    let display = std::env::var("DISPLAY").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    if display.is_none() && wayland.is_none() {
        return Err(
            "Neither DISPLAY nor WAYLAND_DISPLAY is set. Cannot initialize GUI.".to_string(),
        );
    }

    let ui = MainWindow::new().map_err(|e| format!("Failed to create Slint window: {e}"))?;
    let tray = AppTray::new().map_err(|e| format!("Failed to create system tray icon: {e}"))?;

    tray.set_tray_tooltip(format_tray_tooltip(0.0, 0.0, 0).into());
    tray.set_tray_title("trUSDX".into());
    tray.set_tray_visible(true);

    // Close → hide to tray (driver keeps running). Match Slint's HideWindow semantics.
    ui.window().on_close_requested(|| CloseRequestResponse::HideWindow);

    // Tray toggle / show — same pattern as Slint examples/system_tray.
    let ui_toggle = ui.as_weak();
    tray.on_toggle_window(move || {
        let Some(w) = ui_toggle.upgrade() else {
            warn!("tray toggle: main window handle gone");
            return;
        };
        if w.window().is_visible() {
            if let Err(e) = w.hide() {
                warn!(error = %e, "tray: hide failed");
            }
        } else if let Err(e) = w.show() {
            warn!(error = %e, "tray: show failed");
        }
    });

    let state_quit = state.clone();
    let ser_quit = ser.clone();
    tray.on_quit(move || {
        shutdown::shutdown(state_quit.clone(), ser_quit.clone());
        let _ = slint::quit_event_loop();
    });

    let ui_weak = ui.as_weak();
    let tray_weak = tray.as_weak();
    let state_tick = state.clone();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };

        if state_tick.shutting_down.load(Ordering::Relaxed) {
            let _ = ui.hide();
            let _ = slint::quit_event_loop();
            return;
        }

        let in_lvl = state_tick.input_level().clamp(0.0, 1.0);
        let out_lvl = state_tick.output_level().clamp(0.0, 1.0);
        ui.set_rx_level(in_lvl);
        ui.set_rx_text(format!("{:.1}%", in_lvl * 100.0).into());
        ui.set_tx_level(out_lvl);
        ui.set_tx_text(format!("{:.1}%", out_lvl * 100.0).into());

        let freq = state_tick.freq_hz.load(Ordering::Relaxed);
        let freq_mhz = (freq as f64) / 1_000_000.0f64;
        ui.set_freq_text(format!("{:.5} MHz", freq_mhz).into());
        ui.set_mode_text(state_tick.mode().as_str().into());
        ui.set_state_text(if state_tick.tx.load(Ordering::Relaxed) {
            "TX".into()
        } else {
            "RX".into()
        });

        if let Some(tray) = tray_weak.upgrade() {
            tray.set_tray_tooltip(format_tray_tooltip(out_lvl, in_lvl, freq).into());
        }
    });

    let _timer = timer;

    // Create the native window, then hide on the next event-loop tick so we
    // start in the tray without racing show/hide before the loop runs.
    if let Err(e) = ui.show() {
        return Err(format!("Failed to show main window: {e}"));
    }
    let ui_start_hide = ui.as_weak();
    Timer::single_shot(Duration::from_millis(50), move || {
        if let Some(w) = ui_start_hide.upgrade() {
            let _ = w.hide();
            info!("window hidden — use tray to show / quit");
        }
    });

    // Keep tray alive for the lifetime of the event loop.
    let _tray = tray;

    if let Err(e) = slint::run_event_loop() {
        error!(error = %e, "Slint event loop failed");
        return Err(format!("Slint run failed: {e}"));
    }

    if !state.shutting_down.load(Ordering::Relaxed) {
        warn!("GUI event loop exited without shutdown flag; requesting shutdown");
        shutdown::shutdown(state, ser);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tray_tooltip_separate_lines() {
        // Arrange
        let tx = 0.125;
        let rx = 0.202;
        let freq_hz = 14_074_000u64;

        // Act
        let tip = format_tray_tooltip(tx, rx, freq_hz);

        // Assert
        let lines: Vec<&str> = tip.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "RX: 20.2%");
        assert_eq!(lines[1], "TX: 12.5%");
        assert_eq!(lines[2], "FREQ: 14.07400 MHz");
    }

    #[test]
    fn format_tray_tooltip_clamps_levels() {
        // Arrange / Act
        let tip = format_tray_tooltip(2.0, -0.5, 0);

        // Assert
        let lines: Vec<&str> = tip.lines().collect();
        assert_eq!(lines[0], "RX: 0.0%");
        assert_eq!(lines[1], "TX: 100.0%");
        assert_eq!(lines[2], "FREQ: 0.00000 MHz");
    }
}
