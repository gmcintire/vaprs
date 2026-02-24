// History database: tracks recently heard stations with coordinates and timestamps.
// Used by the Tx-iGate to decide if a station heard on APRS-IS should be gated to RF.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Default time-to-live for history entries (1 hour, matches aprx)
pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);

/// A record of a station heard on RF
#[derive(Debug, Clone)]
pub struct StationRecord {
    pub callsign: String,
    pub last_heard: Instant,
    /// Source interface where the station was heard
    pub source_interface: String,
    /// Optional position (lat, lon in degrees) if the packet contained position data
    pub position: Option<(f64, f64)>,
    /// Number of times this station has been heard
    pub heard_count: u32,
}

/// History database tracking recently heard stations
pub struct HistoryDb {
    entries: HashMap<String, StationRecord>,
    ttl: Duration,
}

impl HistoryDb {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
        }
    }

    /// Record that a station was heard on RF.
    /// Updates or creates the entry, resetting the last_heard timestamp.
    pub fn heard(&mut self, callsign: &str, source_interface: &str, position: Option<(f64, f64)>) {
        let key = callsign.to_uppercase();
        let now = Instant::now();

        self.entries
            .entry(key.clone())
            .and_modify(|record| {
                record.last_heard = now;
                record.source_interface = source_interface.to_string();
                if position.is_some() {
                    record.position = position;
                }
                record.heard_count += 1;
            })
            .or_insert_with(|| StationRecord {
                callsign: key,
                last_heard: now,
                source_interface: source_interface.to_string(),
                position,
                heard_count: 1,
            });
    }

    /// Check if a station was heard on RF within the TTL window.
    pub fn was_heard(&self, callsign: &str) -> bool {
        let key = callsign.to_uppercase();
        self.entries
            .get(&key)
            .is_some_and(|record| record.last_heard.elapsed() < self.ttl)
    }

    /// Get the full record for a station, if it exists and hasn't expired.
    pub fn get(&self, callsign: &str) -> Option<&StationRecord> {
        let key = callsign.to_uppercase();
        self.entries
            .get(&key)
            .filter(|record| record.last_heard.elapsed() < self.ttl)
    }

    /// Remove expired entries. Call periodically.
    pub fn cleanup(&mut self) {
        let ttl = self.ttl;
        self.entries
            .retain(|_, record| record.last_heard.elapsed() < ttl);
    }

    /// Number of active (non-expired) entries.
    pub fn len(&self) -> usize {
        let ttl = self.ttl;
        self.entries
            .values()
            .filter(|record| record.last_heard.elapsed() < ttl)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heard_creates_entry() {
        let mut db = HistoryDb::new(DEFAULT_TTL);
        db.heard("OH2MQK", "port0", None);
        assert!(db.was_heard("OH2MQK"));
    }

    #[test]
    fn test_not_heard_returns_false() {
        let db = HistoryDb::new(DEFAULT_TTL);
        assert!(!db.was_heard("UNKNOWN"));
    }

    #[test]
    fn test_heard_updates_timestamp() {
        let mut db = HistoryDb::new(DEFAULT_TTL);
        db.heard("OH2MQK", "port0", None);
        let first = db.get("OH2MQK").unwrap().last_heard;

        // Hear again - timestamp should be >= first
        db.heard("OH2MQK", "port0", None);
        let second = db.get("OH2MQK").unwrap().last_heard;
        assert!(second >= first);
    }

    #[test]
    fn test_heard_increments_count() {
        let mut db = HistoryDb::new(DEFAULT_TTL);
        db.heard("OH2MQK", "port0", None);
        assert_eq!(db.get("OH2MQK").unwrap().heard_count, 1);

        db.heard("OH2MQK", "port0", None);
        assert_eq!(db.get("OH2MQK").unwrap().heard_count, 2);

        db.heard("OH2MQK", "port0", None);
        assert_eq!(db.get("OH2MQK").unwrap().heard_count, 3);
    }

    #[test]
    fn test_expiration() {
        // Use a very short TTL so the entry expires immediately
        let mut db = HistoryDb::new(Duration::from_nanos(1));
        db.heard("OH2MQK", "port0", None);

        // Spin until at least 1ns has elapsed (effectively instant)
        while db.entries.get("OH2MQK").unwrap().last_heard.elapsed() < Duration::from_nanos(1) {
            std::hint::spin_loop();
        }

        assert!(!db.was_heard("OH2MQK"));
    }

    #[test]
    fn test_cleanup_removes_expired() {
        let mut db = HistoryDb::new(Duration::from_nanos(1));
        db.heard("OH2MQK", "port0", None);
        db.heard("OH2RDG", "port1", None);

        // Wait for entries to expire
        while db.entries.get("OH2MQK").unwrap().last_heard.elapsed() < Duration::from_nanos(1) {
            std::hint::spin_loop();
        }

        db.cleanup();
        assert!(db.entries.is_empty());
    }

    #[test]
    fn test_cleanup_keeps_fresh() {
        let mut db = HistoryDb::new(DEFAULT_TTL);
        db.heard("OH2MQK", "port0", None);
        db.heard("OH2RDG", "port1", None);

        db.cleanup();
        assert_eq!(db.entries.len(), 2);
        assert!(db.was_heard("OH2MQK"));
        assert!(db.was_heard("OH2RDG"));
    }

    #[test]
    fn test_get_returns_record() {
        let mut db = HistoryDb::new(DEFAULT_TTL);
        db.heard("OH2MQK", "port0", Some((60.49, 25.09)));

        let record = db.get("OH2MQK").unwrap();
        assert_eq!(record.callsign, "OH2MQK");
        assert_eq!(record.source_interface, "port0");
        assert_eq!(record.position, Some((60.49, 25.09)));
        assert_eq!(record.heard_count, 1);
    }

    #[test]
    fn test_case_insensitive() {
        let mut db = HistoryDb::new(DEFAULT_TTL);
        db.heard("oh2mqk", "port0", None);
        assert!(db.was_heard("OH2MQK"));
        assert!(db.was_heard("oh2mqk"));
        assert!(db.was_heard("Oh2Mqk"));

        // Should update the same entry, not create a new one
        db.heard("OH2MQK", "port1", None);
        assert_eq!(db.get("oh2mqk").unwrap().heard_count, 2);
        assert_eq!(db.entries.len(), 1);
    }

    #[test]
    fn test_position_stored() {
        let mut db = HistoryDb::new(DEFAULT_TTL);

        // First hearing without position
        db.heard("OH2MQK", "port0", None);
        assert_eq!(db.get("OH2MQK").unwrap().position, None);

        // Second hearing with position - should update
        db.heard("OH2MQK", "port0", Some((60.49, 25.09)));
        assert_eq!(db.get("OH2MQK").unwrap().position, Some((60.49, 25.09)));

        // Third hearing without position - should keep previous position
        db.heard("OH2MQK", "port0", None);
        assert_eq!(db.get("OH2MQK").unwrap().position, Some((60.49, 25.09)));
    }

    #[test]
    fn test_len_and_is_empty() {
        let mut db = HistoryDb::new(DEFAULT_TTL);
        assert_eq!(db.len(), 0);
        assert!(db.is_empty());

        db.heard("OH2MQK", "port0", None);
        assert_eq!(db.len(), 1);
        assert!(!db.is_empty());

        db.heard("OH2RDG", "port1", None);
        assert_eq!(db.len(), 2);
        assert!(!db.is_empty());
    }
}
