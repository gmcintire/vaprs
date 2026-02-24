use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::packet::Packet;

/// A pending packet waiting for its viscous delay to expire.
struct PendingPacket {
    packet: Arc<Packet>,
    /// The digipeated TNC2 string (already processed by process_digipeat).
    digipeated_tnc2: String,
    /// When this entry was added.
    added_at: Instant,
    /// How long to delay before transmitting.
    delay: Duration,
}

/// Viscous delay queue for digipeating.
///
/// Holds packets for a configurable delay before retransmitting. If the same
/// packet is heard from another source during the delay, the pending
/// retransmission is cancelled. This prevents multiple digipeaters from all
/// retransmitting the same packet, reducing channel congestion.
pub struct ViscousQueue {
    pending: Vec<PendingPacket>,
}

impl Default for ViscousQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ViscousQueue {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Add a packet to the viscous delay queue.
    /// The `digipeated_tnc2` is the already-processed output from `process_digipeat()`.
    pub fn add(&mut self, packet: Arc<Packet>, digipeated_tnc2: String, delay: Duration) {
        self.pending.push(PendingPacket {
            packet,
            digipeated_tnc2,
            added_at: Instant::now(),
            delay,
        });
    }

    /// Check if a newly heard packet cancels any pending retransmissions.
    /// Returns the number of cancelled entries.
    ///
    /// A packet matches if it has the same source>destination and payload
    /// (same logic as `DupeChecker`). VIAs are ignored, trailing spaces are
    /// stripped.
    pub fn cancel_if_heard(&mut self, packet: &Packet) -> usize {
        let heard_key = dedup_key(packet);
        let before = self.pending.len();
        self.pending
            .retain(|entry| dedup_key(&entry.packet) != heard_key);
        before - self.pending.len()
    }

    /// Drain packets whose delay has expired, returning them for transmission.
    pub fn drain_expired(&mut self) -> Vec<(Arc<Packet>, String)> {
        let now = Instant::now();
        let mut expired = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            if now.duration_since(self.pending[i].added_at) >= self.pending[i].delay {
                let entry = self.pending.swap_remove(i);
                expired.push((entry.packet, entry.digipeated_tnc2));
                // Don't increment i; swap_remove moved the last element here
            } else {
                i += 1;
            }
        }
        expired
    }

    /// Number of pending packets.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Build the dedup key for a packet: (source>dest, trimmed_payload).
/// Same logic as DupeChecker -- VIAs are ignored, trailing spaces stripped.
fn dedup_key(packet: &Packet) -> (String, String) {
    let addr = format!("{}>{}", packet.source_call(), packet.dest_call());
    let payload = packet.payload().trim_end().to_string();
    (addr, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn make_shared(tnc2: &str) -> Arc<Packet> {
        Arc::new(Packet::new(tnc2, "port0", true))
    }

    #[test]
    fn test_add_and_drain_expired() {
        let mut queue = ViscousQueue::new();
        let pkt = make_shared("SRC>DST,WIDE1-1:hello");
        queue.add(pkt, "SRC>DST,MYCALL*:hello".to_string(), Duration::ZERO);

        let expired = queue.drain_expired();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].1, "SRC>DST,MYCALL*:hello");
    }

    #[test]
    fn test_drain_respects_delay() {
        let mut queue = ViscousQueue::new();
        let pkt = make_shared("SRC>DST,WIDE1-1:hello");
        queue.add(
            pkt,
            "SRC>DST,MYCALL*:hello".to_string(),
            Duration::from_secs(1),
        );

        // Should not drain immediately
        let expired = queue.drain_expired();
        assert!(expired.is_empty());
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn test_drain_after_delay() {
        let mut queue = ViscousQueue::new();
        let pkt = make_shared("SRC>DST,WIDE1-1:hello");
        queue.add(
            pkt,
            "SRC>DST,MYCALL*:hello".to_string(),
            Duration::from_millis(50),
        );

        // Wait for the delay to expire
        thread::sleep(Duration::from_millis(60));

        let expired = queue.drain_expired();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].1, "SRC>DST,MYCALL*:hello");
    }

    #[test]
    fn test_cancel_if_heard() {
        let mut queue = ViscousQueue::new();
        let pkt = make_shared("SRC>DST,WIDE1-1:hello");
        queue.add(
            pkt,
            "SRC>DST,MYCALL*:hello".to_string(),
            Duration::from_secs(30),
        );
        assert_eq!(queue.len(), 1);

        // Hearing the same packet from another path should cancel it
        let heard = Packet::new("SRC>DST,DIGI1*:hello", "port1", true);
        let cancelled = queue.cancel_if_heard(&heard);
        assert_eq!(cancelled, 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_cancel_ignores_different_packet() {
        let mut queue = ViscousQueue::new();
        let pkt = make_shared("SRC>DST,WIDE1-1:hello");
        queue.add(
            pkt,
            "SRC>DST,MYCALL*:hello".to_string(),
            Duration::from_secs(30),
        );

        // Different payload should not cancel
        let heard = Packet::new("SRC>DST:world", "port1", true);
        let cancelled = queue.cancel_if_heard(&heard);
        assert_eq!(cancelled, 0);
        assert_eq!(queue.len(), 1);

        // Different source should not cancel
        let heard2 = Packet::new("OTHER>DST:hello", "port1", true);
        let cancelled2 = queue.cancel_if_heard(&heard2);
        assert_eq!(cancelled2, 0);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn test_cancel_ignores_vias() {
        let mut queue = ViscousQueue::new();
        let pkt = make_shared("SRC>DST,WIDE2-2:hello");
        queue.add(
            pkt,
            "SRC>DST,MYCALL*,WIDE2-1:hello".to_string(),
            Duration::from_secs(30),
        );

        // Same src>dst and payload but completely different VIAs should still cancel
        let heard = Packet::new("SRC>DST,OTHERDIGI*,RELAY*:hello", "port1", true);
        let cancelled = queue.cancel_if_heard(&heard);
        assert_eq!(cancelled, 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_multiple_pending() {
        let mut queue = ViscousQueue::new();
        let pkt1 = make_shared("SRC1>DST:aaa");
        let pkt2 = make_shared("SRC2>DST:bbb");
        let pkt3 = make_shared("SRC1>DST:aaa  "); // trailing spaces match pkt1

        queue.add(
            pkt1,
            "SRC1>DST,MYCALL*:aaa".to_string(),
            Duration::from_secs(30),
        );
        queue.add(
            pkt2,
            "SRC2>DST,MYCALL*:bbb".to_string(),
            Duration::from_secs(30),
        );
        queue.add(
            pkt3,
            "SRC1>DST,MYCALL*:aaa".to_string(),
            Duration::from_secs(30),
        );
        assert_eq!(queue.len(), 3);

        // Cancel all packets matching SRC1>DST + "aaa"
        let heard = Packet::new("SRC1>DST:aaa", "port1", true);
        let cancelled = queue.cancel_if_heard(&heard);
        assert_eq!(cancelled, 2); // pkt1 and pkt3 matched
        assert_eq!(queue.len(), 1); // pkt2 remains
    }

    #[test]
    fn test_len_and_is_empty() {
        let mut queue = ViscousQueue::new();
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);

        let pkt1 = make_shared("SRC>DST:aaa");
        queue.add(
            pkt1,
            "SRC>DST,MYCALL*:aaa".to_string(),
            Duration::from_secs(30),
        );
        assert!(!queue.is_empty());
        assert_eq!(queue.len(), 1);

        let pkt2 = make_shared("SRC>DST:bbb");
        queue.add(
            pkt2,
            "SRC>DST,MYCALL*:bbb".to_string(),
            Duration::from_secs(30),
        );
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn test_drain_removes_from_queue() {
        let mut queue = ViscousQueue::new();
        let pkt1 = make_shared("SRC>DST:aaa");
        let pkt2 = make_shared("SRC>DST:bbb");

        queue.add(pkt1, "SRC>DST,MYCALL*:aaa".to_string(), Duration::ZERO);
        queue.add(
            pkt2,
            "SRC>DST,MYCALL*:bbb".to_string(),
            Duration::from_secs(30),
        );
        assert_eq!(queue.len(), 2);

        // Only the zero-delay packet should drain
        let expired = queue.drain_expired();
        assert_eq!(expired.len(), 1);
        assert_eq!(queue.len(), 1);
    }
}
