pub mod dedupe;

use std::time::Instant;

use crate::packet::Packet;

/// Configuration for a digipeater instance.
pub struct DigiConfig {
    /// Callsign of the transmitter (inserted into path).
    pub transmitter_call: String,
    /// Maximum N value allowed in a WIDEn-N request (default 4).
    pub max_hops_req: u8,
    /// Maximum number of already-digipeated hops allowed (default 4).
    pub max_hops_done: u8,
    /// Keywords that trigger New-N digipeating (e.g., ["WIDE", "RELAY"]).
    pub wide_keywords: Vec<String>,
}

impl Default for DigiConfig {
    fn default() -> Self {
        Self {
            transmitter_call: String::new(),
            max_hops_req: 4,
            max_hops_done: 4,
            wide_keywords: vec!["WIDE".to_string(), "RELAY".to_string()],
        }
    }
}

/// Result of digipeater processing.
#[derive(Debug, PartialEq, Eq)]
pub enum DigiResult {
    /// Packet should be retransmitted with this new TNC2 string.
    Digipeat(String),
    /// Packet should not be digipeated.
    Drop(DropReason),
}

/// Reason a packet was not digipeated.
#[derive(Debug, PartialEq, Eq)]
pub enum DropReason {
    /// Packet is not an APRS packet.
    NotAprs,
    /// No VIA fields to process.
    NoViasToProcess,
    /// All VIA fields already have H-bit set.
    HopsExhausted,
    /// Requested or done hops exceed configured maximum.
    ExceedsMaxHops,
    /// Our callsign already appears in the path with H-bit set.
    AlreadyDigipeated,
    /// Rate limited.
    RateLimited,
    /// Duplicate packet.
    Duplicate,
}

/// A parsed VIA address from a TNC2 path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ViaAddr {
    /// The full text without H-bit marker, e.g. "WIDE2-2" or "VIA1".
    raw: String,
    /// The callsign portion (before the dash), e.g. "WIDE2".
    callsign: String,
    /// The SSID portion (after the dash), 0 if absent.
    ssid: u8,
    /// Whether the H-bit (digipeated marker '*') is set.
    h_bit: bool,
}

/// Parsed TNC2 address header.
#[derive(Debug)]
struct ParsedAddresses {
    source: String,
    dest: String,
    vias: Vec<ViaAddr>,
}

/// Parse a VIA field string (e.g. "WIDE2-2", "VIA1*") into a ViaAddr.
fn parse_via(field: &str) -> ViaAddr {
    let h_bit = field.ends_with('*');
    let raw = if h_bit {
        field.trim_end_matches('*')
    } else {
        field
    };

    let (callsign, ssid) = if let Some(dash_pos) = raw.rfind('-') {
        let call = &raw[..dash_pos];
        let ssid_str = &raw[dash_pos + 1..];
        let ssid = ssid_str.parse::<u8>().unwrap_or(0);
        (call.to_string(), ssid)
    } else {
        (raw.to_string(), 0)
    };

    ViaAddr {
        raw: raw.to_string(),
        callsign,
        ssid,
        h_bit,
    }
}

/// Parse the TNC2 address header into source, destination, and VIA list.
fn parse_addresses(addresses: &str) -> Option<ParsedAddresses> {
    let (source, rest) = addresses.split_once('>')?;
    let mut parts = rest.split(',');
    let dest = parts.next()?.to_string();
    let vias: Vec<ViaAddr> = parts.map(parse_via).collect();

    Some(ParsedAddresses {
        source: source.to_string(),
        dest,
        vias,
    })
}

/// Reassemble a TNC2 string from parsed components.
fn reassemble_tnc2(source: &str, dest: &str, vias: &[String], payload: &str) -> String {
    let mut result = format!("{source}>{dest}");
    for via in vias {
        result.push(',');
        result.push_str(via);
    }
    result.push(':');
    result.push_str(payload);
    result
}

/// Check if a callsign matches any of the given keywords.
/// A callsign like "WIDE2" matches keyword "WIDE" because it starts with "WIDE".
fn matches_keyword(callsign: &str, keywords: &[String]) -> bool {
    let upper = callsign.to_uppercase();
    keywords
        .iter()
        .any(|kw| upper.starts_with(&kw.to_uppercase()))
}

/// Process a packet for digipeating.
///
/// Examines the VIA path and applies New-N paradigm digipeating rules.
/// Returns `DigiResult::Digipeat` with the modified TNC2 string if the
/// packet should be retransmitted, or `DigiResult::Drop` with a reason.
pub fn process_digipeat(packet: &Packet, config: &DigiConfig) -> DigiResult {
    if !packet.is_aprs {
        return DigiResult::Drop(DropReason::NotAprs);
    }

    let addresses = packet.addresses();
    let parsed = match parse_addresses(addresses) {
        Some(p) => p,
        None => return DigiResult::Drop(DropReason::NotAprs),
    };

    if parsed.vias.is_empty() {
        return DigiResult::Drop(DropReason::NoViasToProcess);
    }

    // Check if our callsign already appears in the path with H-bit set
    let our_call_upper = config.transmitter_call.to_uppercase();
    for via in &parsed.vias {
        if via.h_bit && via.raw.to_uppercase() == our_call_upper {
            return DigiResult::Drop(DropReason::AlreadyDigipeated);
        }
    }

    // Count already-digipeated hops
    let hops_done = parsed.vias.iter().filter(|v| v.h_bit).count() as u8;
    if hops_done > config.max_hops_done {
        return DigiResult::Drop(DropReason::ExceedsMaxHops);
    }

    // Find the first non-digipeated VIA
    let target_idx = match parsed.vias.iter().position(|v| !v.h_bit) {
        Some(idx) => idx,
        None => return DigiResult::Drop(DropReason::HopsExhausted),
    };

    let target = &parsed.vias[target_idx];

    // Build the output VIA list
    let mut out_vias: Vec<String> = Vec::with_capacity(parsed.vias.len() + 1);

    // Copy all digipeated VIAs before the target (they keep their H-bit markers)
    for via in &parsed.vias[..target_idx] {
        let s = if via.h_bit {
            format!("{}*", via.raw)
        } else {
            via.raw.clone()
        };
        out_vias.push(s);
    }

    // Check for direct callsign match
    if target.raw.to_uppercase() == our_call_upper {
        out_vias.push(format!("{}*", config.transmitter_call));
        // Copy remaining VIAs after the target
        for via in &parsed.vias[target_idx + 1..] {
            let s = if via.h_bit {
                format!("{}*", via.raw)
            } else {
                via.raw.clone()
            };
            out_vias.push(s);
        }
        return DigiResult::Digipeat(reassemble_tnc2(
            &parsed.source,
            &parsed.dest,
            &out_vias,
            packet.payload(),
        ));
    }

    // Check keyword match (New-N paradigm)
    if matches_keyword(&target.callsign, &config.wide_keywords) {
        let n = target.ssid;

        if n == 0 {
            return DigiResult::Drop(DropReason::HopsExhausted);
        }

        // Check if requested hops exceed maximum
        // The original N is encoded in the callsign digit (e.g., WIDE2 means 2 was requested)
        // Parse the digit from the callsign suffix for max_hops_req check
        let requested_n = extract_requested_n(&target.callsign);
        if requested_n > config.max_hops_req {
            return DigiResult::Drop(DropReason::ExceedsMaxHops);
        }

        if n == 1 {
            // Last hop: replace with our callsign
            out_vias.push(format!("{}*", config.transmitter_call));
        } else {
            // More hops remain: insert our callsign, decrement N
            out_vias.push(format!("{}*", config.transmitter_call));
            out_vias.push(format!("{}-{}", target.callsign, n - 1));
        }

        // Copy remaining VIAs after the target
        for via in &parsed.vias[target_idx + 1..] {
            let s = if via.h_bit {
                format!("{}*", via.raw)
            } else {
                via.raw.clone()
            };
            out_vias.push(s);
        }

        return DigiResult::Digipeat(reassemble_tnc2(
            &parsed.source,
            &parsed.dest,
            &out_vias,
            packet.payload(),
        ));
    }

    // No match found
    DigiResult::Drop(DropReason::NoViasToProcess)
}

/// Extract the requested N from a callsign like "WIDE2" → 2, "WIDE1" → 1.
/// Returns the trailing digits as a number, or 0 if none found.
fn extract_requested_n(callsign: &str) -> u8 {
    let digits: String = callsign
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let digits: String = digits.chars().rev().collect();
    digits.parse::<u8>().unwrap_or(0)
}

/// Token bucket rate limiter for controlling transmit rate.
pub struct RateLimiter {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// `max_per_minute` controls the sustained rate. `burst` sets the
    /// maximum number of tokens that can accumulate.
    pub fn new(max_per_minute: f64, burst: f64) -> Self {
        Self {
            tokens: burst,
            max_tokens: burst,
            refill_rate: max_per_minute / 60.0,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume one token. Returns `true` if a token was available.
    pub fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> DigiConfig {
        DigiConfig {
            transmitter_call: "MYCALL".to_string(),
            max_hops_req: 4,
            max_hops_done: 4,
            wide_keywords: vec!["WIDE".to_string(), "RELAY".to_string()],
        }
    }

    // --- Path parsing tests ---

    #[test]
    fn test_parse_via_addresses() {
        let parsed = parse_addresses("SRC>DST,VIA1*,WIDE2-2").unwrap();
        assert_eq!(parsed.source, "SRC");
        assert_eq!(parsed.dest, "DST");
        assert_eq!(parsed.vias.len(), 2);

        assert_eq!(parsed.vias[0].callsign, "VIA1");
        assert_eq!(parsed.vias[0].ssid, 0);
        assert!(parsed.vias[0].h_bit);

        assert_eq!(parsed.vias[1].callsign, "WIDE2");
        assert_eq!(parsed.vias[1].ssid, 2);
        assert!(!parsed.vias[1].h_bit);
    }

    #[test]
    fn test_parse_via_no_vias() {
        let parsed = parse_addresses("SRC>DST").unwrap();
        assert_eq!(parsed.source, "SRC");
        assert_eq!(parsed.dest, "DST");
        assert!(parsed.vias.is_empty());
    }

    #[test]
    fn test_parse_via_with_ssid() {
        let via = parse_via("N0CALL-15*");
        assert_eq!(via.callsign, "N0CALL");
        assert_eq!(via.ssid, 15);
        assert!(via.h_bit);
        assert_eq!(via.raw, "N0CALL-15");
    }

    // --- Digipeater processing tests ---

    #[test]
    fn test_wide1_1_direct_heard() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,WIDE1-1:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(
            result,
            DigiResult::Digipeat("SRC>DST,MYCALL*:data".to_string())
        );
    }

    #[test]
    fn test_wide2_2_first_hop() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,WIDE2-2:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(
            result,
            DigiResult::Digipeat("SRC>DST,MYCALL*,WIDE2-1:data".to_string())
        );
    }

    #[test]
    fn test_wide2_1_last_hop() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,WIDE2-1:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(
            result,
            DigiResult::Digipeat("SRC>DST,MYCALL*:data".to_string())
        );
    }

    #[test]
    fn test_already_digipeated_via_skipped() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,VIA1*,WIDE1-1:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(
            result,
            DigiResult::Digipeat("SRC>DST,VIA1*,MYCALL*:data".to_string())
        );
    }

    #[test]
    fn test_no_via_to_process() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(result, DigiResult::Drop(DropReason::NoViasToProcess));
    }

    #[test]
    fn test_all_vias_digipeated() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,VIA1*:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(result, DigiResult::Drop(DropReason::HopsExhausted));
    }

    #[test]
    fn test_direct_callsign_match() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,MYCALL:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(
            result,
            DigiResult::Digipeat("SRC>DST,MYCALL*:data".to_string())
        );
    }

    #[test]
    fn test_exceeds_max_hops() {
        let config = DigiConfig {
            transmitter_call: "MYCALL".to_string(),
            max_hops_req: 4,
            max_hops_done: 4,
            wide_keywords: vec!["WIDE".to_string()],
        };
        let pkt = Packet::new("SRC>DST,WIDE7-7:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(result, DigiResult::Drop(DropReason::ExceedsMaxHops));
    }

    #[test]
    fn test_our_callsign_already_digipeated() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,MYCALL*,WIDE1-1:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(result, DigiResult::Drop(DropReason::AlreadyDigipeated));
    }

    #[test]
    fn test_not_aprs_packet() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,WIDE1-1:data", "port0", false);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(result, DigiResult::Drop(DropReason::NotAprs));
    }

    #[test]
    fn test_wide_n_zero_drops() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,WIDE2-0:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(result, DigiResult::Drop(DropReason::HopsExhausted));
    }

    #[test]
    fn test_relay_keyword_match() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,RELAY:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        // RELAY has no SSID, so ssid=0, treated as hops exhausted
        assert_eq!(result, DigiResult::Drop(DropReason::HopsExhausted));
    }

    #[test]
    fn test_multiple_vias_with_remaining() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,WIDE2-2,WIDE1-1:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(
            result,
            DigiResult::Digipeat("SRC>DST,MYCALL*,WIDE2-1,WIDE1-1:data".to_string())
        );
    }

    #[test]
    fn test_exceeds_max_hops_done() {
        let config = DigiConfig {
            transmitter_call: "MYCALL".to_string(),
            max_hops_req: 4,
            max_hops_done: 2,
            wide_keywords: vec!["WIDE".to_string()],
        };
        let pkt = Packet::new("SRC>DST,A*,B*,C*,WIDE1-1:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(result, DigiResult::Drop(DropReason::ExceedsMaxHops));
    }

    #[test]
    fn test_unmatched_via_drops() {
        let config = default_config();
        let pkt = Packet::new("SRC>DST,UNKNOWN-1:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(result, DigiResult::Drop(DropReason::NoViasToProcess));
    }

    #[test]
    fn test_extract_requested_n() {
        assert_eq!(extract_requested_n("WIDE2"), 2);
        assert_eq!(extract_requested_n("WIDE1"), 1);
        assert_eq!(extract_requested_n("RELAY"), 0);
        assert_eq!(extract_requested_n("WIDE12"), 12);
    }

    #[test]
    fn test_case_insensitive_callsign_match() {
        let config = DigiConfig {
            transmitter_call: "MYCALL".to_string(),
            ..default_config()
        };
        let pkt = Packet::new("SRC>DST,mycall:data", "port0", true);
        let result = process_digipeat(&pkt, &config);
        assert_eq!(
            result,
            DigiResult::Digipeat("SRC>DST,MYCALL*:data".to_string())
        );
    }

    // --- Rate limiter tests ---

    #[test]
    fn test_rate_limiter_allows() {
        let mut limiter = RateLimiter::new(60.0, 5.0);
        assert!(limiter.try_consume());
    }

    #[test]
    fn test_rate_limiter_blocks() {
        let mut limiter = RateLimiter::new(60.0, 2.0);
        assert!(limiter.try_consume());
        assert!(limiter.try_consume());
        assert!(!limiter.try_consume());
    }

    #[test]
    fn test_rate_limiter_refills() {
        let mut limiter = RateLimiter::new(6000.0, 1.0); // 100/sec
        assert!(limiter.try_consume());
        assert!(!limiter.try_consume());

        // Wait for refill (100/sec means 1 token in 10ms)
        std::thread::sleep(std::time::Duration::from_millis(20));

        assert!(limiter.try_consume());
    }
}
