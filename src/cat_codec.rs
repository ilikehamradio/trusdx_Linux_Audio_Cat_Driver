//! Pure CAT / audio codec helpers (no serial or PulseAudio I/O).

pub const TEXT_BUF_MAX: usize = 64;
pub const AUDIO_SAMPLE_SIZE: usize = 48;
pub const I16_TO_F32_SCALE: f32 = 32768.0;
pub const TX_GAIN: f32 = 1.0;

/// Hamlib / display mode id matching Kenwood-style MD values used by trUSDX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RadioMode {
    Md0 = 0,
    Lsb = 1,
    Usb = 2,
    Cw = 3,
    Fm = 4,
    Am = 5,
}

impl RadioMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Md0,
            1 => Self::Lsb,
            3 => Self::Cw,
            4 => Self::Fm,
            5 => Self::Am,
            _ => Self::Usb,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Md0 => "MD0",
            Self::Lsb => "LSB",
            Self::Usb => "USB",
            Self::Cw => "CW",
            Self::Fm => "FM",
            Self::Am => "AM",
        }
    }

    pub fn passband_hz(self) -> u32 {
        match self {
            Self::Cw => 500,
            Self::Fm => 15000,
            Self::Am => 6000,
            _ => 2400,
        }
    }

    pub fn cat_command(self) -> &'static [u8] {
        match self {
            Self::Md0 => b"MD0;",
            Self::Lsb => b"MD1;",
            Self::Usb => b"MD2;",
            Self::Cw => b"MD3;",
            Self::Fm => b"MD4;",
            Self::Am => b"MD5;",
        }
    }

    /// Parse hamlib mode name (case-insensitive).
    pub fn from_hamlib_name(name: &str) -> Option<Self> {
        let n = name.trim();
        let upper = n.to_ascii_uppercase();
        match upper.as_str() {
            "LSB" => Some(Self::Lsb),
            "USB" | "PKTUSB" | "USB-D" => Some(Self::Usb),
            "CW" | "CWL" | "CW-L" | "CWU" | "CW-U" => Some(Self::Cw),
            "FM" | "WFM" => Some(Self::Fm),
            "AM" => Some(Self::Am),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatFrame {
    /// Start of inbound US audio stream (`US` with no `;` terminator yet).
    AudioStreamStart,
    Frequency(u64),
    Mode(RadioMode),
    Other,
}

/// Parse a complete CAT text frame (bytes up to and including `;`, or the raw `US` start).
pub fn parse_cat_text_frame(buf: &[u8]) -> CatFrame {
    if buf.len() == 2 && buf[0] == b'U' && buf[1] == b'S' {
        return CatFrame::AudioStreamStart;
    }
    if buf.len() >= 2 && buf[0] == b'F' && buf[1] == b'A' {
        if let Ok(text) = std::str::from_utf8(buf) {
            let digits: String = text
                .chars()
                .skip(2)
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(hz) = digits.parse::<u64>() {
                return CatFrame::Frequency(hz);
            }
        }
        return CatFrame::Other;
    }
    if buf.len() >= 2 && buf[0] == b'M' && buf[1] == b'D' {
        let code = buf.get(2).copied().unwrap_or(b'2');
        let mode = match code {
            b'0' => RadioMode::Md0,
            b'1' => RadioMode::Lsb,
            b'3' => RadioMode::Cw,
            b'4' => RadioMode::Fm,
            b'5' => RadioMode::Am,
            _ => RadioMode::Usb,
        };
        return CatFrame::Mode(mode);
    }
    CatFrame::Other
}

/// Format a set-frequency CAT command `FA` + 11 digits + `;`.
pub fn format_fa_command(hz: u64) -> Vec<u8> {
    format!("FA{:011};", hz).into_bytes()
}

/// Convert little-endian PCM bytes into i16 samples.
pub fn bytes_to_i16_le(bytes: &[u8], output: &mut [i16]) {
    for (index, chunk) in bytes.chunks_exact(2).enumerate() {
        if index >= output.len() {
            break;
        }
        output[index] = i16::from_le_bytes([chunk[0], chunk[1]]);
    }
}

/// RMS of i16 samples normalized to 0.0–1.0.
pub fn calculate_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples
        .iter()
        .map(|&sample_value| {
            let normalized_sample = sample_value as f32 / I16_TO_F32_SCALE;
            normalized_sample * normalized_sample
        })
        .sum();
    (sum_sq / samples.len() as f32).sqrt().min(1.0)
}

/// Encode i16 PCM to 8-bit unsigned CAT audio, escaping `;` → `:`.
pub fn encode_tx_samples(samples: &[i16], gain: f32, out: &mut [u8]) {
    let n = samples.len().min(out.len());
    for i in 0..n {
        let scaled = (samples[i] as f32 * gain).clamp(-32768.0, 32767.0) as i16;
        let mut byte = (128i16 + (scaled / 256)).clamp(0, 255) as u8;
        if byte == b';' {
            byte = b':';
        }
        out[i] = byte;
    }
}

/// Push a byte into a text buffer; clear if over cap (resync).
pub fn push_text_byte(text_buf: &mut Vec<u8>, byte: u8) {
    text_buf.push(byte);
    if text_buf.len() > TEXT_BUF_MAX {
        text_buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cat_text_frame_detects_us_start() {
        // Arrange
        let buf = b"US";

        // Act
        let frame = parse_cat_text_frame(buf);

        // Assert
        assert_eq!(frame, CatFrame::AudioStreamStart);
    }

    #[test]
    fn parse_cat_text_frame_parses_frequency() {
        // Arrange
        let buf = b"FA00014074000;";

        // Act
        let frame = parse_cat_text_frame(buf);

        // Assert
        assert_eq!(frame, CatFrame::Frequency(14_074_000));
    }

    #[test]
    fn parse_cat_text_frame_parses_mode_usb() {
        // Arrange
        let buf = b"MD2;";

        // Act
        let frame = parse_cat_text_frame(buf);

        // Assert
        assert_eq!(frame, CatFrame::Mode(RadioMode::Usb));
    }

    #[test]
    fn format_fa_command_pads_eleven_digits() {
        // Arrange
        let hz = 7_074_000u64;

        // Act
        let cmd = format_fa_command(hz);

        // Assert
        assert_eq!(std::str::from_utf8(&cmd).unwrap(), "FA00007074000;");
    }

    #[test]
    fn encode_tx_samples_escapes_semicolon() {
        // Arrange
        // Choose sample so 128 + sample/256 == b';' (59)
        // 128 + s/256 = 59 => s/256 = -69 => s = -69*256 = -17664
        let samples = [-17664i16];
        let mut out = [0u8; 1];

        // Act
        encode_tx_samples(&samples, 1.0, &mut out);

        // Assert
        assert_eq!(out[0], b':');
    }

    #[test]
    fn encode_tx_samples_midscale_silence() {
        // Arrange
        let samples = [0i16; 4];
        let mut out = [0u8; 4];

        // Act
        encode_tx_samples(&samples, 1.0, &mut out);

        // Assert
        assert_eq!(out, [128, 128, 128, 128]);
    }

    #[test]
    fn calculate_rms_zero_for_silence() {
        // Arrange
        let samples = [0i16; 8];

        // Act
        let rms = calculate_rms(&samples);

        // Assert
        assert!((rms - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn radio_mode_from_hamlib_name_maps_common_aliases() {
        // Arrange / Act / Assert
        assert_eq!(RadioMode::from_hamlib_name("usb"), Some(RadioMode::Usb));
        assert_eq!(RadioMode::from_hamlib_name("PKTUSB"), Some(RadioMode::Usb));
        assert_eq!(RadioMode::from_hamlib_name("lsb"), Some(RadioMode::Lsb));
        assert_eq!(RadioMode::from_hamlib_name("CW"), Some(RadioMode::Cw));
        assert_eq!(RadioMode::from_hamlib_name("nope"), None);
    }

    #[test]
    fn push_text_byte_clears_when_over_cap() {
        // Arrange
        let mut buf = vec![0u8; TEXT_BUF_MAX];

        // Act
        push_text_byte(&mut buf, b'X');

        // Assert
        assert!(buf.is_empty());
    }

    #[test]
    fn bytes_to_i16_le_decodes_little_endian() {
        // Arrange
        let bytes = [0x34u8, 0x12, 0xFF, 0x7F];
        let mut out = [0i16; 2];

        // Act
        bytes_to_i16_le(&bytes, &mut out);

        // Assert
        assert_eq!(out[0], 0x1234);
        assert_eq!(out[1], 0x7FFF);
    }
}
