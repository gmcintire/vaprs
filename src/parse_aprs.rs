// APRS packet payload parser
//
// Extracts structured data from APRS packet payloads: position reports,
// messages, objects, and items. Used by the filter and history database
// to index and route packets.
//
// APRS payload format reference (APRS Protocol Reference 1.0.1):
//   Position: '!' or '=' + lat/lon (uncompressed or compressed)
//   Message:  ':' + 9-char addressee + ':' + text + optional '{' + msgno
//   Object:   ';' + 9-char name + '*' or '_' + timestamp + position
//   Item:     ')' + 3-9 char name + '!' or '_' + position

/// Parsed APRS data extracted from a packet payload.
#[derive(Debug, Clone, PartialEq)]
pub enum AprsData {
    /// Position report with coordinates and symbol.
    Position {
        lat: f64,
        lon: f64,
        symbol_table: char,
        symbol_code: char,
        comment: Option<String>,
    },
    /// APRS message to a specific addressee.
    Message {
        addressee: String,
        text: String,
        message_id: Option<String>,
    },
    /// Object (named, timestamped position).
    Object {
        name: String,
        /// `true` = live object, `false` = killed/deleted object.
        live: bool,
        lat: f64,
        lon: f64,
    },
    /// Item (named position, simpler than object).
    Item {
        name: String,
        /// `true` = live item, `false` = killed/deleted item.
        live: bool,
        lat: f64,
        lon: f64,
    },
    /// Unrecognized or unparseable format.
    Unknown,
}

/// Parse an APRS payload string into structured data.
///
/// The payload is the portion of a TNC2 frame after the `:` separator
/// (i.e., everything after the address header).
///
/// Recognizes position reports (`!`, `=`, `/`, `@`), messages (`:`),
/// objects (`;`), and items (`)`).
pub fn parse_aprs(payload: &str) -> AprsData {
    if payload.is_empty() {
        return AprsData::Unknown;
    }

    let first = payload.as_bytes()[0];
    match first {
        // Position without timestamp (! or =)
        b'!' | b'=' => parse_position(payload),
        // Position with timestamp (/ or @)
        b'/' | b'@' => parse_position_with_timestamp(payload),
        // Message
        b':' => parse_message(payload),
        // Object
        b';' => parse_object(payload),
        // Item
        b')' => parse_item(payload),
        _ => AprsData::Unknown,
    }
}

/// Parse APRS uncompressed latitude string.
///
/// Format: `DDMM.MMN` where DD = degrees, MM.MM = decimal minutes,
/// N = hemisphere (N or S).
///
/// Example: `"4903.50N"` = 49 degrees 3.50 minutes North = 49.058333 degrees
pub fn parse_latitude(s: &str) -> Option<f64> {
    if s.len() < 8 {
        return None;
    }

    let deg: f64 = s[0..2].parse().ok()?;
    let min: f64 = s[2..7].parse().ok()?;
    let hem = s.as_bytes()[7];

    if deg > 90.0 || min >= 60.0 {
        return None;
    }

    let value = deg + min / 60.0;
    match hem {
        b'N' => Some(value),
        b'S' => Some(-value),
        _ => None,
    }
}

/// Parse APRS uncompressed longitude string.
///
/// Format: `DDDMM.MME` where DDD = degrees, MM.MM = decimal minutes,
/// E = hemisphere (E or W).
///
/// Example: `"07201.75W"` = 72 degrees 1.75 minutes West = -72.029167 degrees
pub fn parse_longitude(s: &str) -> Option<f64> {
    if s.len() < 9 {
        return None;
    }

    let deg: f64 = s[0..3].parse().ok()?;
    let min: f64 = s[3..8].parse().ok()?;
    let hem = s.as_bytes()[8];

    if deg > 180.0 || min >= 60.0 {
        return None;
    }

    let value = deg + min / 60.0;
    match hem {
        b'E' => Some(value),
        b'W' => Some(-value),
        _ => None,
    }
}

/// Parse a position report (data type '!' or '=').
///
/// Supports both uncompressed and compressed formats.
fn parse_position(payload: &str) -> AprsData {
    // Skip the data type identifier (! or =)
    let rest = &payload[1..];

    if rest.is_empty() {
        return AprsData::Unknown;
    }

    // Check for compressed format: starts with a symbol table char
    // followed by 4 compressed lat chars, 4 compressed lon chars
    let first_byte = rest.as_bytes()[0];
    if first_byte == b'/' || first_byte == b'\\' || first_byte.is_ascii_uppercase() {
        // Might be compressed - try it
        if let Some(result) = try_parse_compressed(rest) {
            return result;
        }
    }

    // Uncompressed format: DDMM.MMN/DDDMM.MMESymbol
    parse_uncompressed_position(rest)
}

/// Parse a position report with timestamp (data type '/' or '@').
///
/// Format: type_char + 7-char timestamp + position data
fn parse_position_with_timestamp(payload: &str) -> AprsData {
    let rest = &payload[1..];

    // Timestamp is 7 characters (DHM or HMS format)
    if rest.len() < 7 {
        return AprsData::Unknown;
    }

    let after_timestamp = &rest[7..];
    if after_timestamp.is_empty() {
        return AprsData::Unknown;
    }

    // Check for compressed format
    let first_byte = after_timestamp.as_bytes()[0];
    if first_byte == b'/' || first_byte == b'\\' || first_byte.is_ascii_uppercase() {
        if let Some(result) = try_parse_compressed(after_timestamp) {
            return result;
        }
    }

    parse_uncompressed_position(after_timestamp)
}

/// Parse uncompressed position from the portion after the data type identifier.
///
/// Expected format: `DDMM.MMN{sym_table}DDDMM.MME{sym_code}{comment}`
fn parse_uncompressed_position(s: &str) -> AprsData {
    // Need at least 19 chars: 8 (lat) + 1 (sym_table) + 9 (lon) + 1 (sym_code)
    if s.len() < 19 {
        return AprsData::Unknown;
    }

    let lat = match parse_latitude(&s[0..8]) {
        Some(v) => v,
        None => return AprsData::Unknown,
    };

    let symbol_table = s.as_bytes()[8] as char;

    let lon = match parse_longitude(&s[9..18]) {
        Some(v) => v,
        None => return AprsData::Unknown,
    };

    let symbol_code = s.as_bytes()[18] as char;

    let comment = if s.len() > 19 {
        let c = s[19..].to_string();
        if c.is_empty() {
            None
        } else {
            Some(c)
        }
    } else {
        None
    };

    AprsData::Position {
        lat,
        lon,
        symbol_table,
        symbol_code,
        comment,
    }
}

/// Try to parse compressed position format.
///
/// Compressed format: {sym_table}{YYYY}{XXXX}{sym_code}{cs}{ct}
/// where YYYY and XXXX are base-91 encoded lat/lon (4 chars each).
fn try_parse_compressed(s: &str) -> Option<AprsData> {
    // Need at least 13 characters: 1 (sym_table) + 4 (lat) + 4 (lon) + 1 (sym_code) + 2 (cs+ct) + 1
    if s.len() < 10 {
        return None;
    }

    let symbol_table = s.as_bytes()[0] as char;

    // Decode base-91 compressed latitude (4 chars at positions 1..5)
    let lat_chars = &s.as_bytes()[1..5];
    let lat_val = decode_base91_4(lat_chars)?;
    let lat = 90.0 - (lat_val as f64 / 380926.0);

    // Decode base-91 compressed longitude (4 chars at positions 5..9)
    let lon_chars = &s.as_bytes()[5..9];
    let lon_val = decode_base91_4(lon_chars)?;
    let lon = -180.0 + (lon_val as f64 / 190463.0);

    let symbol_code = s.as_bytes()[9] as char;

    let comment = if s.len() > 12 {
        Some(s[13..].to_string())
    } else {
        None
    };

    let comment = comment.filter(|c| !c.is_empty());

    Some(AprsData::Position {
        lat,
        lon,
        symbol_table,
        symbol_code,
        comment,
    })
}

/// Decode a 4-character base-91 encoded value.
///
/// Each character represents a digit in base-91 with ASCII offset 33.
fn decode_base91_4(chars: &[u8]) -> Option<u32> {
    if chars.len() < 4 {
        return None;
    }

    let mut value: u32 = 0;
    for &c in &chars[..4] {
        if !(33..=124).contains(&c) {
            return None;
        }
        value = value * 91 + (c - 33) as u32;
    }
    Some(value)
}

/// Parse an APRS message.
///
/// Format: `:ADDRESSEE :message text{msgno`
/// The addressee field is exactly 9 characters, space-padded.
fn parse_message(payload: &str) -> AprsData {
    // Minimum: ':' + 9-char addressee + ':' + at least empty text = 11 chars
    if payload.len() < 11 {
        return AprsData::Unknown;
    }

    // First char is ':', addressee is next 9 chars
    let addressee_raw = &payload[1..10];

    // Find the second ':' that terminates the addressee field
    if payload.as_bytes()[10] != b':' {
        return AprsData::Unknown;
    }

    let addressee = addressee_raw.trim_end().to_string();
    let text_part = &payload[11..];

    // Check for message ID: text{msgno
    let (text, message_id) = if let Some(brace_pos) = text_part.rfind('{') {
        let msg_text = text_part[..brace_pos].to_string();
        let msg_id = text_part[brace_pos + 1..].to_string();
        let msg_id = if msg_id.is_empty() {
            None
        } else {
            Some(msg_id)
        };
        (msg_text, msg_id)
    } else {
        (text_part.to_string(), None)
    };

    AprsData::Message {
        addressee,
        text,
        message_id,
    }
}

/// Parse an APRS object.
///
/// Format: `;name     *DDHHMMzDDMM.MMN/DDDMM.MMESymbol`
///
/// The name field is exactly 9 characters, space-padded.
/// `*` means live object, `_` means killed object.
fn parse_object(payload: &str) -> AprsData {
    // Minimum: ';' + 9-char name + '*' or '_' + 7-char timestamp + 19-char position = 37
    if payload.len() < 37 {
        return AprsData::Unknown;
    }

    let name = payload[1..10].trim_end().to_string();
    let live_kill = payload.as_bytes()[10];
    let live = match live_kill {
        b'*' => true,
        b'_' => false,
        _ => return AprsData::Unknown,
    };

    // After live/kill indicator is a 7-character timestamp, then position
    let position_start = 18; // 1 + 9 + 1 + 7
    if payload.len() < position_start + 19 {
        return AprsData::Unknown;
    }

    let pos_str = &payload[position_start..];
    let lat = match parse_latitude(&pos_str[0..8]) {
        Some(v) => v,
        None => return AprsData::Unknown,
    };
    let lon = match parse_longitude(&pos_str[9..18]) {
        Some(v) => v,
        None => return AprsData::Unknown,
    };

    AprsData::Object {
        name,
        live,
        lat,
        lon,
    }
}

/// Parse an APRS item.
///
/// Format: `)name!DDMM.MMN/DDDMM.MMESymbol` or `)name_DDMM.MMN/DDDMM.MMESymbol`
///
/// The name field is 3-9 characters terminated by `!` (live) or `_` (killed).
fn parse_item(payload: &str) -> AprsData {
    if payload.len() < 4 {
        return AprsData::Unknown;
    }

    // Find the live/kill separator (! or _) in the name portion (positions 1..10)
    let search_end = payload.len().min(10);
    let separator_pos = payload[1..search_end].find(['!', '_']).map(|p| p + 1); // adjust for the offset

    let separator_pos = match separator_pos {
        Some(p) => p,
        None => return AprsData::Unknown,
    };

    let name = payload[1..separator_pos].to_string();
    let live = payload.as_bytes()[separator_pos] == b'!';

    let pos_str = &payload[separator_pos + 1..];
    if pos_str.len() < 19 {
        return AprsData::Unknown;
    }

    let lat = match parse_latitude(&pos_str[0..8]) {
        Some(v) => v,
        None => return AprsData::Unknown,
    };
    let lon = match parse_longitude(&pos_str[9..18]) {
        Some(v) => v,
        None => return AprsData::Unknown,
    };

    AprsData::Item {
        name,
        live,
        lat,
        lon,
    }
}

/// Extract position coordinates from a full TNC2 packet line, if present.
pub fn extract_position(tnc2: &str) -> Option<(f64, f64)> {
    let payload = tnc2.split(':').nth(1)?;
    match parse_aprs(payload) {
        AprsData::Position { lat, lon, .. } => Some((lat, lon)),
        AprsData::Object { lat, lon, .. } => Some((lat, lon)),
        AprsData::Item { lat, lon, .. } => Some((lat, lon)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to compare floating point with tolerance
    fn approx_eq(a: f64, b: f64, epsilon: f64) -> bool {
        (a - b).abs() < epsilon
    }

    // --- parse_latitude tests ---

    #[test]
    fn parse_latitude_north() {
        let lat = parse_latitude("4903.50N").unwrap();
        assert!(approx_eq(lat, 49.058333, 0.001));
    }

    #[test]
    fn parse_latitude_south() {
        let lat = parse_latitude("3351.00S").unwrap();
        assert!(approx_eq(lat, -33.85, 0.001));
    }

    #[test]
    fn parse_latitude_zero() {
        let lat = parse_latitude("0000.00N").unwrap();
        assert!(approx_eq(lat, 0.0, 0.001));
    }

    #[test]
    fn parse_latitude_max_north() {
        let lat = parse_latitude("9000.00N").unwrap();
        assert!(approx_eq(lat, 90.0, 0.001));
    }

    #[test]
    fn parse_latitude_too_short() {
        assert!(parse_latitude("4903.5").is_none());
    }

    #[test]
    fn parse_latitude_invalid_hemisphere() {
        assert!(parse_latitude("4903.50X").is_none());
    }

    #[test]
    fn parse_latitude_invalid_degrees() {
        assert!(parse_latitude("9100.00N").is_none());
    }

    #[test]
    fn parse_latitude_invalid_minutes() {
        assert!(parse_latitude("4960.00N").is_none());
    }

    #[test]
    fn parse_latitude_non_numeric() {
        assert!(parse_latitude("ABCD.EFN").is_none());
    }

    // --- parse_longitude tests ---

    #[test]
    fn parse_longitude_west() {
        let lon = parse_longitude("07201.75W").unwrap();
        assert!(approx_eq(lon, -72.029167, 0.001));
    }

    #[test]
    fn parse_longitude_east() {
        let lon = parse_longitude("02505.43E").unwrap();
        assert!(approx_eq(lon, 25.090500, 0.001));
    }

    #[test]
    fn parse_longitude_zero() {
        let lon = parse_longitude("00000.00E").unwrap();
        assert!(approx_eq(lon, 0.0, 0.001));
    }

    #[test]
    fn parse_longitude_max_east() {
        let lon = parse_longitude("18000.00E").unwrap();
        assert!(approx_eq(lon, 180.0, 0.001));
    }

    #[test]
    fn parse_longitude_too_short() {
        assert!(parse_longitude("07201.7").is_none());
    }

    #[test]
    fn parse_longitude_invalid_hemisphere() {
        assert!(parse_longitude("07201.75X").is_none());
    }

    #[test]
    fn parse_longitude_invalid_degrees() {
        assert!(parse_longitude("18100.00E").is_none());
    }

    #[test]
    fn parse_longitude_invalid_minutes() {
        assert!(parse_longitude("07260.00W").is_none());
    }

    #[test]
    fn parse_longitude_non_numeric() {
        assert!(parse_longitude("ABCDE.FGE").is_none());
    }

    // --- parse_aprs position tests ---

    #[test]
    fn parse_position_without_timestamp_bang() {
        let result = parse_aprs("!4903.50N/07201.75W-");
        match result {
            AprsData::Position {
                lat,
                lon,
                symbol_table,
                symbol_code,
                ..
            } => {
                assert!(approx_eq(lat, 49.058333, 0.001));
                assert!(approx_eq(lon, -72.029167, 0.001));
                assert_eq!(symbol_table, '/');
                assert_eq!(symbol_code, '-');
            }
            _ => panic!("expected Position, got {:?}", result),
        }
    }

    #[test]
    fn parse_position_without_timestamp_equals() {
        let result = parse_aprs("=4903.50N/07201.75W-");
        match result {
            AprsData::Position { lat, lon, .. } => {
                assert!(approx_eq(lat, 49.058333, 0.001));
                assert!(approx_eq(lon, -72.029167, 0.001));
            }
            _ => panic!("expected Position, got {:?}", result),
        }
    }

    #[test]
    fn parse_position_with_comment() {
        let result = parse_aprs("!4903.50N/07201.75W-PHG2360/Test station");
        match result {
            AprsData::Position { comment, .. } => {
                assert_eq!(comment, Some("PHG2360/Test station".to_string()));
            }
            _ => panic!("expected Position, got {:?}", result),
        }
    }

    #[test]
    fn parse_position_no_comment() {
        let result = parse_aprs("!4903.50N/07201.75W-");
        match result {
            AprsData::Position { comment, .. } => {
                assert!(comment.is_none());
            }
            _ => panic!("expected Position, got {:?}", result),
        }
    }

    #[test]
    fn parse_position_alternate_symbol_table() {
        let result = parse_aprs("!4903.50N\\07201.75WE");
        match result {
            AprsData::Position {
                symbol_table,
                symbol_code,
                ..
            } => {
                assert_eq!(symbol_table, '\\');
                assert_eq!(symbol_code, 'E');
            }
            _ => panic!("expected Position, got {:?}", result),
        }
    }

    #[test]
    fn parse_position_with_timestamp_slash() {
        // Format: /DDHHMMz + position
        let result = parse_aprs("/092345z4903.50N/07201.75W>");
        match result {
            AprsData::Position {
                lat,
                lon,
                symbol_code,
                ..
            } => {
                assert!(approx_eq(lat, 49.058333, 0.001));
                assert!(approx_eq(lon, -72.029167, 0.001));
                assert_eq!(symbol_code, '>');
            }
            _ => panic!("expected Position, got {:?}", result),
        }
    }

    #[test]
    fn parse_position_with_timestamp_at() {
        let result = parse_aprs("@092345z4903.50N/07201.75W>");
        match result {
            AprsData::Position { lat, lon, .. } => {
                assert!(approx_eq(lat, 49.058333, 0.001));
                assert!(approx_eq(lon, -72.029167, 0.001));
            }
            _ => panic!("expected Position, got {:?}", result),
        }
    }

    // --- compressed position tests ---

    #[test]
    fn parse_compressed_position() {
        // !/5L!!<*e7>7P[
        // This is a standard compressed APRS position
        let result = parse_aprs("!/5L!!<*e7>7P[");
        match result {
            AprsData::Position {
                symbol_table,
                symbol_code,
                ..
            } => {
                assert_eq!(symbol_table, '/');
                assert_eq!(symbol_code, '>');
            }
            _ => panic!("expected Position, got {:?}", result),
        }
    }

    // --- parse_aprs message tests ---

    #[test]
    fn parse_message_basic() {
        let result = parse_aprs(":BLN1     :Test bulletin");
        match result {
            AprsData::Message {
                addressee,
                text,
                message_id,
            } => {
                assert_eq!(addressee, "BLN1");
                assert_eq!(text, "Test bulletin");
                assert!(message_id.is_none());
            }
            _ => panic!("expected Message, got {:?}", result),
        }
    }

    #[test]
    fn parse_message_with_id() {
        let result = parse_aprs(":OH2MQK-1 :Hello there{123");
        match result {
            AprsData::Message {
                addressee,
                text,
                message_id,
            } => {
                assert_eq!(addressee, "OH2MQK-1");
                assert_eq!(text, "Hello there");
                assert_eq!(message_id, Some("123".to_string()));
            }
            _ => panic!("expected Message, got {:?}", result),
        }
    }

    #[test]
    fn parse_message_empty_text() {
        let result = parse_aprs(":TEST     :");
        match result {
            AprsData::Message {
                addressee, text, ..
            } => {
                assert_eq!(addressee, "TEST");
                assert_eq!(text, "");
            }
            _ => panic!("expected Message, got {:?}", result),
        }
    }

    #[test]
    fn parse_message_full_addressee() {
        let result = parse_aprs(":ABCDEFGHI:text");
        match result {
            AprsData::Message { addressee, .. } => {
                assert_eq!(addressee, "ABCDEFGHI");
            }
            _ => panic!("expected Message, got {:?}", result),
        }
    }

    #[test]
    fn parse_message_too_short() {
        let result = parse_aprs(":SHORT");
        assert_eq!(result, AprsData::Unknown);
    }

    #[test]
    fn parse_message_no_second_colon() {
        // Addressee field doesn't end with ':'
        let result = parse_aprs(":123456789X");
        assert_eq!(result, AprsData::Unknown);
    }

    #[test]
    fn parse_message_with_ack() {
        let result = parse_aprs(":OH2MQK-1 :ack123");
        match result {
            AprsData::Message { text, .. } => {
                assert_eq!(text, "ack123");
            }
            _ => panic!("expected Message, got {:?}", result),
        }
    }

    // --- parse_aprs object tests ---

    #[test]
    fn parse_object_live() {
        let result = parse_aprs(";LEADER   *092345z4903.50N/07201.75W>");
        match result {
            AprsData::Object {
                name,
                live,
                lat,
                lon,
            } => {
                assert_eq!(name, "LEADER");
                assert!(live);
                assert!(approx_eq(lat, 49.058333, 0.001));
                assert!(approx_eq(lon, -72.029167, 0.001));
            }
            _ => panic!("expected Object, got {:?}", result),
        }
    }

    #[test]
    fn parse_object_killed() {
        let result = parse_aprs(";LEADER   _092345z4903.50N/07201.75W>");
        match result {
            AprsData::Object { name, live, .. } => {
                assert_eq!(name, "LEADER");
                assert!(!live);
            }
            _ => panic!("expected Object, got {:?}", result),
        }
    }

    #[test]
    fn parse_object_too_short() {
        let result = parse_aprs(";SHORT");
        assert_eq!(result, AprsData::Unknown);
    }

    #[test]
    fn parse_object_invalid_live_kill() {
        let result = parse_aprs(";LEADER   X092345z4903.50N/07201.75W>");
        assert_eq!(result, AprsData::Unknown);
    }

    #[test]
    fn parse_object_full_name() {
        let result = parse_aprs(";ABCDEFGHI*092345z4903.50N/07201.75W>");
        match result {
            AprsData::Object { name, .. } => {
                assert_eq!(name, "ABCDEFGHI");
            }
            _ => panic!("expected Object, got {:?}", result),
        }
    }

    // --- parse_aprs item tests ---

    #[test]
    fn parse_item_live() {
        let result = parse_aprs(")AID #2!4903.50N/07201.75W>");
        match result {
            AprsData::Item {
                name,
                live,
                lat,
                lon,
            } => {
                assert_eq!(name, "AID #2");
                assert!(live);
                assert!(approx_eq(lat, 49.058333, 0.001));
                assert!(approx_eq(lon, -72.029167, 0.001));
            }
            _ => panic!("expected Item, got {:?}", result),
        }
    }

    #[test]
    fn parse_item_killed() {
        let result = parse_aprs(")AID #2_4903.50N/07201.75W>");
        match result {
            AprsData::Item { name, live, .. } => {
                assert_eq!(name, "AID #2");
                assert!(!live);
            }
            _ => panic!("expected Item, got {:?}", result),
        }
    }

    #[test]
    fn parse_item_short_name() {
        let result = parse_aprs(")AB!4903.50N/07201.75W>");
        match result {
            AprsData::Item { name, .. } => {
                assert_eq!(name, "AB");
            }
            _ => panic!("expected Item, got {:?}", result),
        }
    }

    #[test]
    fn parse_item_too_short() {
        let result = parse_aprs(")A");
        assert_eq!(result, AprsData::Unknown);
    }

    #[test]
    fn parse_item_no_separator() {
        // No ! or _ found in name area
        let result = parse_aprs(")ABCDEFGHIJ");
        assert_eq!(result, AprsData::Unknown);
    }

    // --- parse_aprs unknown/edge cases ---

    #[test]
    fn parse_empty_payload() {
        assert_eq!(parse_aprs(""), AprsData::Unknown);
    }

    #[test]
    fn parse_unknown_data_type() {
        assert_eq!(parse_aprs("~test data"), AprsData::Unknown);
    }

    #[test]
    fn parse_single_char_payload() {
        assert_eq!(parse_aprs("!"), AprsData::Unknown);
    }

    #[test]
    fn parse_position_truncated() {
        assert_eq!(parse_aprs("!4903.50N/072"), AprsData::Unknown);
    }

    // --- decode_base91_4 tests ---

    #[test]
    fn decode_base91_minimum_value() {
        // Four '!' (ASCII 33) characters = 0
        assert_eq!(decode_base91_4(b"!!!!"), Some(0));
    }

    #[test]
    fn decode_base91_known_value() {
        // "5L!!" from the APRS spec example
        let result = decode_base91_4(b"5L!!").unwrap();
        assert!(result > 0);
    }

    #[test]
    fn decode_base91_too_short() {
        assert!(decode_base91_4(b"!!").is_none());
    }

    #[test]
    fn decode_base91_invalid_char() {
        // Space (ASCII 32) is below the minimum (33)
        assert!(decode_base91_4(b" !!!").is_none());
    }

    #[test]
    fn decode_base91_high_char_invalid() {
        // ASCII 125 '}' is above the maximum (124 '|')
        assert!(decode_base91_4(b"}!!!").is_none());
    }

    // --- AprsData derives ---

    #[test]
    fn aprs_data_debug_format() {
        let data = AprsData::Unknown;
        let debug = format!("{:?}", data);
        assert!(debug.contains("Unknown"));
    }

    #[test]
    fn aprs_data_clone() {
        let data = AprsData::Position {
            lat: 49.0,
            lon: -72.0,
            symbol_table: '/',
            symbol_code: '>',
            comment: Some("test".to_string()),
        };
        let cloned = data.clone();
        assert_eq!(data, cloned);
    }

    #[test]
    fn aprs_data_position_equality() {
        let a = AprsData::Position {
            lat: 49.0,
            lon: -72.0,
            symbol_table: '/',
            symbol_code: '>',
            comment: None,
        };
        let b = AprsData::Position {
            lat: 49.0,
            lon: -72.0,
            symbol_table: '/',
            symbol_code: '>',
            comment: None,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn aprs_data_message_equality() {
        let a = AprsData::Message {
            addressee: "TEST".to_string(),
            text: "hello".to_string(),
            message_id: Some("1".to_string()),
        };
        let b = AprsData::Message {
            addressee: "TEST".to_string(),
            text: "hello".to_string(),
            message_id: Some("1".to_string()),
        };
        assert_eq!(a, b);
    }

    // --- Real-world packet examples ---

    #[test]
    fn parse_real_position_report() {
        let result = parse_aprs("!6029.50N/02505.43E>PHG2360/RELAY,WIDE");
        match result {
            AprsData::Position {
                lat,
                lon,
                symbol_table,
                symbol_code,
                comment,
            } => {
                assert!(approx_eq(lat, 60.491667, 0.001));
                assert!(approx_eq(lon, 25.090500, 0.001));
                assert_eq!(symbol_table, '/');
                assert_eq!(symbol_code, '>');
                assert_eq!(comment, Some("PHG2360/RELAY,WIDE".to_string()));
            }
            _ => panic!("expected Position"),
        }
    }

    #[test]
    fn parse_real_message() {
        let result = parse_aprs(":BLN1     :Weather report for today");
        match result {
            AprsData::Message {
                addressee, text, ..
            } => {
                assert_eq!(addressee, "BLN1");
                assert_eq!(text, "Weather report for today");
            }
            _ => panic!("expected Message"),
        }
    }

    // --- extract_position tests ---

    #[test]
    fn extract_position_from_tnc2() {
        let pos = extract_position("OH2MQK-1>APRS:!6029.50N/02505.43E>").unwrap();
        assert!(approx_eq(pos.0, 60.491667, 0.001));
        assert!(approx_eq(pos.1, 25.090500, 0.001));
    }

    #[test]
    fn extract_position_from_message() {
        assert!(extract_position("OH2MQK-1>APRS::BLN1     :test").is_none());
    }

    #[test]
    fn extract_position_no_payload() {
        assert!(extract_position("OH2MQK-1>APRS").is_none());
    }
}
