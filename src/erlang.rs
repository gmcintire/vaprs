// Erlang monitoring - per-interface byte/packet counting at
// 1-minute, 10-minute, and 60-minute time windows.
//
// Each interface tracks running totals of packets received/transmitted,
// bytes received/transmitted, and drops. Every minute the current window
// is rotated into history, and rolling 10-minute and 60-minute summaries
// are recomputed from the stored minute samples.

use serde::Serialize;
use std::time::Instant;

/// Statistics for a single time window.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WindowStats {
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub drops: u64,
}

impl WindowStats {
    /// Sum two WindowStats together.
    fn add(&self, other: &WindowStats) -> WindowStats {
        WindowStats {
            rx_packets: self.rx_packets + other.rx_packets,
            tx_packets: self.tx_packets + other.tx_packets,
            rx_bytes: self.rx_bytes + other.rx_bytes,
            tx_bytes: self.tx_bytes + other.tx_bytes,
            drops: self.drops + other.drops,
        }
    }
}

/// Per-interface channel statistics.
pub struct ChannelStats {
    /// Callsign/name of the interface.
    pub name: String,
    /// Running totals for the current 1-minute window.
    current: WindowStats,
    /// Completed stats for the last 1-minute window.
    pub last_1min: WindowStats,
    /// Accumulated stats for the last 10 minutes.
    pub last_10min: WindowStats,
    /// Accumulated stats for the last 60 minutes.
    pub last_60min: WindowStats,
    /// Ring buffer of minute samples for computing rolling windows.
    minute_samples: Vec<WindowStats>,
    /// When the last rotation occurred.
    last_rotation: Instant,
}

impl ChannelStats {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            current: WindowStats::default(),
            last_1min: WindowStats::default(),
            last_10min: WindowStats::default(),
            last_60min: WindowStats::default(),
            minute_samples: Vec::new(),
            last_rotation: Instant::now(),
        }
    }

    /// Record a received packet with the given byte count.
    pub fn record_rx(&mut self, bytes: u64) {
        self.current.rx_packets += 1;
        self.current.rx_bytes += bytes;
    }

    /// Record a transmitted packet with the given byte count.
    pub fn record_tx(&mut self, bytes: u64) {
        self.current.tx_packets += 1;
        self.current.tx_bytes += bytes;
    }

    /// Record a dropped packet.
    pub fn record_drop(&mut self) {
        self.current.drops += 1;
    }

    /// Access the current (in-progress) window stats.
    pub fn current(&self) -> &WindowStats {
        &self.current
    }

    /// Rotate the 1-minute window.
    ///
    /// Moves current stats to `last_1min`, pushes the sample into the
    /// ring buffer, recomputes the 10-minute and 60-minute rolling
    /// summaries, and resets the current window to zero.
    pub fn rotate(&mut self) {
        // Move current into last_1min
        self.last_1min = self.current.clone();

        // Push to ring buffer (keep at most 60 samples)
        self.minute_samples.push(self.current.clone());
        if self.minute_samples.len() > 60 {
            self.minute_samples.remove(0);
        }

        // Recompute last_10min from the most recent 10 samples
        let ten_start = self.minute_samples.len().saturating_sub(10);
        self.last_10min = self.minute_samples[ten_start..]
            .iter()
            .fold(WindowStats::default(), |acc, s| acc.add(s));

        // Recompute last_60min from all samples (up to 60)
        self.last_60min = self
            .minute_samples
            .iter()
            .fold(WindowStats::default(), |acc, s| acc.add(s));

        // Reset current
        self.current = WindowStats::default();
        self.last_rotation = Instant::now();
    }
}

/// Manager for all interface channel stats.
pub struct ErlangMonitor {
    channels: Vec<ChannelStats>,
}

impl ErlangMonitor {
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
        }
    }

    /// Add a new channel for the given interface name.
    pub fn add_channel(&mut self, name: &str) {
        self.channels.push(ChannelStats::new(name));
    }

    /// Look up a channel by name (immutable).
    pub fn get(&self, name: &str) -> Option<&ChannelStats> {
        self.channels.iter().find(|c| c.name == name)
    }

    /// Look up a channel by name (mutable).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut ChannelStats> {
        self.channels.iter_mut().find(|c| c.name == name)
    }

    /// Rotate all channels' 1-minute windows.
    pub fn rotate_all(&mut self) {
        for channel in &mut self.channels {
            channel.rotate();
        }
    }

    /// Return a slice of all channels.
    pub fn channels(&self) -> &[ChannelStats] {
        &self.channels
    }

    /// Snapshot all channel stats for the dashboard.
    pub fn snapshot(&self) -> Vec<ErlangChannelSnapshot> {
        self.channels
            .iter()
            .map(|ch| ErlangChannelSnapshot {
                name: ch.name.clone(),
                current: ch.current.clone(),
                last_1min: ch.last_1min.clone(),
                last_10min: ch.last_10min.clone(),
                last_60min: ch.last_60min.clone(),
            })
            .collect()
    }
}

/// Serializable snapshot of a single channel's erlang stats.
#[derive(Debug, Clone, Serialize)]
pub struct ErlangChannelSnapshot {
    pub name: String,
    pub current: WindowStats,
    pub last_1min: WindowStats,
    pub last_10min: WindowStats,
    pub last_60min: WindowStats,
}

impl Default for ErlangMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_rx() {
        let mut stats = ChannelStats::new("port0");
        stats.record_rx(100);
        stats.record_rx(200);

        assert_eq!(stats.current().rx_packets, 2);
        assert_eq!(stats.current().rx_bytes, 300);
        // TX and drops should be untouched
        assert_eq!(stats.current().tx_packets, 0);
        assert_eq!(stats.current().tx_bytes, 0);
        assert_eq!(stats.current().drops, 0);
    }

    #[test]
    fn test_record_tx() {
        let mut stats = ChannelStats::new("port0");
        stats.record_tx(50);
        stats.record_tx(75);
        stats.record_tx(25);

        assert_eq!(stats.current().tx_packets, 3);
        assert_eq!(stats.current().tx_bytes, 150);
        assert_eq!(stats.current().rx_packets, 0);
    }

    #[test]
    fn test_rotate_moves_to_last_1min() {
        let mut stats = ChannelStats::new("port0");
        stats.record_rx(100);
        stats.record_tx(50);
        stats.record_drop();

        stats.rotate();

        assert_eq!(stats.last_1min.rx_packets, 1);
        assert_eq!(stats.last_1min.rx_bytes, 100);
        assert_eq!(stats.last_1min.tx_packets, 1);
        assert_eq!(stats.last_1min.tx_bytes, 50);
        assert_eq!(stats.last_1min.drops, 1);
    }

    #[test]
    fn test_rotate_resets_current() {
        let mut stats = ChannelStats::new("port0");
        stats.record_rx(100);
        stats.record_tx(50);
        stats.record_drop();

        stats.rotate();

        assert_eq!(stats.current().rx_packets, 0);
        assert_eq!(stats.current().rx_bytes, 0);
        assert_eq!(stats.current().tx_packets, 0);
        assert_eq!(stats.current().tx_bytes, 0);
        assert_eq!(stats.current().drops, 0);
    }

    #[test]
    fn test_10min_accumulation() {
        let mut stats = ChannelStats::new("port0");

        // Simulate 10 minutes, each with 5 rx packets of 100 bytes
        for _ in 0..10 {
            for _ in 0..5 {
                stats.record_rx(100);
            }
            stats.rotate();
        }

        assert_eq!(stats.last_10min.rx_packets, 50); // 5 * 10
        assert_eq!(stats.last_10min.rx_bytes, 5000); // 100 * 5 * 10
    }

    #[test]
    fn test_10min_window_slides() {
        let mut stats = ChannelStats::new("port0");

        // First 10 minutes: 10 packets each
        for _ in 0..10 {
            for _ in 0..10 {
                stats.record_rx(10);
            }
            stats.rotate();
        }
        assert_eq!(stats.last_10min.rx_packets, 100);

        // Next 5 minutes: 1 packet each
        for _ in 0..5 {
            stats.record_rx(10);
            stats.rotate();
        }

        // last_10min should now include 5 old (10 pkts each) + 5 new (1 pkt each)
        assert_eq!(stats.last_10min.rx_packets, 55);
    }

    #[test]
    fn test_60min_accumulation() {
        let mut stats = ChannelStats::new("port0");

        // Simulate 60 minutes, each with 1 rx packet of 10 bytes
        for _ in 0..60 {
            stats.record_rx(10);
            stats.rotate();
        }

        assert_eq!(stats.last_60min.rx_packets, 60);
        assert_eq!(stats.last_60min.rx_bytes, 600);
    }

    #[test]
    fn test_60min_ring_buffer_cap() {
        let mut stats = ChannelStats::new("port0");

        // Simulate 70 minutes to verify ring buffer stays at 60
        for i in 0..70 {
            stats.record_rx(i + 1);
            stats.rotate();
        }

        // Ring buffer should have exactly 60 entries
        assert_eq!(stats.minute_samples.len(), 60);

        // last_60min should only cover the last 60 minutes (minutes 11-70)
        // Bytes: sum of 11..=70 = sum(1..=70) - sum(1..=10)
        let expected_bytes: u64 = (11..=70).sum();
        assert_eq!(stats.last_60min.rx_bytes, expected_bytes);
        assert_eq!(stats.last_60min.rx_packets, 60);
    }

    #[test]
    fn test_monitor_add_and_get() {
        let mut monitor = ErlangMonitor::new();
        monitor.add_channel("port0");
        monitor.add_channel("aprsis");

        assert!(monitor.get("port0").is_some());
        assert!(monitor.get("aprsis").is_some());
        assert!(monitor.get("nonexistent").is_none());

        assert_eq!(monitor.channels().len(), 2);
    }

    #[test]
    fn test_monitor_get_mut() {
        let mut monitor = ErlangMonitor::new();
        monitor.add_channel("port0");

        if let Some(ch) = monitor.get_mut("port0") {
            ch.record_rx(100);
        }

        let ch = monitor.get("port0").unwrap();
        assert_eq!(ch.current().rx_packets, 1);
        assert_eq!(ch.current().rx_bytes, 100);
    }

    #[test]
    fn test_rotate_all() {
        let mut monitor = ErlangMonitor::new();
        monitor.add_channel("port0");
        monitor.add_channel("port1");

        // Record some data on each channel
        monitor.get_mut("port0").unwrap().record_rx(100);
        monitor.get_mut("port1").unwrap().record_tx(200);

        monitor.rotate_all();

        // Both channels should have their current reset and last_1min populated
        let p0 = monitor.get("port0").unwrap();
        assert_eq!(p0.last_1min.rx_packets, 1);
        assert_eq!(p0.current().rx_packets, 0);

        let p1 = monitor.get("port1").unwrap();
        assert_eq!(p1.last_1min.tx_packets, 1);
        assert_eq!(p1.current().tx_packets, 0);
    }

    #[test]
    fn test_monitor_default() {
        let monitor = ErlangMonitor::default();
        assert!(monitor.channels().is_empty());
    }

    #[test]
    fn test_record_drop() {
        let mut stats = ChannelStats::new("port0");
        stats.record_drop();
        stats.record_drop();
        stats.record_drop();

        assert_eq!(stats.current().drops, 3);
        assert_eq!(stats.current().rx_packets, 0);
        assert_eq!(stats.current().tx_packets, 0);
    }

    #[test]
    fn test_window_stats_default() {
        let ws = WindowStats::default();
        assert_eq!(ws.rx_packets, 0);
        assert_eq!(ws.tx_packets, 0);
        assert_eq!(ws.rx_bytes, 0);
        assert_eq!(ws.tx_bytes, 0);
        assert_eq!(ws.drops, 0);
    }
}
