use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::packet::Packet;

/// Default duplicate detection window
pub const DEFAULT_STORETIME: Duration = Duration::from_secs(30);

/// Result of checking a packet for duplicates
#[derive(Debug, PartialEq, Eq)]
pub enum DupeResult {
    /// First time seeing this packet
    New,
    /// This packet is a duplicate (seen_count includes this observation)
    Duplicate { seen_count: u32 },
}

/// Key used to identify a unique packet for duplicate detection.
/// VIA addresses are stripped -- only source and destination matter.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct DupeKey {
    /// "SOURCE>DEST" (no VIAs)
    addr: String,
    /// Payload with trailing spaces stripped
    payload: String,
}

/// Entry tracking how many times a packet has been observed.
struct DupeEntry {
    seen_count: u32,
    expires_at: Instant,
}

/// Hash-based duplicate packet detector with configurable expiration.
///
/// Packets are keyed by source>destination and payload (trailing spaces stripped).
/// VIA addresses are ignored so the same original packet relayed through different
/// paths is still detected as a duplicate.
pub struct DupeChecker {
    storetime: Duration,
    entries: HashMap<DupeKey, DupeEntry>,
}

impl DupeKey {
    fn from_packet(packet: &Packet) -> Self {
        let addr = format!("{}>{}", packet.source_call(), packet.dest_call());
        let payload = packet.payload().trim_end().to_string();
        Self { addr, payload }
    }
}

impl DupeChecker {
    pub fn new(storetime: Duration) -> Self {
        Self {
            storetime,
            entries: HashMap::new(),
        }
    }

    /// Check if a packet is a duplicate. If not seen before, adds it.
    /// Returns `DupeResult::New` for first observation, `Duplicate` for repeats.
    pub fn check(&mut self, packet: &Packet) -> DupeResult {
        let key = DupeKey::from_packet(packet);
        let now = Instant::now();

        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.expires_at > now {
                entry.seen_count += 1;
                return DupeResult::Duplicate {
                    seen_count: entry.seen_count,
                };
            }
            // Expired -- treat as new, reset the entry
            entry.seen_count = 1;
            entry.expires_at = now + self.storetime;
            return DupeResult::New;
        }

        self.entries.insert(
            key,
            DupeEntry {
                seen_count: 1,
                expires_at: now + self.storetime,
            },
        );
        DupeResult::New
    }

    /// Remove expired entries. Call periodically (e.g., every 60 seconds).
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, entry| entry.expires_at > now);
    }

    /// Number of entries currently stored
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_checker() -> DupeChecker {
        DupeChecker::new(DEFAULT_STORETIME)
    }

    #[test]
    fn test_new_packet_not_duplicate() {
        let mut checker = make_checker();
        let pkt = Packet::new("SRC>DST:hello", "port0", true);
        assert_eq!(checker.check(&pkt), DupeResult::New);
    }

    #[test]
    fn test_same_packet_is_duplicate() {
        let mut checker = make_checker();
        let pkt = Packet::new("SRC>DST:hello", "port0", true);
        assert_eq!(checker.check(&pkt), DupeResult::New);
        assert_eq!(checker.check(&pkt), DupeResult::Duplicate { seen_count: 2 });
    }

    #[test]
    fn test_different_packets_not_duplicate() {
        let mut checker = make_checker();
        let pkt1 = Packet::new("SRC>DST:hello", "port0", true);
        let pkt2 = Packet::new("SRC>DST:world", "port0", true);
        let pkt3 = Packet::new("OTHER>DST:hello", "port0", true);

        assert_eq!(checker.check(&pkt1), DupeResult::New);
        assert_eq!(checker.check(&pkt2), DupeResult::New);
        assert_eq!(checker.check(&pkt3), DupeResult::New);
    }

    #[test]
    fn test_via_addresses_ignored() {
        let mut checker = make_checker();
        let pkt1 = Packet::new("SRC>DST,VIA1,VIA2:hello", "port0", true);
        let pkt2 = Packet::new("SRC>DST,WIDE1-1:hello", "port1", true);
        let pkt3 = Packet::new("SRC>DST:hello", "port2", true);

        assert_eq!(checker.check(&pkt1), DupeResult::New);
        assert_eq!(
            checker.check(&pkt2),
            DupeResult::Duplicate { seen_count: 2 }
        );
        assert_eq!(
            checker.check(&pkt3),
            DupeResult::Duplicate { seen_count: 3 }
        );
    }

    #[test]
    fn test_trailing_spaces_stripped() {
        let mut checker = make_checker();
        let pkt1 = Packet::new("SRC>DST:payload", "port0", true);
        let pkt2 = Packet::new("SRC>DST:payload   ", "port0", true);

        assert_eq!(checker.check(&pkt1), DupeResult::New);
        assert_eq!(
            checker.check(&pkt2),
            DupeResult::Duplicate { seen_count: 2 }
        );
    }

    #[test]
    fn test_expiration() {
        let storetime = Duration::from_millis(50);
        let mut checker = DupeChecker::new(storetime);
        let pkt = Packet::new("SRC>DST:hello", "port0", true);

        assert_eq!(checker.check(&pkt), DupeResult::New);

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(60));

        // After expiration, same packet should be New again
        assert_eq!(checker.check(&pkt), DupeResult::New);
    }

    #[test]
    fn test_seen_count_increments() {
        let mut checker = make_checker();
        let pkt = Packet::new("SRC>DST:hello", "port0", true);

        assert_eq!(checker.check(&pkt), DupeResult::New);
        assert_eq!(checker.check(&pkt), DupeResult::Duplicate { seen_count: 2 });
        assert_eq!(checker.check(&pkt), DupeResult::Duplicate { seen_count: 3 });
    }

    #[test]
    fn test_cleanup_removes_expired() {
        let storetime = Duration::from_millis(50);
        let mut checker = DupeChecker::new(storetime);

        let pkt = Packet::new("SRC>DST:hello", "port0", true);
        checker.check(&pkt);
        assert_eq!(checker.len(), 1);

        // Wait for expiration, then cleanup
        std::thread::sleep(Duration::from_millis(60));
        checker.cleanup();

        assert_eq!(checker.len(), 0);
        assert!(checker.is_empty());
    }

    #[test]
    fn test_cleanup_keeps_fresh() {
        let mut checker = make_checker();

        let pkt1 = Packet::new("SRC>DST:hello", "port0", true);
        let pkt2 = Packet::new("SRC>DST:world", "port0", true);
        checker.check(&pkt1);
        checker.check(&pkt2);
        assert_eq!(checker.len(), 2);

        // Cleanup should not remove fresh entries
        checker.cleanup();
        assert_eq!(checker.len(), 2);
    }

    #[test]
    fn test_len_and_is_empty() {
        let mut checker = make_checker();
        assert!(checker.is_empty());
        assert_eq!(checker.len(), 0);

        let pkt1 = Packet::new("SRC>DST:hello", "port0", true);
        checker.check(&pkt1);
        assert!(!checker.is_empty());
        assert_eq!(checker.len(), 1);

        let pkt2 = Packet::new("SRC>DST:world", "port0", true);
        checker.check(&pkt2);
        assert_eq!(checker.len(), 2);

        // Duplicate should not increase entry count
        checker.check(&pkt1);
        assert_eq!(checker.len(), 2);
    }
}
