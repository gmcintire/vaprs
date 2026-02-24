// D-PRS (D-STAR Position Reporting System) to APRS conversion
//
// D-PRS transmits GPS positions in D-STAR digital voice data streams.
// This module converts D-PRS position reports into standard APRS format
// for gating to the APRS-IS network.
//
// D-PRS callsign format: 8 characters, space-padded, with optional
// D-STAR module indicator suffix (A/B/C/G).

/// Convert a D-PRS position report to an APRS-format position string.
///
/// Produces a standard APRS position report with the `!` data type
/// identifier, formatted as: `!DDMM.MMN/DDDMM.MMESymbol comment`
///
/// # Arguments
/// * `callsign` - Source callsign (will be included in the TNC2 header)
/// * `latitude` - Latitude in decimal degrees (positive = North)
/// * `longitude` - Longitude in decimal degrees (positive = East)
/// * `symbol_table` - APRS symbol table identifier ('/' or '\\')
/// * `symbol_code` - APRS symbol code character
/// * `comment` - Free-text comment to append
pub fn dprs_to_aprs(
    callsign: &str,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
) -> String {
    let lat_hem = if latitude >= 0.0 { 'N' } else { 'S' };
    let lon_hem = if longitude >= 0.0 { 'E' } else { 'W' };

    let lat_abs = latitude.abs();
    let lon_abs = longitude.abs();

    let lat_deg = lat_abs as u32;
    let lat_min = (lat_abs - lat_deg as f64) * 60.0;

    let lon_deg = lon_abs as u32;
    let lon_min = (lon_abs - lon_deg as f64) * 60.0;

    let position = format!(
        "!{:02}{:05.2}{}{}{:03}{:05.2}{}{}",
        lat_deg, lat_min, lat_hem, symbol_table, lon_deg, lon_min, lon_hem, symbol_code,
    );

    let dest = "APDPRS";

    if comment.is_empty() {
        format!("{callsign}>{dest}:{position}")
    } else {
        format!("{callsign}>{dest}:{position}{comment}")
    }
}

/// Parse a D-PRS callsign from the raw 8-character D-STAR format.
///
/// D-STAR callsigns are 8 characters with trailing space padding and
/// an optional module indicator (A, B, C, or G) as the last non-space
/// character. This function strips whitespace and removes the module
/// indicator if present, returning the cleaned callsign.
///
/// Returns `None` if the input is empty or contains only whitespace.
pub fn parse_dprs_callsign(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // D-STAR module indicators are single characters A/B/C/G appended
    // after a space to the callsign. In the 8-char field, the format is:
    // "CALL  G " where G is the module indicator.
    // After trimming, if the last char is a module indicator preceded by
    // a space in the original, strip it.
    let bytes = trimmed.as_bytes();
    let last = bytes[bytes.len() - 1];

    // Check if the raw string had a space before this last character,
    // indicating it's a module indicator rather than part of the callsign
    if bytes.len() >= 2 && matches!(last, b'A' | b'B' | b'C' | b'G') {
        // Look at the raw string to see if there was a space before the module letter
        let raw_trimmed_end = raw.trim_end();
        if raw_trimmed_end.len() >= 2 {
            let before_last = raw_trimmed_end.as_bytes()[raw_trimmed_end.len() - 2];
            if before_last == b' ' {
                // This is a module indicator - strip the space and indicator
                let without_module = raw_trimmed_end[..raw_trimmed_end.len() - 1].trim();
                if without_module.is_empty() {
                    return None;
                }
                return Some(without_module.to_string());
            }
        }
    }

    Some(trimmed.to_string())
}

/// Map a D-PRS symbol byte to APRS symbol table and code.
///
/// D-PRS uses simplified symbol codes that map to specific APRS symbol
/// table/code pairs. Returns `('/', '/')` (dot) as default for unknown symbols.
pub fn dprs_symbol_to_aprs(symbol: u8) -> (char, char) {
    match symbol {
        // Common D-PRS symbol mappings
        b'A' => ('/', 'a'),  // Ambulance
        b'E' => ('\\', 'E'), // Eyeball
        b'H' => ('/', 'h'),  // Hospital
        b'I' => ('/', '>'),  // Car
        b'J' => ('/', 'j'),  // Jeep
        b'O' => ('/', 'O'),  // Balloon
        b'R' => ('/', 'R'),  // Recreational vehicle
        b'S' => ('/', 's'),  // Ship/boat
        b'T' => ('/', 'T'),  // Truck
        b'V' => ('/', 'v'),  // Van
        b'Y' => ('/', 'Y'),  // Yacht
        b'>' => ('/', '>'),  // Car
        _ => ('/', '/'),     // Default: dot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- dprs_to_aprs tests ---

    #[test]
    fn dprs_to_aprs_north_east_position() {
        let result = dprs_to_aprs("OH2MQK-1", 60.491667, 25.090500, '/', '>', "D-PRS");
        assert!(result.starts_with("OH2MQK-1>APDPRS:!"));
        // Should contain latitude in DDMM.MM format
        assert!(result.contains("6029.50N"));
        // Should contain longitude in DDDMM.MM format
        assert!(result.contains("02505.43E"));
        assert!(result.contains("D-PRS"));
    }

    #[test]
    fn dprs_to_aprs_south_west_position() {
        let result = dprs_to_aprs("VK2ABC", -33.85, -151.21, '/', '-', "");
        assert!(result.contains('S'));
        assert!(result.contains('W'));
    }

    #[test]
    fn dprs_to_aprs_zero_position() {
        let result = dprs_to_aprs("TEST", 0.0, 0.0, '/', '/', "");
        assert!(result.contains("0000.00N"));
        assert!(result.contains("00000.00E"));
    }

    #[test]
    fn dprs_to_aprs_empty_comment() {
        let result = dprs_to_aprs("TEST", 49.058333, -72.029167, '/', '>', "");
        // Should not have trailing content after symbol code
        assert!(!result.ends_with(' '));
        assert!(result.contains("APDPRS"));
    }

    #[test]
    fn dprs_to_aprs_with_comment() {
        let result = dprs_to_aprs("N0CALL", 34.05, -118.25, '/', '>', "Los Angeles");
        assert!(result.ends_with("Los Angeles"));
    }

    #[test]
    fn dprs_to_aprs_symbol_placement() {
        let result = dprs_to_aprs("TEST", 49.058333, -72.029167, '/', '>', "");
        // Symbol table '/' should be between lat and lon
        // Symbol code '>' should be after lon
        // Format: !DDMM.MMN/DDDMM.MME>
        let payload = result.split(':').nth(1).unwrap();
        assert!(payload.starts_with('!'));
        // Check the symbol table char is between lat and lon hemispheres
        let n_pos = payload.find('N').unwrap();
        assert_eq!(payload.as_bytes()[n_pos + 1], b'/'); // symbol table
    }

    #[test]
    fn dprs_to_aprs_alternate_symbol_table() {
        let result = dprs_to_aprs("TEST", 49.0, -72.0, '\\', 'E', "");
        let payload = result.split(':').nth(1).unwrap();
        // Check for alternate symbol table indicator
        assert!(payload.contains('\\'));
    }

    #[test]
    fn dprs_to_aprs_destination_is_apdprs() {
        let result = dprs_to_aprs("SRC", 0.0, 0.0, '/', '/', "");
        assert!(result.contains(">APDPRS:"));
    }

    #[test]
    fn dprs_to_aprs_negative_latitude() {
        let result = dprs_to_aprs("ZL1ABC", -41.2865, 174.7762, '/', '>', "");
        assert!(result.contains('S'));
        // 41 degrees 17.19 minutes
        assert!(result.contains("4117.19S"));
    }

    #[test]
    fn dprs_to_aprs_negative_longitude() {
        let result = dprs_to_aprs("W1AW", 41.714775, -72.727260, '/', '>', "");
        assert!(result.contains('W'));
        assert!(result.contains("07243.64W"));
    }

    // --- parse_dprs_callsign tests ---

    #[test]
    fn parse_simple_callsign() {
        let result = parse_dprs_callsign("OH2MQK  ");
        assert_eq!(result, Some("OH2MQK".to_string()));
    }

    #[test]
    fn parse_callsign_with_ssid() {
        let result = parse_dprs_callsign("OH2MQK-1");
        assert_eq!(result, Some("OH2MQK-1".to_string()));
    }

    #[test]
    fn parse_callsign_with_module_indicator() {
        let result = parse_dprs_callsign("OH2MQK G");
        assert_eq!(result, Some("OH2MQK".to_string()));
    }

    #[test]
    fn parse_callsign_with_module_a() {
        let result = parse_dprs_callsign("W1AW   A");
        assert_eq!(result, Some("W1AW".to_string()));
    }

    #[test]
    fn parse_callsign_with_module_b() {
        let result = parse_dprs_callsign("W1AW   B");
        assert_eq!(result, Some("W1AW".to_string()));
    }

    #[test]
    fn parse_callsign_with_module_c() {
        let result = parse_dprs_callsign("N0CALL C");
        assert_eq!(result, Some("N0CALL".to_string()));
    }

    #[test]
    fn parse_empty_callsign_returns_none() {
        assert_eq!(parse_dprs_callsign(""), None);
    }

    #[test]
    fn parse_whitespace_only_returns_none() {
        assert_eq!(parse_dprs_callsign("        "), None);
    }

    #[test]
    fn parse_callsign_no_padding() {
        let result = parse_dprs_callsign("OH2MQK-1");
        assert_eq!(result, Some("OH2MQK-1".to_string()));
    }

    #[test]
    fn parse_callsign_ending_in_a_without_space_is_kept() {
        // If the character before the module letter is NOT a space,
        // it's part of the callsign, not a module indicator
        let result = parse_dprs_callsign("TESTCA  ");
        assert_eq!(result, Some("TESTCA".to_string()));
    }

    // --- dprs_symbol_to_aprs tests ---

    #[test]
    fn symbol_car() {
        let (table, code) = dprs_symbol_to_aprs(b'>');
        assert_eq!(table, '/');
        assert_eq!(code, '>');
    }

    #[test]
    fn symbol_ambulance() {
        let (table, code) = dprs_symbol_to_aprs(b'A');
        assert_eq!(table, '/');
        assert_eq!(code, 'a');
    }

    #[test]
    fn symbol_hospital() {
        let (table, code) = dprs_symbol_to_aprs(b'H');
        assert_eq!(table, '/');
        assert_eq!(code, 'h');
    }

    #[test]
    fn symbol_unknown_defaults_to_dot() {
        let (table, code) = dprs_symbol_to_aprs(b'?');
        assert_eq!(table, '/');
        assert_eq!(code, '/');
    }

    #[test]
    fn symbol_ship() {
        let (table, code) = dprs_symbol_to_aprs(b'S');
        assert_eq!(table, '/');
        assert_eq!(code, 's');
    }

    #[test]
    fn symbol_eyeball() {
        let (table, code) = dprs_symbol_to_aprs(b'E');
        assert_eq!(table, '\\');
        assert_eq!(code, 'E');
    }
}
