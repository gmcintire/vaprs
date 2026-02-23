// AX.25 binary frame <-> TNC2 text format conversion
// Ported from aprx ax25.c

use crate::error::VaprsError;

/// Maximum number of via/digipeater addresses in an AX.25 frame.
const MAX_VIA_ADDRS: usize = 8;

/// Minimum AX.25 frame length: 7 dst + 7 src + 1 control + 1 PID = 16 bytes.
const MIN_FRAME_LEN: usize = 16;

/// Validate that a character is a valid AX.25 callsign character (A-Z, 0-9).
fn is_valid_call_char(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

/// Encode a callsign string (e.g., "OH2MQK" or "OH2MQK-15") into a 7-byte AX.25 address field.
///
/// The `ssid_flags` parameter provides the base flags byte (typically 0x60 for source/via,
/// 0xE0 for destination). The SSID bits will be OR'd in.
pub fn encode_ax25_address(callsign: &str, ssid_flags: u8) -> Result<[u8; 7], VaprsError> {
    let (call_part, ssid) = if let Some(idx) = callsign.find('-') {
        let call = &callsign[..idx];
        let ssid_str = &callsign[idx + 1..];
        let ssid: u8 = ssid_str
            .parse()
            .map_err(|_| VaprsError::Ax25(format!("invalid SSID: {}", ssid_str)))?;
        if ssid > 15 {
            return Err(VaprsError::Ax25(format!("SSID {} out of range 0-15", ssid)));
        }
        (call, ssid)
    } else {
        (callsign, 0u8)
    };

    if call_part.is_empty() || call_part.len() > 6 {
        return Err(VaprsError::Ax25(format!(
            "callsign '{}' must be 1-6 characters",
            call_part
        )));
    }

    for c in call_part.chars() {
        if !is_valid_call_char(c) {
            return Err(VaprsError::Ax25(format!(
                "invalid character '{}' in callsign '{}'",
                c, call_part
            )));
        }
    }

    let mut addr = [0u8; 7];
    let call_bytes = call_part.as_bytes();

    // Fill 6 callsign bytes, shifted left by 1, padded with spaces
    for (i, slot) in addr[..6].iter_mut().enumerate() {
        let ch = if i < call_bytes.len() {
            call_bytes[i]
        } else {
            b' '
        };
        *slot = ch << 1;
    }

    // SSID byte: flags | (ssid << 1)
    addr[6] = ssid_flags | (ssid << 1);

    Ok(addr)
}

/// Decode a 7-byte AX.25 address field back into a callsign string.
///
/// If `mark_hbit` is true and the H-bit (bit 7 of SSID byte) is set,
/// append '*' to indicate the address has been digipeated.
///
/// Returns (callsign_string, raw_ssid_byte).
pub fn decode_ax25_address(ax25: &[u8; 7], mark_hbit: bool) -> Result<(String, u8), VaprsError> {
    let mut call = String::with_capacity(10);

    // Extract callsign characters (shift right by 1, trim trailing spaces)
    for &byte in &ax25[..6] {
        let ch = byte >> 1;
        if ch != b' ' {
            call.push(ch as char);
        }
    }

    if call.is_empty() {
        return Err(VaprsError::Ax25("empty callsign in AX.25 address".into()));
    }

    let ssid_byte = ax25[6];
    let ssid = (ssid_byte >> 1) & 0x0F;

    if ssid != 0 {
        call.push('-');
        call.push_str(&ssid.to_string());
    }

    // Mark digipeated addresses with '*'
    if mark_hbit && (ssid_byte & 0x80) != 0 {
        call.push('*');
    }

    Ok((call, ssid_byte))
}

/// Result of converting an AX.25 binary frame to TNC2 text format.
#[derive(Debug, Clone)]
pub struct Tnc2Frame {
    /// TNC2-format string: "SOURCE>DEST,VIA1,VIA2:payload"
    pub tnc2: String,
    /// Length of the address portion (up to but not including ':')
    pub tnc2_addr_len: usize,
    /// Length of AX.25 address fields in the binary frame
    pub ax25_addr_len: usize,
    /// Whether this is an APRS frame (UI frame with PID 0xF0)
    pub is_aprs: bool,
    /// UI PID value, or -1 if not a UI frame
    pub ui_pid: i16,
}

/// Convert an AX.25 binary frame to TNC2 text format.
///
/// Format: SOURCE>DESTINATION,VIA1,VIA2:payload
///
/// The binary frame layout is:
/// - bytes [0..7]:   destination address
/// - bytes [7..14]:  source address
/// - bytes [14..]:   via addresses (7 bytes each), until address-end bit is set
/// - control byte
/// - PID byte
/// - payload
pub fn ax25_to_tnc2(frame: &[u8]) -> Result<Tnc2Frame, VaprsError> {
    if frame.len() < MIN_FRAME_LEN {
        return Err(VaprsError::Ax25(format!(
            "frame too short: {} bytes (minimum {})",
            frame.len(),
            MIN_FRAME_LEN
        )));
    }

    // Decode destination address (bytes 0-6)
    let dst_bytes: &[u8; 7] = frame[0..7].try_into().unwrap();
    let (dst_call, _) = decode_ax25_address(dst_bytes, false)?;

    // Decode source address (bytes 7-13)
    let src_bytes: &[u8; 7] = frame[7..14].try_into().unwrap();
    let (src_call, src_ssid) = decode_ax25_address(src_bytes, false)?;

    // Build TNC2 address: SOURCE>DESTINATION
    let mut tnc2 = String::with_capacity(256);
    tnc2.push_str(&src_call);
    tnc2.push('>');
    tnc2.push_str(&dst_call);

    // Track the end of address fields in the binary frame
    let mut addr_end = 14;
    let mut last_addr_found = (src_ssid & 0x01) != 0;

    // Decode via addresses
    let mut via_count = 0;
    while !last_addr_found {
        if via_count >= MAX_VIA_ADDRS {
            return Err(VaprsError::Ax25("too many via addresses (max 8)".into()));
        }

        if addr_end + 7 > frame.len() {
            return Err(VaprsError::Ax25(
                "frame truncated in via address field".into(),
            ));
        }

        let via_bytes: &[u8; 7] = frame[addr_end..addr_end + 7].try_into().unwrap();
        let (via_call, via_ssid) = decode_ax25_address(via_bytes, true)?;

        tnc2.push(',');
        tnc2.push_str(&via_call);

        addr_end += 7;
        via_count += 1;
        last_addr_found = (via_ssid & 0x01) != 0;
    }

    let ax25_addr_len = addr_end;

    // Need at least control + PID bytes after addresses
    if addr_end + 2 > frame.len() {
        return Err(VaprsError::Ax25(
            "frame truncated after address fields".into(),
        ));
    }

    let tnc2_addr_len = tnc2.len();

    let control = frame[addr_end];
    let pid = frame[addr_end + 1];

    // Check for UI frame (control byte 0x03)
    if control != 0x03 {
        // Not a UI frame: return address part only
        tnc2.push(':');
        return Ok(Tnc2Frame {
            tnc2,
            tnc2_addr_len,
            ax25_addr_len,
            is_aprs: false,
            ui_pid: -1,
        });
    }

    let is_aprs = pid == 0xF0;

    // Append payload after control + PID bytes
    tnc2.push(':');
    let payload_start = addr_end + 2;
    if payload_start < frame.len() {
        let payload = &frame[payload_start..];
        // Copy payload, stop at newline, strip trailing CR
        for &b in payload {
            if b == b'\n' {
                break;
            }
            if b == b'\r' {
                continue;
            }
            tnc2.push(b as char);
        }
    }

    Ok(Tnc2Frame {
        tnc2,
        tnc2_addr_len,
        ax25_addr_len,
        is_aprs,
        ui_pid: pid as i16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_callsign_no_ssid() {
        let ax25 = encode_ax25_address("OH2MQK", 0x60).unwrap();
        assert_eq!(ax25[0], b'O' << 1);
        assert_eq!(ax25[1], b'H' << 1);
        assert_eq!(ax25[5], b'K' << 1); // 6-char call fills all positions
        assert_eq!(ax25[6] & 0x1E, 0); // SSID 0

        // Shorter callsign gets space-padded
        let ax25 = encode_ax25_address("APRS", 0xE0).unwrap();
        assert_eq!(ax25[4], b' ' << 1); // padded with space
        assert_eq!(ax25[5], b' ' << 1); // padded with space
    }

    #[test]
    fn test_encode_callsign_with_ssid() {
        let ax25 = encode_ax25_address("OH2MQK-15", 0x60).unwrap();
        assert_eq!((ax25[6] >> 1) & 0x0F, 15);
    }

    #[test]
    fn test_decode_callsign() {
        let ax25 = encode_ax25_address("OH2MQK-1", 0x60).unwrap();
        let (call, ssid_byte) = decode_ax25_address(&ax25, false).unwrap();
        assert_eq!(call, "OH2MQK-1");
        assert_eq!(ssid_byte & 0x80, 0); // no H-bit
    }

    #[test]
    fn test_decode_with_hbit() {
        let mut ax25 = encode_ax25_address("WIDE1-1", 0x60).unwrap();
        ax25[6] |= 0x80; // set H-bit (has been digipeated)
        let (call, _) = decode_ax25_address(&ax25, true).unwrap();
        assert_eq!(call, "WIDE1-1*");
    }

    #[test]
    fn test_ssid_zero_not_printed() {
        let ax25 = encode_ax25_address("APRS", 0xE0).unwrap();
        let (call, _) = decode_ax25_address(&ax25, false).unwrap();
        assert_eq!(call, "APRS");
    }

    #[test]
    fn test_invalid_callsign_lowercase() {
        assert!(encode_ax25_address("oh2mqk", 0x60).is_err());
    }

    #[test]
    fn test_invalid_ssid_too_high() {
        assert!(encode_ax25_address("TEST-16", 0x60).is_err());
    }

    #[test]
    fn test_ax25_frame_to_tnc2() {
        // Build: OH2MQK-1>APRS with UI frame, APRS PID, position payload
        let dst = encode_ax25_address("APRS", 0xE0).unwrap();
        let mut src = encode_ax25_address("OH2MQK-1", 0x60).unwrap();
        src[6] |= 0x01; // mark as last address
        let mut frame = Vec::new();
        frame.extend_from_slice(&dst);
        frame.extend_from_slice(&src);
        frame.push(0x03); // UI control
        frame.push(0xF0); // APRS PID
        frame.extend_from_slice(b"!6029.50N/02505.43E>");

        let result = ax25_to_tnc2(&frame).unwrap();
        assert!(result.tnc2.starts_with("OH2MQK-1>APRS:"));
        assert!(result.tnc2.contains("!6029.50N/02505.43E>"));
        assert!(result.is_aprs);
        assert_eq!(result.ui_pid, 0xF0);
    }

    #[test]
    fn test_ax25_frame_with_via() {
        let dst = encode_ax25_address("APRS", 0xE0).unwrap();
        let src = encode_ax25_address("OH2MQK-1", 0x60).unwrap();
        let mut via = encode_ax25_address("WIDE1-1", 0x60).unwrap();
        via[6] |= 0x01; // mark as last address
        let mut frame = Vec::new();
        frame.extend_from_slice(&dst);
        frame.extend_from_slice(&src);
        frame.extend_from_slice(&via);
        frame.push(0x03);
        frame.push(0xF0);
        frame.extend_from_slice(b"!test");

        let result = ax25_to_tnc2(&frame).unwrap();
        assert!(result.tnc2.starts_with("OH2MQK-1>APRS,WIDE1-1:"));
    }

    #[test]
    fn test_frame_too_short() {
        let frame = vec![0u8; 10];
        assert!(ax25_to_tnc2(&frame).is_err());
    }

    #[test]
    fn test_non_ui_frame() {
        // Build frame with control byte != 0x03
        let dst = encode_ax25_address("APRS", 0xE0).unwrap();
        let mut src = encode_ax25_address("TEST-1", 0x60).unwrap();
        src[6] |= 0x01;
        let mut frame = Vec::new();
        frame.extend_from_slice(&dst);
        frame.extend_from_slice(&src);
        frame.push(0x13); // NOT a UI frame
        frame.push(0xF0);
        frame.extend_from_slice(b"data");

        let result = ax25_to_tnc2(&frame).unwrap();
        assert!(!result.is_aprs);
        assert_eq!(result.ui_pid, -1);
    }

    #[test]
    fn test_max_via_fields() {
        // AX.25 allows up to 8 via addresses
        let dst = encode_ax25_address("APRS", 0xE0).unwrap();
        let src = encode_ax25_address("SRC", 0x60).unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&dst);
        frame.extend_from_slice(&src);
        for i in 0..8 {
            let name = format!("VIA{}", i);
            let mut via = encode_ax25_address(&name, 0x60).unwrap();
            if i == 7 {
                via[6] |= 0x01;
            } // last address
            frame.extend_from_slice(&via);
        }
        frame.push(0x03);
        frame.push(0xF0);
        frame.extend_from_slice(b"test");

        let result = ax25_to_tnc2(&frame).unwrap();
        assert!(result.is_aprs);
    }
}
