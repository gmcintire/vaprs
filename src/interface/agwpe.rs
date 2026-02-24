// AGWPE (AGW Packet Engine) socket interface
//
// AGWPE is a protocol for communicating with software TNCs over TCP.
// The frame format uses a 36-byte header followed by variable-length data.
//
// Header layout (36 bytes, all little-endian):
//   Offset  Size  Field
//   0       4     Port number (u32 LE)
//   4       4     Data kind (ASCII char in first byte, rest zero)
//   8       10    Callsign from (null-padded)
//   18      10    Callsign to (null-padded)
//   28      4     Data length (u32 LE)
//   32      4     User (unused, zero)
//
// Important data kinds:
//   'k' = Raw AX.25 data frame (received, flags/CRC stripped)
//   'K' = Raw AX.25 transmit
//   'R' = Version info request
//   'G' = Port info request
//   'X' = Register callsign
//   'x' = Unregister callsign

/// Total length of an AGWPE frame header in bytes.
pub const AGWPE_HEADER_LEN: usize = 36;

/// An AGWPE protocol frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgwpeFrame {
    /// TNC port number.
    pub port: u32,
    /// Data kind byte (ASCII character identifying the frame type).
    pub data_kind: u8,
    /// Source callsign (from field).
    pub call_from: String,
    /// Destination callsign (to field).
    pub call_to: String,
    /// Frame payload data.
    pub data: Vec<u8>,
}

/// Parse an AGWPE frame from a header and data payload.
///
/// The header must be exactly `AGWPE_HEADER_LEN` bytes. The data slice
/// should contain the number of bytes indicated by the data length field
/// in the header.
pub fn parse_agwpe_frame(header: &[u8; AGWPE_HEADER_LEN], data: &[u8]) -> AgwpeFrame {
    let port = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let data_kind = header[4];

    let call_from = extract_callsign(&header[8..18]);
    let call_to = extract_callsign(&header[18..28]);

    AgwpeFrame {
        port,
        data_kind,
        call_from,
        call_to,
        data: data.to_vec(),
    }
}

/// Encode an AGWPE frame into a byte vector (header + data).
pub fn encode_agwpe_frame(frame: &AgwpeFrame) -> Vec<u8> {
    let mut buf = vec![0u8; AGWPE_HEADER_LEN + frame.data.len()];

    // Port (bytes 0..4)
    buf[0..4].copy_from_slice(&frame.port.to_le_bytes());

    // Data kind (bytes 4..8, first byte is the kind, rest zero)
    buf[4] = frame.data_kind;
    // bytes 5..8 already zero

    // Call from (bytes 8..18, null-padded)
    write_callsign(&mut buf[8..18], &frame.call_from);

    // Call to (bytes 18..28, null-padded)
    write_callsign(&mut buf[18..28], &frame.call_to);

    // Data length (bytes 28..32)
    let data_len = frame.data.len() as u32;
    buf[28..32].copy_from_slice(&data_len.to_le_bytes());

    // User field (bytes 32..36) already zero

    // Data payload
    buf[AGWPE_HEADER_LEN..].copy_from_slice(&frame.data);

    buf
}

/// Create a register-callsign frame ('X' kind).
///
/// Tells the AGWPE host to associate the given callsign with the
/// specified port so we receive frames addressed to it.
pub fn register_callsign(port: u32, callsign: &str) -> AgwpeFrame {
    AgwpeFrame {
        port,
        data_kind: b'X',
        call_from: callsign.to_string(),
        call_to: String::new(),
        data: Vec::new(),
    }
}

/// Create an unregister-callsign frame ('x' kind).
pub fn unregister_callsign(port: u32, callsign: &str) -> AgwpeFrame {
    AgwpeFrame {
        port,
        data_kind: b'x',
        call_from: callsign.to_string(),
        call_to: String::new(),
        data: Vec::new(),
    }
}

/// Create a version info request frame ('R' kind).
pub fn version_request() -> AgwpeFrame {
    AgwpeFrame {
        port: 0,
        data_kind: b'R',
        call_from: String::new(),
        call_to: String::new(),
        data: Vec::new(),
    }
}

/// Create a port info request frame ('G' kind).
pub fn port_info_request() -> AgwpeFrame {
    AgwpeFrame {
        port: 0,
        data_kind: b'G',
        call_from: String::new(),
        call_to: String::new(),
        data: Vec::new(),
    }
}

/// Create a raw AX.25 transmit frame ('K' kind).
pub fn raw_transmit(port: u32, call_from: &str, call_to: &str, ax25_data: &[u8]) -> AgwpeFrame {
    AgwpeFrame {
        port,
        data_kind: b'K',
        call_from: call_from.to_string(),
        call_to: call_to.to_string(),
        data: ax25_data.to_vec(),
    }
}

/// Extract a null-terminated callsign from a 10-byte field.
fn extract_callsign(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).to_string()
}

/// Write a callsign into a 10-byte null-padded field.
fn write_callsign(field: &mut [u8], callsign: &str) {
    let bytes = callsign.as_bytes();
    let copy_len = bytes.len().min(field.len());
    field[..copy_len].copy_from_slice(&bytes[..copy_len]);
    // Rest is already zero from vec initialization
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_agwpe_frame tests ---

    #[test]
    fn parse_minimal_frame_with_no_data() {
        let mut header = [0u8; AGWPE_HEADER_LEN];
        // port = 0, data_kind = 'R', no callsigns, data_len = 0
        header[4] = b'R';

        let frame = parse_agwpe_frame(&header, &[]);
        assert_eq!(frame.port, 0);
        assert_eq!(frame.data_kind, b'R');
        assert_eq!(frame.call_from, "");
        assert_eq!(frame.call_to, "");
        assert!(frame.data.is_empty());
    }

    #[test]
    fn parse_frame_with_port_number() {
        let mut header = [0u8; AGWPE_HEADER_LEN];
        // port = 2 (little-endian)
        header[0] = 2;
        header[4] = b'k';

        let frame = parse_agwpe_frame(&header, &[0xAA, 0xBB]);
        assert_eq!(frame.port, 2);
        assert_eq!(frame.data_kind, b'k');
        assert_eq!(frame.data, vec![0xAA, 0xBB]);
    }

    #[test]
    fn parse_frame_with_large_port_number() {
        let mut header = [0u8; AGWPE_HEADER_LEN];
        // port = 0x01020304 little-endian
        header[0..4].copy_from_slice(&0x01020304u32.to_le_bytes());
        header[4] = b'G';

        let frame = parse_agwpe_frame(&header, &[]);
        assert_eq!(frame.port, 0x01020304);
    }

    #[test]
    fn parse_frame_with_callsigns() {
        let mut header = [0u8; AGWPE_HEADER_LEN];
        header[4] = b'k';

        // call_from = "OH2MQK-1" at offset 8
        let from_bytes = b"OH2MQK-1";
        header[8..8 + from_bytes.len()].copy_from_slice(from_bytes);

        // call_to = "APRS" at offset 18
        let to_bytes = b"APRS";
        header[18..18 + to_bytes.len()].copy_from_slice(to_bytes);

        let frame = parse_agwpe_frame(&header, &[]);
        assert_eq!(frame.call_from, "OH2MQK-1");
        assert_eq!(frame.call_to, "APRS");
    }

    #[test]
    fn parse_frame_callsign_null_padded() {
        let mut header = [0u8; AGWPE_HEADER_LEN];
        header[4] = b'X';

        // Short callsign followed by nulls
        header[8] = b'A';
        header[9] = b'B';
        // rest is zero (null padding)

        let frame = parse_agwpe_frame(&header, &[]);
        assert_eq!(frame.call_from, "AB");
    }

    #[test]
    fn parse_frame_full_length_callsign() {
        let mut header = [0u8; AGWPE_HEADER_LEN];
        header[4] = b'k';

        // 10-byte callsign field completely filled
        header[8..18].copy_from_slice(b"0123456789");

        let frame = parse_agwpe_frame(&header, &[]);
        assert_eq!(frame.call_from, "0123456789");
    }

    #[test]
    fn parse_frame_with_data_payload() {
        let mut header = [0u8; AGWPE_HEADER_LEN];
        header[4] = b'k';

        let data = vec![1, 2, 3, 4, 5, 6, 7, 8];
        // data_len in header at offset 28
        header[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());

        let frame = parse_agwpe_frame(&header, &data);
        assert_eq!(frame.data, data);
    }

    // --- encode_agwpe_frame tests ---

    #[test]
    fn encode_empty_frame() {
        let frame = AgwpeFrame {
            port: 0,
            data_kind: b'R',
            call_from: String::new(),
            call_to: String::new(),
            data: Vec::new(),
        };

        let encoded = encode_agwpe_frame(&frame);
        assert_eq!(encoded.len(), AGWPE_HEADER_LEN);
        assert_eq!(encoded[4], b'R');
        // Data length should be 0
        assert_eq!(
            u32::from_le_bytes([encoded[28], encoded[29], encoded[30], encoded[31]]),
            0
        );
    }

    #[test]
    fn encode_frame_with_port() {
        let frame = AgwpeFrame {
            port: 3,
            data_kind: b'K',
            call_from: String::new(),
            call_to: String::new(),
            data: Vec::new(),
        };

        let encoded = encode_agwpe_frame(&frame);
        assert_eq!(
            u32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]),
            3
        );
    }

    #[test]
    fn encode_frame_with_callsigns() {
        let frame = AgwpeFrame {
            port: 0,
            data_kind: b'X',
            call_from: "OH2MQK-1".to_string(),
            call_to: "APRS".to_string(),
            data: Vec::new(),
        };

        let encoded = encode_agwpe_frame(&frame);

        // Verify call_from field
        assert_eq!(&encoded[8..16], b"OH2MQK-1");
        assert_eq!(encoded[16], 0); // null padding
        assert_eq!(encoded[17], 0);

        // Verify call_to field
        assert_eq!(&encoded[18..22], b"APRS");
        assert_eq!(encoded[22], 0); // null padding
    }

    #[test]
    fn encode_frame_with_data() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let frame = AgwpeFrame {
            port: 1,
            data_kind: b'K',
            call_from: "SRC".to_string(),
            call_to: "DST".to_string(),
            data: data.clone(),
        };

        let encoded = encode_agwpe_frame(&frame);
        assert_eq!(encoded.len(), AGWPE_HEADER_LEN + 4);

        // Data length field
        let data_len = u32::from_le_bytes([encoded[28], encoded[29], encoded[30], encoded[31]]);
        assert_eq!(data_len, 4);

        // Actual data
        assert_eq!(&encoded[AGWPE_HEADER_LEN..], &data);
    }

    #[test]
    fn encode_frame_user_field_is_zero() {
        let frame = AgwpeFrame {
            port: 0,
            data_kind: b'G',
            call_from: String::new(),
            call_to: String::new(),
            data: Vec::new(),
        };

        let encoded = encode_agwpe_frame(&frame);
        // User field at offset 32..36 should be zero
        assert_eq!(&encoded[32..36], &[0, 0, 0, 0]);
    }

    #[test]
    fn encode_frame_data_kind_rest_bytes_zero() {
        let frame = AgwpeFrame {
            port: 0,
            data_kind: b'k',
            call_from: String::new(),
            call_to: String::new(),
            data: Vec::new(),
        };

        let encoded = encode_agwpe_frame(&frame);
        // Data kind field is 4 bytes: first is the kind, rest should be zero
        assert_eq!(encoded[4], b'k');
        assert_eq!(encoded[5], 0);
        assert_eq!(encoded[6], 0);
        assert_eq!(encoded[7], 0);
    }

    // --- roundtrip tests ---

    #[test]
    fn roundtrip_encode_then_parse() {
        let original = AgwpeFrame {
            port: 5,
            data_kind: b'k',
            call_from: "OH2MQK-1".to_string(),
            call_to: "APRS".to_string(),
            data: vec![1, 2, 3, 4, 5],
        };

        let encoded = encode_agwpe_frame(&original);
        let header: [u8; AGWPE_HEADER_LEN] = encoded[..AGWPE_HEADER_LEN].try_into().unwrap();
        let data = &encoded[AGWPE_HEADER_LEN..];

        let parsed = parse_agwpe_frame(&header, data);
        assert_eq!(parsed, original);
    }

    #[test]
    fn roundtrip_empty_data() {
        let original = AgwpeFrame {
            port: 0,
            data_kind: b'R',
            call_from: String::new(),
            call_to: String::new(),
            data: Vec::new(),
        };

        let encoded = encode_agwpe_frame(&original);
        let header: [u8; AGWPE_HEADER_LEN] = encoded[..AGWPE_HEADER_LEN].try_into().unwrap();

        let parsed = parse_agwpe_frame(&header, &[]);
        assert_eq!(parsed, original);
    }

    #[test]
    fn roundtrip_max_callsign_length() {
        let original = AgwpeFrame {
            port: 0,
            data_kind: b'X',
            call_from: "0123456789".to_string(),
            call_to: "ABCDEFGHIJ".to_string(),
            data: Vec::new(),
        };

        let encoded = encode_agwpe_frame(&original);
        let header: [u8; AGWPE_HEADER_LEN] = encoded[..AGWPE_HEADER_LEN].try_into().unwrap();

        let parsed = parse_agwpe_frame(&header, &[]);
        assert_eq!(parsed, original);
    }

    // --- register_callsign tests ---

    #[test]
    fn register_callsign_creates_x_frame() {
        let frame = register_callsign(0, "OH2MQK-1");
        assert_eq!(frame.port, 0);
        assert_eq!(frame.data_kind, b'X');
        assert_eq!(frame.call_from, "OH2MQK-1");
        assert_eq!(frame.call_to, "");
        assert!(frame.data.is_empty());
    }

    #[test]
    fn register_callsign_with_port() {
        let frame = register_callsign(2, "TEST-1");
        assert_eq!(frame.port, 2);
        assert_eq!(frame.data_kind, b'X');
        assert_eq!(frame.call_from, "TEST-1");
    }

    #[test]
    fn register_callsign_encodes_correctly() {
        let frame = register_callsign(1, "N0CALL");
        let encoded = encode_agwpe_frame(&frame);

        assert_eq!(encoded[4], b'X');
        assert_eq!(&encoded[8..14], b"N0CALL");
        assert_eq!(encoded[14], 0); // null padding
    }

    // --- unregister_callsign tests ---

    #[test]
    fn unregister_callsign_creates_lowercase_x_frame() {
        let frame = unregister_callsign(0, "OH2MQK-1");
        assert_eq!(frame.data_kind, b'x');
        assert_eq!(frame.call_from, "OH2MQK-1");
    }

    // --- version_request tests ---

    #[test]
    fn version_request_creates_r_frame() {
        let frame = version_request();
        assert_eq!(frame.port, 0);
        assert_eq!(frame.data_kind, b'R');
        assert!(frame.call_from.is_empty());
        assert!(frame.call_to.is_empty());
        assert!(frame.data.is_empty());
    }

    // --- port_info_request tests ---

    #[test]
    fn port_info_request_creates_g_frame() {
        let frame = port_info_request();
        assert_eq!(frame.port, 0);
        assert_eq!(frame.data_kind, b'G');
        assert!(frame.data.is_empty());
    }

    // --- raw_transmit tests ---

    #[test]
    fn raw_transmit_creates_k_frame() {
        let ax25_data = vec![0x01, 0x02, 0x03];
        let frame = raw_transmit(0, "SRC-1", "DST-2", &ax25_data);
        assert_eq!(frame.port, 0);
        assert_eq!(frame.data_kind, b'K');
        assert_eq!(frame.call_from, "SRC-1");
        assert_eq!(frame.call_to, "DST-2");
        assert_eq!(frame.data, ax25_data);
    }

    #[test]
    fn raw_transmit_with_port() {
        let frame = raw_transmit(3, "SRC", "DST", &[0xFF]);
        assert_eq!(frame.port, 3);
        assert_eq!(frame.data, vec![0xFF]);
    }

    // --- extract_callsign edge cases ---

    #[test]
    fn extract_callsign_all_nulls() {
        let field = [0u8; 10];
        assert_eq!(extract_callsign(&field), "");
    }

    #[test]
    fn extract_callsign_no_nulls() {
        let field = b"ABCDEFGHIJ";
        assert_eq!(extract_callsign(field), "ABCDEFGHIJ");
    }

    #[test]
    fn extract_callsign_with_embedded_null() {
        let mut field = [0u8; 10];
        field[0] = b'A';
        field[1] = b'B';
        field[2] = 0; // null terminator
        field[3] = b'C'; // should be ignored
        assert_eq!(extract_callsign(&field), "AB");
    }

    // --- Debug formatting ---

    #[test]
    fn agwpe_frame_debug_format() {
        let frame = register_callsign(0, "TEST");
        let debug = format!("{:?}", frame);
        assert!(debug.contains("TEST"));
        assert!(debug.contains("AgwpeFrame"));
    }

    // --- Large data payload ---

    #[test]
    fn encode_and_parse_large_payload() {
        let data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let frame = AgwpeFrame {
            port: 0,
            data_kind: b'k',
            call_from: "SRC".to_string(),
            call_to: "DST".to_string(),
            data: data.clone(),
        };

        let encoded = encode_agwpe_frame(&frame);
        assert_eq!(encoded.len(), AGWPE_HEADER_LEN + 1000);

        let header: [u8; AGWPE_HEADER_LEN] = encoded[..AGWPE_HEADER_LEN].try_into().unwrap();
        let parsed = parse_agwpe_frame(&header, &encoded[AGWPE_HEADER_LEN..]);
        assert_eq!(parsed.data, data);
    }
}
