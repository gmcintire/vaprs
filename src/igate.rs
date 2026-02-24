// Rx-iGate: packet filtering and forwarding from RF to APRS-IS.
//
// Implements the igate_to_aprsis() logic from aprx igate.c.
// Filters packets received on RF and forwards valid ones to APRS-IS
// with appropriate q-construct headers (qAR = heard on RF by iGate).

use crate::packet::Packet;

/// Prefixes that are forbidden in the source callsign field.
const FORBIDDEN_SOURCE_PREFIXES: &[&str] = &[
    "WIDE", "RELAY", "TRACE", "TCPIP", "TCPXX", "NOCALL", "N0CALL",
];

/// Prefixes that are forbidden in the destination callsign field.
const FORBIDDEN_DEST_PREFIXES: &[&str] =
    &["TCPIP", "TCPXX", "NOGATE", "RFONLY", "NOCALL", "N0CALL"];

/// Prefixes that are forbidden in via (digipeater path) callsigns.
const FORBIDDEN_VIA_PREFIXES: &[&str] = &["RFONLY", "NOGATE", "TCPIP", "TCPXX"];

/// Check if a source callsign is forbidden for iGating.
///
/// Returns true if the callsign starts with any forbidden source prefix
/// (case-sensitive, prefix match).
pub fn is_forbidden_source(callsign: &str) -> bool {
    FORBIDDEN_SOURCE_PREFIXES
        .iter()
        .any(|prefix| callsign.starts_with(prefix))
}

/// Check if a destination callsign is forbidden for iGating.
///
/// Returns true if the callsign starts with any forbidden destination prefix
/// (case-sensitive, prefix match).
pub fn is_forbidden_destination(callsign: &str) -> bool {
    FORBIDDEN_DEST_PREFIXES
        .iter()
        .any(|prefix| callsign.starts_with(prefix))
}

/// Check if a via callsign is forbidden for iGating.
///
/// Returns true if the callsign starts with any forbidden via prefix
/// (case-sensitive, prefix match). The '*' digipeated marker is stripped
/// before checking.
pub fn is_forbidden_via(callsign: &str) -> bool {
    let clean = callsign.strip_suffix('*').unwrap_or(callsign);
    FORBIDDEN_VIA_PREFIXES
        .iter()
        .any(|prefix| clean.starts_with(prefix))
}

/// Extract via callsigns from a TNC2 address string.
///
/// Given "SRC>DST,VIA1,VIA2*", returns vec!["VIA1", "VIA2*"].
/// If there are no via entries, returns an empty vec.
fn extract_via_callsigns(addresses: &str) -> Vec<&str> {
    // Split on '>' to get source and the rest
    let after_gt = match addresses.split('>').nth(1) {
        Some(rest) => rest,
        None => return Vec::new(),
    };

    // Split rest by ',' - first element is destination, rest are via
    let parts: Vec<&str> = after_gt.split(',').collect();
    if parts.len() > 1 {
        parts[1..].to_vec()
    } else {
        Vec::new()
    }
}

/// Check if a packet should be gated to APRS-IS.
///
/// Returns `Some(formatted_line)` if the packet should be forwarded,
/// `None` if it should be discarded.
///
/// The formatted line has the q-construct appended:
/// `SOURCE>DEST,VIA1,VIA2,qAR,GATECALL:payload`
pub fn gate_to_aprsis(packet: &Packet, gate_call: &str) -> Option<String> {
    let payload = packet.payload();

    // Drop APRS query packets (payload starts with '?')
    if payload.starts_with('?') {
        return None;
    }

    // Handle 3rd-party frames: payload starts with '}'
    if let Some(inner) = payload.strip_prefix('}') {
        // The inner frame is a full TNC2 packet string
        // Recursively filter the inner frame
        let inner_packet = Packet::new(inner, &packet.source_interface, packet.is_aprs);
        return gate_to_aprsis(&inner_packet, gate_call);
    }

    // Check forbidden source
    if is_forbidden_source(packet.source_call()) {
        return None;
    }

    // Check forbidden destination
    if is_forbidden_destination(packet.dest_call()) {
        return None;
    }

    // Check forbidden via callsigns
    let via_calls = extract_via_callsigns(packet.addresses());
    for via in &via_calls {
        if is_forbidden_via(via) {
            return None;
        }
    }

    // Build the gated packet with q-construct
    let addresses = packet.addresses();
    Some(format!("{},qAR,{}:{}", addresses, gate_call, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to build a Packet from a TNC2 string
    fn pkt(tnc2: &str) -> Packet {
        Packet::new(tnc2, "radio0", true)
    }

    // --- Forbidden source tests ---

    #[test]
    fn test_forbidden_source_wide() {
        assert!(gate_to_aprsis(&pkt("WIDE1-1>APRS:data"), "GATE").is_none());
    }

    #[test]
    fn test_forbidden_source_relay() {
        assert!(gate_to_aprsis(&pkt("RELAY>APRS:data"), "GATE").is_none());
    }

    #[test]
    fn test_forbidden_source_tcpip() {
        assert!(gate_to_aprsis(&pkt("TCPIP>APRS:data"), "GATE").is_none());
    }

    #[test]
    fn test_forbidden_source_nocall() {
        assert!(gate_to_aprsis(&pkt("NOCALL>APRS:data"), "GATE").is_none());
    }

    #[test]
    fn test_forbidden_source_n0call() {
        assert!(gate_to_aprsis(&pkt("N0CALL>APRS:data"), "GATE").is_none());
    }

    // --- Forbidden destination tests ---

    #[test]
    fn test_forbidden_dest_tcpip() {
        assert!(gate_to_aprsis(&pkt("TEST>TCPIP:data"), "GATE").is_none());
    }

    #[test]
    fn test_forbidden_dest_nogate() {
        assert!(gate_to_aprsis(&pkt("TEST>NOGATE:data"), "GATE").is_none());
    }

    #[test]
    fn test_forbidden_dest_rfonly() {
        assert!(gate_to_aprsis(&pkt("TEST>RFONLY:data"), "GATE").is_none());
    }

    // --- Forbidden via tests ---

    #[test]
    fn test_forbidden_via_rfonly() {
        assert!(gate_to_aprsis(&pkt("TEST>APRS,RFONLY:data"), "GATE").is_none());
    }

    #[test]
    fn test_forbidden_via_nogate() {
        assert!(gate_to_aprsis(&pkt("TEST>APRS,NOGATE:data"), "GATE").is_none());
    }

    #[test]
    fn test_forbidden_via_tcpip() {
        assert!(gate_to_aprsis(&pkt("TEST>APRS,TCPIP:data"), "GATE").is_none());
    }

    // --- Query packet rejection ---

    #[test]
    fn test_query_packet_rejected() {
        assert!(gate_to_aprsis(&pkt("TEST>APRS:?APRS?"), "GATE").is_none());
    }

    // --- Valid packet gating ---

    #[test]
    fn test_valid_packet_gated() {
        let result = gate_to_aprsis(&pkt("OH2MQK-1>APRS,WIDE1-1*:!6029.50N/02505.43E>"), "GATE");
        assert!(result.is_some());
        let line = result.unwrap();
        assert!(line.contains("qAR,GATE"));
        assert!(line.contains("!6029.50N/02505.43E>"));
    }

    #[test]
    fn test_qar_construct_format() {
        let result = gate_to_aprsis(
            &pkt("OH2MQK-1>APRS,WIDE1-1*:!6029.50N/02505.43E>"),
            "MYCALL",
        );
        let line = result.unwrap();
        assert_eq!(
            line,
            "OH2MQK-1>APRS,WIDE1-1*,qAR,MYCALL:!6029.50N/02505.43E>"
        );
    }

    // --- Third-party frame handling ---

    #[test]
    fn test_third_party_frame() {
        // Outer frame wraps an inner valid packet
        let result = gate_to_aprsis(&pkt("OH2MQK>APRS:}TEST>APRS:valid"), "GATE");
        assert!(result.is_some());
        let line = result.unwrap();
        // The inner frame is what gets gated
        assert_eq!(line, "TEST>APRS,qAR,GATE:valid");
    }

    #[test]
    fn test_third_party_with_forbidden() {
        // Inner frame has forbidden source - should be rejected
        let result = gate_to_aprsis(&pkt("OH2MQK>APRS:}NOCALL>APRS:data"), "GATE");
        assert!(result.is_none());
    }

    // --- Helper function tests ---

    #[test]
    fn test_valid_source_passes() {
        assert!(!is_forbidden_source("OH2MQK-1"));
    }

    #[test]
    fn test_forbidden_source_prefix_match() {
        // "WIDE2-2" starts with "WIDE" so it is forbidden
        assert!(is_forbidden_source("WIDE2-2"));
    }

    #[test]
    fn test_via_with_star_suffix() {
        // WIDE1-1* in via path - WIDE is NOT a forbidden via prefix,
        // only RFONLY/NOGATE/TCPIP/TCPXX are forbidden in via.
        // So WIDE1-1* should NOT be forbidden in via.
        assert!(!is_forbidden_via("WIDE1-1*"));
    }

    #[test]
    fn test_empty_via_list() {
        // Packet with no via entries should pass
        let result = gate_to_aprsis(&pkt("OH2MQK>APRS:data"), "GATE");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "OH2MQK>APRS,qAR,GATE:data");
    }

    // --- Additional edge case tests ---

    #[test]
    fn test_forbidden_source_trace() {
        assert!(is_forbidden_source("TRACE5-5"));
    }

    #[test]
    fn test_forbidden_source_tcpxx() {
        assert!(is_forbidden_source("TCPXX"));
    }

    #[test]
    fn test_forbidden_dest_tcpxx() {
        assert!(is_forbidden_destination("TCPXX"));
    }

    #[test]
    fn test_forbidden_dest_nocall() {
        assert!(is_forbidden_destination("NOCALL"));
    }

    #[test]
    fn test_forbidden_dest_n0call() {
        assert!(is_forbidden_destination("N0CALL"));
    }

    #[test]
    fn test_forbidden_via_tcpxx() {
        assert!(is_forbidden_via("TCPXX"));
    }

    #[test]
    fn test_forbidden_via_with_star_stripped() {
        // RFONLY* should still be forbidden after stripping *
        assert!(is_forbidden_via("RFONLY*"));
    }

    #[test]
    fn test_normal_callsign_not_forbidden_dest() {
        assert!(!is_forbidden_destination("APRS"));
    }

    #[test]
    fn test_normal_callsign_not_forbidden_via() {
        assert!(!is_forbidden_via("OH2RDK"));
    }

    #[test]
    fn test_extract_via_no_via() {
        let vias = extract_via_callsigns("SRC>DST");
        assert!(vias.is_empty());
    }

    #[test]
    fn test_extract_via_single() {
        let vias = extract_via_callsigns("SRC>DST,VIA1");
        assert_eq!(vias, vec!["VIA1"]);
    }

    #[test]
    fn test_extract_via_multiple() {
        let vias = extract_via_callsigns("SRC>DST,VIA1,VIA2*,VIA3");
        assert_eq!(vias, vec!["VIA1", "VIA2*", "VIA3"]);
    }

    #[test]
    fn test_multiple_via_with_one_forbidden() {
        // Second via is NOGATE - should reject
        let result = gate_to_aprsis(&pkt("TEST>APRS,WIDE1-1,NOGATE:data"), "GATE");
        assert!(result.is_none());
    }

    #[test]
    fn test_query_with_different_query() {
        // Any payload starting with '?' is a query
        assert!(gate_to_aprsis(&pkt("TEST>APRS:?WX?"), "GATE").is_none());
    }

    #[test]
    fn test_payload_starting_with_normal_char() {
        // Payload starting with '!' (position report) should pass
        let result = gate_to_aprsis(&pkt("TEST>APRS:!4903.50N/07201.75W-"), "GATE");
        assert!(result.is_some());
    }
}
