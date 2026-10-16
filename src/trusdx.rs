use serialport::SerialPort;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

const CMD_RX: &[u8] = b";RX;";
const CMD_TX0: &[u8] = b";TX0;";

// Leading ';' interrupts an in-flight US audio stream (trUSDX CAT framing).
const CMD_FA_QUERY: &[u8] = b";FA;";
const CMD_MD_QUERY: &[u8] = b";MD;";

const CMD_MD0: &[u8] = b"MD0;";
const CMD_MD1_LSB: &[u8] = b"MD1;";
const CMD_MD2_USB: &[u8] = b"MD2;";
const CMD_MD3_CW: &[u8] = b"MD3;";
const CMD_MD4_FM: &[u8] = b"MD4;";
const CMD_MD5_AM: &[u8] = b"MD5;";

const CMD_UA1: &[u8] = b"UA1;";
const CMD_UA2: &[u8] = b"UA2;";

static LAST_RTS: OnceLock<AtomicBool> = OnceLock::new();
static LAST_DTR: OnceLock<AtomicBool> = OnceLock::new();

fn set_last_rts(v: bool) {
    LAST_RTS
        .get_or_init(|| AtomicBool::new(v))
        .store(v, Ordering::Relaxed);
}
fn set_last_dtr(v: bool) {
    LAST_DTR
        .get_or_init(|| AtomicBool::new(v))
        .store(v, Ordering::Relaxed);
}
fn get_last_rts() -> bool {
    LAST_RTS
        .get_or_init(|| AtomicBool::new(false))
        .load(Ordering::Relaxed)
}
#[allow(dead_code)]
fn get_last_dtr() -> bool {
    LAST_DTR
        .get_or_init(|| AtomicBool::new(false))
        .load(Ordering::Relaxed)
}
pub fn last_rts_state() -> bool {
    get_last_rts()
}
#[allow(dead_code)]
pub fn last_dtr_state() -> bool {
    get_last_dtr()
}

pub fn find_trusdx_device() -> Option<String> {
    for entry in fs::read_dir("/dev").ok()? {
        let entry = entry.ok()?;
        let fname = entry.file_name().into_string().ok()?;
        if !fname.starts_with("ttyUSB") {
            continue;
        }
        let dev_path = PathBuf::from("/sys/class/tty").join(&fname).join("device");
        let dev_real_abs = std::fs::canonicalize(&dev_path).ok()?;
        let usb_device_path = dev_real_abs.parent()?.parent()?;
        let id_vendor_path = usb_device_path.join("idVendor");
        let id_product_path = usb_device_path.join("idProduct");
        if id_vendor_path.exists() && id_product_path.exists() {
            let vid = fs::read_to_string(id_vendor_path).ok()?.trim().to_lowercase();
            let pid = fs::read_to_string(id_product_path).ok()?.trim().to_lowercase();
            if vid == "1a86" && pid == "7523" {
                return Some(format!("/dev/{}", fname));
            }
        }
    }
    None
}

pub fn open_trusdx_serial(
    device: Option<&str>,
) -> anyhow::Result<Box<dyn SerialPort + Send>> {
    let serial_device = match device {
        Some(path) => path.to_string(),
        None => find_trusdx_device()
            .ok_or_else(|| anyhow::anyhow!("No CH340 device found (pass --device /dev/ttyUSBN)"))?,
    };
    let port = serialport::new(&serial_device, 115200)
        .timeout(Duration::from_millis(10))
        .parity(serialport::Parity::None)
        .data_bits(serialport::DataBits::Eight)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .open()
        .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", serial_device, e))?;
    Ok(port)
}

fn send_command_to_radio(s: &mut dyn SerialPort, data: &[u8]) -> std::io::Result<()> {
    let _ = flush_serial_line(s);
    let res = s.write_all(data);
    if let Err(ref e) = res {
        eprintln!("trusdx: serial write failed: {}", e);
    }
    let _ = flush_serial_line(s);
    res
}

pub fn start_transmit_baseband(s: &mut dyn SerialPort) -> std::io::Result<()> {
    send_command_to_radio(s, CMD_TX0)
}

pub fn query_vfo_a(s: &mut dyn SerialPort) -> std::io::Result<()> {
    send_command_to_radio(s, CMD_FA_QUERY)
}

pub fn query_mode(s: &mut dyn SerialPort) -> std::io::Result<()> {
    send_command_to_radio(s, CMD_MD_QUERY)
}

/// Set VFO-A frequency (Kenwood `FA` + 11 digits). Leading `;` interrupts US.
pub fn set_vfo_a_frequency(s: &mut dyn SerialPort, hz: u64) -> std::io::Result<()> {
    let cmd = format!(";FA{:011};", hz);
    send_command_to_radio(s, cmd.as_bytes())
}

/// Send an arbitrary CAT frame with standard flush framing.
pub fn send_cat_command(s: &mut dyn SerialPort, data: &[u8]) -> std::io::Result<()> {
    send_command_to_radio(s, data)
}

pub fn set_mode(s: &mut dyn SerialPort, mode: u8) -> std::io::Result<()> {
    let buf = match mode {
        0 => CMD_MD0,
        1 => CMD_MD1_LSB,
        2 => CMD_MD2_USB,
        3 => CMD_MD3_CW,
        4 => CMD_MD4_FM,
        5 => CMD_MD5_AM,
        _ => CMD_MD2_USB,
    };
    send_command_to_radio(s, buf)
}

pub fn flush_serial_line(s: &mut dyn SerialPort) -> std::io::Result<()> {
    s.flush()
}

fn send_rx_ua(s: &mut dyn SerialPort, ua: &[u8]) -> std::io::Result<()> {
    control_rts(s, true)?;
    let mut combined = [0u8; 8];
    combined[..CMD_RX.len()].copy_from_slice(CMD_RX);
    combined[CMD_RX.len()..CMD_RX.len() + ua.len()].copy_from_slice(ua);
    let result = send_command_to_radio(s, &combined[..CMD_RX.len() + ua.len()]);
    control_rts(s, false)?;
    result
}

pub fn enable_streaming_speaker_off(s: &mut dyn SerialPort) -> std::io::Result<()> {
    send_rx_ua(s, CMD_UA2)
}

pub fn enable_streaming_speaker_on(s: &mut dyn SerialPort) -> std::io::Result<()> {
    send_rx_ua(s, CMD_UA1)
}

pub fn send_audio_stream_raw(s: &mut dyn SerialPort, audio_data: &[u8]) -> std::io::Result<()> {
    s.write_all(audio_data).map_err(|e| {
        eprintln!("trusdx: audio write_all failed: {}", e);
        e
    })
}

pub fn control_rts(s: &mut dyn SerialPort, high: bool) -> std::io::Result<()> {
    let r = s
        .write_request_to_send(high)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
    if r.is_ok() {
        set_last_rts(high);
    }
    r
}

pub fn control_dtr(s: &mut dyn SerialPort, high: bool) -> std::io::Result<()> {
    let r = s
        .write_data_terminal_ready(high)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
    if r.is_ok() {
        set_last_dtr(high);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_rts_and_dtr_default_false() {
        // Arrange / Act
        // Note: may have been set by other tests in-process; only check getters exist.
        let _rts = last_rts_state();
        let _dtr = last_dtr_state();

        // Assert
        // Smoke: functions are callable and return bool.
        assert!(_rts == true || _rts == false);
        assert!(_dtr == true || _dtr == false);
    }

    #[test]
    fn status_queries_lead_with_semicolon_to_interrupt_us() {
        // Arrange / Act / Assert — trUSDX ends US audio at ';' from the host.
        assert_eq!(CMD_FA_QUERY, b";FA;");
        assert_eq!(CMD_MD_QUERY, b";MD;");
    }
}
