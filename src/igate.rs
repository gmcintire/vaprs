// Rx-iGate: packet filtering and forwarding from RF to APRS-IS.
//
// Implements the igate_to_aprsis() logic from aprx igate.c.
// Filters packets received on RF and forwards valid ones to APRS-IS
// with appropriate q-construct headers (qAR = heard on RF by iGate).

use crate::history::HistoryDb;
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

/// Maximum recursion depth for third-party frame unwrapping.
const MAX_THIRD_PARTY_DEPTH: u8 = 3;

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

/// Truncate a string at the first CR or LF character to prevent CRLF injection.
pub(crate) fn sanitize_line(s: &str) -> &str {
    match s.find(&['\r', '\n'][..]) {
        Some(pos) => &s[..pos],
        None => s,
    }
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
    gate_to_aprsis_inner(packet, gate_call, 0)
}

fn gate_to_aprsis_inner(packet: &Packet, gate_call: &str, depth: u8) -> Option<String> {
    let payload = packet.payload();

    // Drop APRS query packets (payload starts with '?')
    if payload.starts_with('?') {
        return None;
    }

    // Handle 3rd-party frames: payload starts with '}'
    if let Some(inner) = payload.strip_prefix('}') {
        if depth >= MAX_THIRD_PARTY_DEPTH {
            return None;
        }
        let inner_packet = Packet::new(inner, &packet.source_interface, packet.is_aprs);
        return gate_to_aprsis_inner(&inner_packet, gate_call, depth + 1);
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
    let addresses = sanitize_line(packet.addresses());
    let payload = sanitize_line(payload);
    Some(format!("{},qAR,{}:{}", addresses, gate_call, payload))
}

/// Additional forbidden prefixes for Tx-iGate via checking.
/// Packets from APRS-IS containing these in any address field or via path are rejected.
const TX_FORBIDDEN_VIA: &[&str] = &["TCPXX", "NOGATE", "RFONLY", "qAX"];

/// Check if a packet from APRS-IS should be gated to RF.
/// Returns `Some(third_party_frame)` if it should be gated, `None` if not.
///
/// The third-party frame format is: `}SRC>DST,TCPIP,GATECALL*:payload`
///
/// Parameters:
/// - `packet`: the packet received from APRS-IS
/// - `gate_call`: this iGate's callsign
/// - `history`: the RF history database
pub fn gate_to_rf(packet: &Packet, gate_call: &str, history: &HistoryDb) -> Option<String> {
    let payload = packet.payload();

    // Drop query packets (payload starts with '?')
    if payload.starts_with('?') {
        return None;
    }

    // Drop 3rd-party frames from APRS-IS (payload starts with '}')
    if payload.starts_with('}') {
        return None;
    }

    // Only gate message packets (payload starts with ':')
    if !payload.starts_with(':') {
        return None;
    }

    // Check forbidden addresses: source, destination, and via path
    let source = packet.source_call();
    let dest = packet.dest_call();

    // Check source against Tx forbidden prefixes
    if TX_FORBIDDEN_VIA
        .iter()
        .any(|prefix| source.starts_with(prefix))
    {
        return None;
    }

    // Check destination against Tx forbidden prefixes
    if TX_FORBIDDEN_VIA
        .iter()
        .any(|prefix| dest.starts_with(prefix))
    {
        return None;
    }

    // Check via path against Tx forbidden prefixes
    let via_calls = extract_via_callsigns(packet.addresses());
    for via in &via_calls {
        let clean = via.strip_suffix('*').unwrap_or(via);
        if TX_FORBIDDEN_VIA
            .iter()
            .any(|prefix| clean.starts_with(prefix))
        {
            return None;
        }
    }

    // Verify the message addressee was heard recently on RF.
    // The recipient is encoded in the payload as the 9-char addressee field,
    // not the packet header destination.
    let addressee = extract_message_addressee(payload);
    if !history.was_heard(addressee) {
        return None;
    }

    // Verify source station was NOT heard recently on RF
    // (if they're local, they don't need internet-to-RF gating)
    if history.was_heard(source) {
        return None;
    }

    // Format as 3rd-party frame: }SRC>DST,TCPIP,GATECALL*:payload
    let source = sanitize_line(source);
    let dest = sanitize_line(dest);
    let payload = sanitize_line(payload);
    Some(format!(
        "}}{}>{},TCPIP,{}*:{}",
        source, dest, gate_call, payload
    ))
}

/// Extract the addressee from an APRS message payload.
/// Message format: `:ADDRESSEE :message text{id`
/// The addressee is 9 characters after the initial ':', padded with spaces.
fn extract_message_addressee(payload: &str) -> &str {
    // Payload starts with ':', addressee is next 9 chars, then ':'
    let after_colon = &payload[1..]; // skip the leading ':'
                                     // Find the next ':' which terminates the addressee field
    match after_colon.find(':') {
        Some(pos) => after_colon[..pos].trim(),
        None => after_colon.trim(),
    }
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

    // --- Tx-iGate (gate_to_rf) tests ---

    fn make_history() -> HistoryDb {
        HistoryDb::new(crate::history::DEFAULT_TTL)
    }

    #[test]
    fn test_gate_to_rf_valid_message() {
        // APRS message to station heard on RF gets gated
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS,qAR,SOMEGATE::WA1ABC   :Hello{123");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_some());
    }

    #[test]
    fn test_gate_to_rf_dest_not_heard() {
        // If the message addressee is not in historydb, don't gate
        let history = make_history();
        // WA1ABC not heard on RF
        let p = pkt("KB1ABC>APRS::WA1ABC   :Hello{123");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    #[test]
    fn test_gate_to_rf_source_heard_on_rf() {
        // If source IS heard on RF, don't gate (they're local, no need)
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);
        history.heard("KB1ABC", "radio0", None); // source also on RF

        let p = pkt("KB1ABC>APRS::WA1ABC   :Hello{123");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    #[test]
    fn test_gate_to_rf_not_a_message() {
        // Position reports (payload starting with '!') NOT gated
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS:!4903.50N/07201.75W-");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    #[test]
    fn test_gate_to_rf_third_party_rejected() {
        // 3rd-party frames from APRS-IS rejected
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS:}TEST>APRS::WA1ABC   :Hello");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    #[test]
    fn test_gate_to_rf_forbidden_tcpxx() {
        // TCPXX in via path rejected
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS,TCPXX::WA1ABC   :Hello{123");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    #[test]
    fn test_gate_to_rf_forbidden_nogate() {
        // NOGATE in via path rejected
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS,NOGATE::WA1ABC   :Hello{123");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    #[test]
    fn test_gate_to_rf_forbidden_rfonly() {
        // RFONLY in via path rejected
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS,RFONLY::WA1ABC   :Hello{123");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    #[test]
    fn test_gate_to_rf_forbidden_qax() {
        // qAX in via path rejected
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS,qAX::WA1ABC   :Hello{123");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    #[test]
    fn test_gate_to_rf_format() {
        // Verify 3rd-party frame format: }SRC>DST,TCPIP,GATECALL*:payload
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS::WA1ABC   :Hello{123");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_some());
        let frame = result.unwrap();
        assert_eq!(frame, "}KB1ABC>APRS,TCPIP,MYGATE*::WA1ABC   :Hello{123");
    }

    #[test]
    fn test_gate_to_rf_query_rejected() {
        // Query packets ('?') rejected for Tx
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);

        let p = pkt("KB1ABC>APRS:?APRS?");
        let result = gate_to_rf(&p, "MYGATE", &history);
        assert!(result.is_none());
    }

    // --- CRLF injection sanitization tests ---

    #[test]
    fn test_sanitize_line_clean() {
        assert_eq!(sanitize_line("hello world"), "hello world");
    }

    #[test]
    fn test_sanitize_line_cr() {
        assert_eq!(sanitize_line("hello\rinjected"), "hello");
    }

    #[test]
    fn test_sanitize_line_lf() {
        assert_eq!(sanitize_line("hello\ninjected"), "hello");
    }

    #[test]
    fn test_sanitize_line_crlf() {
        assert_eq!(sanitize_line("hello\r\ninjected"), "hello");
    }

    #[test]
    fn test_sanitize_line_empty() {
        assert_eq!(sanitize_line(""), "");
    }

    #[test]
    fn test_gate_to_aprsis_crlf_in_payload() {
        let p = pkt("TEST>APRS:data\r\nINJECTED>APRS:evil");
        let result = gate_to_aprsis(&p, "GATE").unwrap();
        assert!(!result.contains('\r'));
        assert!(!result.contains('\n'));
        assert!(result.contains("data"));
        assert!(!result.contains("INJECTED"));
    }

    #[test]
    fn test_gate_to_rf_crlf_in_payload() {
        let mut history = make_history();
        history.heard("WA1ABC", "radio0", None);
        let p = pkt("KB1ABC>APRS::WA1ABC   :Hello\r\nINJECTED");
        let result = gate_to_rf(&p, "MYGATE", &history).unwrap();
        assert!(!result.contains('\r'));
        assert!(!result.contains('\n'));
    }

    // --- Third-party frame recursion depth limit tests ---

    #[test]
    fn test_deeply_nested_third_party_frames() {
        // 10 levels of properly-formed nesting - should be rejected (depth > MAX_THIRD_PARTY_DEPTH)
        // Each level is a valid third-party frame: }CALL>APRS:}CALL>APRS:...
        let mut nested = "TEST>APRS:data".to_string();
        for i in (1..=10).rev() {
            nested = format!("L{}>APRS:}}{}", i, nested);
        }
        let p = pkt(&nested);
        let result = gate_to_aprsis(&p, "GATE");
        assert!(
            result.is_none(),
            "deeply nested third-party frames should be rejected"
        );
    }

    #[test]
    fn test_third_party_frame_at_max_depth() {
        // Exactly 3 levels of nesting should still work
        // }}}TEST>APRS:data  (3 unwraps)
        let p = pkt("L1>APRS:}L2>APRS:}L3>APRS:}TEST>APRS:data");
        let result = gate_to_aprsis(&p, "GATE");
        assert!(result.is_some(), "3 levels of nesting should be allowed");
    }

    #[test]
    fn test_third_party_frame_exceeds_max_depth() {
        // 4 levels of nesting should be rejected
        let p = pkt("L1>APRS:}L2>APRS:}L3>APRS:}L4>APRS:}TEST>APRS:data");
        let result = gate_to_aprsis(&p, "GATE");
        assert!(result.is_none(), "4 levels of nesting should be rejected");
    }
}
