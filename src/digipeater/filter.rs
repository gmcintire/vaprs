use regex::Regex;

use crate::packet::Packet;

/// A single filter expression for digipeater source filtering.
///
/// Follows APRS-IS style filter syntax (b/, p/, -b/, -p/) and
/// aprx regex filters (regex-source/, regex-destination/, regex-via/, regex-data/).
pub enum Filter {
    /// b/CALL1/CALL2 or -b/CALL1/CALL2
    /// Pass (or reject) packets from listed callsigns. Supports * wildcard.
    Budlist {
        callsigns: Vec<String>,
        negate: bool,
    },
    /// p/PREFIX1/PREFIX2 or -p/PREFIX1/PREFIX2
    /// Pass (or reject) packets with matching destination prefix.
    Prefix { prefixes: Vec<String>, negate: bool },
    /// regex-source/PATTERN - reject packets where source callsign matches
    RegexSource(Regex),
    /// regex-destination/PATTERN - reject packets where destination callsign matches
    RegexDestination(Regex),
    /// regex-via/PATTERN - reject packets where any VIA callsign matches
    RegexVia(Regex),
    /// regex-data/PATTERN - reject packets where payload data matches
    RegexData(Regex),
}

/// A filter chain - list of filters applied in order.
///
/// Evaluation logic (following aprx):
/// - If no positive filters exist: all packets pass (only negative/regex filters apply)
/// - If positive filters exist: packet must match at least one positive filter
/// - Negative filters always reject: if any negative filter matches, packet is rejected
/// - Regex filters are reject filters: if any regex matches, packet is rejected
///
/// Order of evaluation:
/// 1. Check negative filters first - if any match, reject
/// 2. Check regex filters - if any match, reject
/// 3. If positive filters exist, check them - must match at least one
/// 4. If no positive filters, pass
pub struct FilterChain {
    filters: Vec<Filter>,
}

impl Default for FilterChain {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterChain {
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
        }
    }

    /// Parse a filter string and add the resulting filter to the chain.
    pub fn add_filter(&mut self, filter_str: &str) -> Result<(), String> {
        let filter = parse_filter(filter_str)?;
        self.filters.push(filter);
        Ok(())
    }

    /// Check if a packet passes all filters.
    ///
    /// An empty filter chain passes everything.
    pub fn check(&self, packet: &Packet) -> bool {
        if self.filters.is_empty() {
            return true;
        }

        // Step 1: Check negative filters - if any match, reject
        for filter in &self.filters {
            match filter {
                Filter::Budlist {
                    callsigns,
                    negate: true,
                } => {
                    if matches_budlist(packet.source_call(), callsigns) {
                        return false;
                    }
                }
                Filter::Prefix {
                    prefixes,
                    negate: true,
                } => {
                    if matches_prefix(packet.dest_call(), prefixes) {
                        return false;
                    }
                }
                _ => {}
            }
        }

        // Step 2: Check regex filters - if any match, reject
        for filter in &self.filters {
            match filter {
                Filter::RegexSource(re) => {
                    if re.is_match(packet.source_call()) {
                        return false;
                    }
                }
                Filter::RegexDestination(re) => {
                    if re.is_match(packet.dest_call()) {
                        return false;
                    }
                }
                Filter::RegexVia(re) => {
                    if matches_regex_via(packet, re) {
                        return false;
                    }
                }
                Filter::RegexData(re) => {
                    if re.is_match(packet.payload()) {
                        return false;
                    }
                }
                _ => {}
            }
        }

        // Step 3: Check positive filters
        let has_positive = self.filters.iter().any(|f| {
            matches!(
                f,
                Filter::Budlist { negate: false, .. } | Filter::Prefix { negate: false, .. }
            )
        });

        if !has_positive {
            // No positive filters -- pass (only negative/regex filters applied above)
            return true;
        }

        // Must match at least one positive filter
        for filter in &self.filters {
            match filter {
                Filter::Budlist {
                    callsigns,
                    negate: false,
                } => {
                    if matches_budlist(packet.source_call(), callsigns) {
                        return true;
                    }
                }
                Filter::Prefix {
                    prefixes,
                    negate: false,
                } => {
                    if matches_prefix(packet.dest_call(), prefixes) {
                        return true;
                    }
                }
                _ => {}
            }
        }

        false
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    pub fn len(&self) -> usize {
        self.filters.len()
    }
}

/// Parse a filter specification string into a `Filter`.
pub fn parse_filter(spec: &str) -> Result<Filter, String> {
    // Check for negative prefix first
    if let Some(rest) = spec.strip_prefix("-b/") {
        let callsigns = parse_slash_list(rest)?;
        return Ok(Filter::Budlist {
            callsigns,
            negate: true,
        });
    }

    if let Some(rest) = spec.strip_prefix("-p/") {
        let prefixes = parse_slash_list(rest)?;
        return Ok(Filter::Prefix {
            prefixes,
            negate: true,
        });
    }

    if let Some(rest) = spec.strip_prefix("b/") {
        let callsigns = parse_slash_list(rest)?;
        return Ok(Filter::Budlist {
            callsigns,
            negate: false,
        });
    }

    if let Some(rest) = spec.strip_prefix("p/") {
        let prefixes = parse_slash_list(rest)?;
        return Ok(Filter::Prefix {
            prefixes,
            negate: false,
        });
    }

    if let Some(rest) = spec.strip_prefix("regex-source/") {
        let re = Regex::new(rest).map_err(|e| format!("invalid regex: {e}"))?;
        return Ok(Filter::RegexSource(re));
    }

    if let Some(rest) = spec.strip_prefix("regex-destination/") {
        let re = Regex::new(rest).map_err(|e| format!("invalid regex: {e}"))?;
        return Ok(Filter::RegexDestination(re));
    }

    if let Some(rest) = spec.strip_prefix("regex-via/") {
        let re = Regex::new(rest).map_err(|e| format!("invalid regex: {e}"))?;
        return Ok(Filter::RegexVia(re));
    }

    if let Some(rest) = spec.strip_prefix("regex-data/") {
        let re = Regex::new(rest).map_err(|e| format!("invalid regex: {e}"))?;
        return Ok(Filter::RegexData(re));
    }

    Err(format!("unknown filter type: {spec}"))
}

/// Parse a slash-separated list of values, e.g. "CALL1/CALL2/CALL3".
/// All values are uppercased for consistent matching.
fn parse_slash_list(s: &str) -> Result<Vec<String>, String> {
    let items: Vec<String> = s
        .split('/')
        .filter(|item| !item.is_empty())
        .map(|item| item.to_uppercase())
        .collect();

    if items.is_empty() {
        return Err("empty filter list".to_string());
    }

    Ok(items)
}

/// Check if a callsign matches any pattern in the budlist.
/// Patterns ending with '*' match as prefix; exact patterns match exactly.
/// Matching is case-insensitive.
fn matches_budlist(source_call: &str, patterns: &[String]) -> bool {
    let upper = source_call.to_uppercase();
    patterns.iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('*') {
            upper.starts_with(prefix)
        } else {
            upper == *pattern
        }
    })
}

/// Check if a destination callsign matches any prefix in the list.
/// Matching is case-insensitive and always uses prefix (starts_with) matching.
fn matches_prefix(dest_call: &str, prefixes: &[String]) -> bool {
    let upper = dest_call.to_uppercase();
    prefixes
        .iter()
        .any(|prefix| upper.starts_with(prefix.as_str()))
}

/// Extract VIA callsigns from a packet's addresses and check if any match the regex.
fn matches_regex_via(packet: &Packet, re: &Regex) -> bool {
    let addresses = packet.addresses();
    // addresses format: "SRC>DST,VIA1,VIA2" or "SRC>DST"
    if let Some((_src, rest)) = addresses.split_once('>') {
        // rest is "DST,VIA1,VIA2" or "DST"
        let mut parts = rest.split(',');
        // Skip destination
        parts.next();
        // Check each VIA
        for via in parts {
            // Strip H-bit marker for matching
            let via_clean = via.trim_end_matches('*');
            if re.is_match(via_clean) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::Packet;

    fn make_packet(tnc2: &str) -> Packet {
        Packet::new(tnc2, "port0", true)
    }

    // --- FilterChain tests ---

    #[test]
    fn test_empty_chain_passes_all() {
        let chain = FilterChain::new();
        let pkt = make_packet("OH2MQK>APRS,WIDE1-1:test data");
        assert!(chain.check(&pkt));
    }

    #[test]
    fn test_budlist_exact_match() {
        let mut chain = FilterChain::new();
        chain.add_filter("b/OH2MQK").unwrap();

        let pass = make_packet("OH2MQK>APRS:test");
        let reject = make_packet("W3ADO>APRS:test");
        assert!(chain.check(&pass));
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_budlist_wildcard() {
        let mut chain = FilterChain::new();
        chain.add_filter("b/OH2MQK*").unwrap();

        let pass1 = make_packet("OH2MQK>APRS:test");
        let pass2 = make_packet("OH2MQK-1>APRS:test");
        let pass3 = make_packet("OH2MQK-15>APRS:test");
        let reject = make_packet("W3ADO>APRS:test");

        assert!(chain.check(&pass1));
        assert!(chain.check(&pass2));
        assert!(chain.check(&pass3));
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_negative_budlist() {
        let mut chain = FilterChain::new();
        chain.add_filter("-b/NOCALL").unwrap();

        let pass = make_packet("OH2MQK>APRS:test");
        let reject = make_packet("NOCALL>APRS:test");

        assert!(chain.check(&pass));
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_prefix_filter() {
        let mut chain = FilterChain::new();
        chain.add_filter("p/APRS").unwrap();

        let pass = make_packet("SRC>APRS,WIDE1-1:test");
        let pass2 = make_packet("SRC>APRSFI:test");
        let reject = make_packet("SRC>CQ:test");

        assert!(chain.check(&pass));
        assert!(chain.check(&pass2));
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_negative_prefix() {
        let mut chain = FilterChain::new();
        chain.add_filter("-p/CQ").unwrap();

        let pass = make_packet("SRC>APRS:test");
        let reject = make_packet("SRC>CQ:test");
        let reject2 = make_packet("SRC>CQCQCQ:test");

        assert!(chain.check(&pass));
        assert!(!chain.check(&reject));
        assert!(!chain.check(&reject2));
    }

    #[test]
    fn test_regex_source() {
        let mut chain = FilterChain::new();
        chain.add_filter("regex-source/^NOCALL").unwrap();

        // Regex filters reject when matched
        let pass = make_packet("OH2MQK>APRS:test");
        let reject = make_packet("NOCALL>APRS:test");
        let reject2 = make_packet("NOCALL-5>APRS:test");

        assert!(chain.check(&pass));
        assert!(!chain.check(&reject));
        assert!(!chain.check(&reject2));
    }

    #[test]
    fn test_regex_data() {
        let mut chain = FilterChain::new();
        chain.add_filter("regex-data/^\\}").unwrap();

        // Reject packets whose payload starts with '}'
        let pass = make_packet("SRC>DST:!6029.50N/02505.43E>");
        let reject = make_packet("SRC>DST:}INNER>DST:payload");

        assert!(chain.check(&pass));
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_regex_rejects_matching() {
        let mut chain = FilterChain::new();
        chain.add_filter("regex-destination/^TCPIP").unwrap();

        let pass = make_packet("SRC>APRS:test");
        let reject = make_packet("SRC>TCPIP:test");

        assert!(chain.check(&pass));
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_combined_positive_and_negative() {
        let mut chain = FilterChain::new();
        // Positive: only pass packets from OH2*
        chain.add_filter("b/OH2*").unwrap();
        // Negative: but reject OH2BAD
        chain.add_filter("-b/OH2BAD").unwrap();

        let pass = make_packet("OH2MQK>APRS:test");
        let reject_not_positive = make_packet("W3ADO>APRS:test");
        let reject_negative = make_packet("OH2BAD>APRS:test");

        assert!(chain.check(&pass));
        assert!(!chain.check(&reject_not_positive));
        assert!(!chain.check(&reject_negative));
    }

    // --- parse_filter tests ---

    #[test]
    fn test_parse_budlist() {
        let filter = parse_filter("b/CALL1/CALL2").unwrap();
        match filter {
            Filter::Budlist { callsigns, negate } => {
                assert!(!negate);
                assert_eq!(callsigns, vec!["CALL1", "CALL2"]);
            }
            _ => panic!("expected Budlist"),
        }
    }

    #[test]
    fn test_parse_negative_budlist() {
        let filter = parse_filter("-b/CALL1").unwrap();
        match filter {
            Filter::Budlist { callsigns, negate } => {
                assert!(negate);
                assert_eq!(callsigns, vec!["CALL1"]);
            }
            _ => panic!("expected Budlist"),
        }
    }

    #[test]
    fn test_parse_prefix() {
        let filter = parse_filter("p/APRS/CQ").unwrap();
        match filter {
            Filter::Prefix { prefixes, negate } => {
                assert!(!negate);
                assert_eq!(prefixes, vec!["APRS", "CQ"]);
            }
            _ => panic!("expected Prefix"),
        }
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_filter("x/invalid").is_err());
        assert!(parse_filter("").is_err());
        assert!(parse_filter("b/").is_err());
        assert!(parse_filter("regex-source/[invalid").is_err());
    }

    #[test]
    fn test_multiple_budlist_entries() {
        let mut chain = FilterChain::new();
        chain.add_filter("b/OH2MQK*/W3ADO*").unwrap();

        let pass1 = make_packet("OH2MQK>APRS:test");
        let pass2 = make_packet("OH2MQK-1>APRS:test");
        let pass3 = make_packet("W3ADO>APRS:test");
        let pass4 = make_packet("W3ADO-15>APRS:test");
        let reject = make_packet("N0CALL>APRS:test");

        assert!(chain.check(&pass1));
        assert!(chain.check(&pass2));
        assert!(chain.check(&pass3));
        assert!(chain.check(&pass4));
        assert!(!chain.check(&reject));
    }

    // --- Additional edge case tests ---

    #[test]
    fn test_case_insensitive_budlist() {
        let mut chain = FilterChain::new();
        chain.add_filter("b/oh2mqk").unwrap();

        // Filter stored as uppercase, source call should match case-insensitively
        let pass = make_packet("OH2MQK>APRS:test");
        assert!(chain.check(&pass));
    }

    #[test]
    fn test_regex_via_filter() {
        let mut chain = FilterChain::new();
        chain.add_filter("regex-via/^TCPIP").unwrap();

        let pass = make_packet("SRC>DST,WIDE1-1:test");
        let reject = make_packet("SRC>DST,TCPIP*:test");

        assert!(chain.check(&pass));
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_is_empty_and_len() {
        let mut chain = FilterChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);

        chain.add_filter("b/TEST").unwrap();
        assert!(!chain.is_empty());
        assert_eq!(chain.len(), 1);

        chain.add_filter("-b/BAD").unwrap();
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn test_only_negative_filters_pass_unmatched() {
        // With only negative filters and no positive filters,
        // unmatched packets should pass
        let mut chain = FilterChain::new();
        chain.add_filter("-b/SPAM").unwrap();
        chain.add_filter("-p/TCPIP").unwrap();

        let pass = make_packet("GOOD>APRS:test");
        assert!(chain.check(&pass));

        let reject1 = make_packet("SPAM>APRS:test");
        assert!(!chain.check(&reject1));

        let reject2 = make_packet("SRC>TCPIP:test");
        assert!(!chain.check(&reject2));
    }

    #[test]
    fn test_multiple_positive_filters_or_logic() {
        // Multiple positive filters: packet must match at least one (OR)
        let mut chain = FilterChain::new();
        chain.add_filter("b/OH2MQK").unwrap();
        chain.add_filter("p/APRS").unwrap();

        // Matches budlist but not prefix
        let pass1 = make_packet("OH2MQK>CQ:test");
        assert!(chain.check(&pass1));

        // Matches prefix but not budlist
        let pass2 = make_packet("W3ADO>APRS:test");
        assert!(chain.check(&pass2));

        // Matches neither
        let reject = make_packet("W3ADO>CQ:test");
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_negative_overrides_positive() {
        // Negative filter takes precedence even when positive matches
        let mut chain = FilterChain::new();
        chain.add_filter("b/OH2*").unwrap();
        chain.add_filter("-b/OH2BAD").unwrap();

        // Matches positive but also matches negative -> reject
        let reject = make_packet("OH2BAD>APRS:test");
        assert!(!chain.check(&reject));
    }

    #[test]
    fn test_default_trait() {
        let chain = FilterChain::default();
        assert!(chain.is_empty());
    }
}
