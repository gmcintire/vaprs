pub mod server;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use serde_json;

use crate::erlang::ErlangChannelSnapshot;

/// Maximum number of recent packets kept in the ring buffer.
const MAX_RECENT_PACKETS: usize = 200;

/// A snapshot of a single packet for the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct PacketSnapshot {
    pub timestamp: u64,
    pub source_call: String,
    pub dest_call: String,
    pub interface: String,
    pub payload: String,
    pub raw: String,
    pub sequence: u64,
}

/// A snapshot of a recently heard station.
#[derive(Debug, Clone, Serialize)]
pub struct StationSnapshot {
    pub callsign: String,
    pub last_heard_secs_ago: u64,
    pub interface: String,
    pub heard_count: u32,
    pub position: Option<(f64, f64)>,
}

/// Dashboard state shared between the web server and the rest of the system.
pub struct DashboardState {
    pub mycall: String,
    pub started_at: Instant,
    pub interfaces: Vec<String>,
    pub erlang_stats: Vec<ErlangChannelSnapshot>,
    pub aprsis_connected: bool,
    pub aprsis_server: String,
    pub recent_packets: Vec<PacketSnapshot>,
    pub packet_sequence: u64,
    pub stations: HashMap<String, StationEntry>,
}

/// Internal tracking for a heard station.
pub struct StationEntry {
    pub callsign: String,
    pub last_heard: Instant,
    pub interface: String,
    pub heard_count: u32,
    pub position: Option<(f64, f64)>,
}

pub type SharedDashboardState = Arc<Mutex<DashboardState>>;

impl DashboardState {
    pub fn new(mycall: &str, interfaces: Vec<String>) -> Self {
        Self {
            mycall: mycall.to_string(),
            started_at: Instant::now(),
            interfaces,
            erlang_stats: Vec::new(),
            aprsis_connected: false,
            aprsis_server: String::new(),
            recent_packets: Vec::new(),
            packet_sequence: 0,
            stations: HashMap::new(),
        }
    }

    /// Push a new packet snapshot, maintaining ring buffer size.
    pub fn push_packet(&mut self, mut snapshot: PacketSnapshot) {
        self.packet_sequence += 1;
        snapshot.sequence = self.packet_sequence;
        if self.recent_packets.len() >= MAX_RECENT_PACKETS {
            self.recent_packets.remove(0);
        }
        self.recent_packets.push(snapshot);
    }

    /// Record a station heard on an interface.
    pub fn record_station(
        &mut self,
        callsign: &str,
        interface: &str,
        position: Option<(f64, f64)>,
    ) {
        let key = callsign.to_uppercase();
        let now = Instant::now();
        let entry = self
            .stations
            .entry(key.clone())
            .or_insert_with(|| StationEntry {
                callsign: key,
                last_heard: now,
                interface: interface.to_string(),
                heard_count: 0,
                position: None,
            });
        entry.last_heard = now;
        entry.interface = interface.to_string();
        entry.heard_count += 1;
        if position.is_some() {
            entry.position = position;
        }
    }

    /// Build a JSON string of the full dashboard state.
    pub fn to_json(&self) -> String {
        let uptime_secs = self.started_at.elapsed().as_secs();
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut stations: Vec<StationSnapshot> = self
            .stations
            .values()
            .map(|e| StationSnapshot {
                callsign: e.callsign.clone(),
                last_heard_secs_ago: e.last_heard.elapsed().as_secs(),
                interface: e.interface.clone(),
                heard_count: e.heard_count,
                position: e.position,
            })
            .collect();
        stations.sort_by_key(|s| s.last_heard_secs_ago);

        // Compute packets per minute from erlang stats
        let (rx_per_min, tx_per_min) = self.erlang_stats.iter().fold((0u64, 0u64), |acc, ch| {
            (
                acc.0 + ch.last_1min.rx_packets,
                acc.1 + ch.last_1min.tx_packets,
            )
        });

        let state = DashboardJson {
            mycall: &self.mycall,
            uptime_secs,
            timestamp: now_epoch,
            interfaces: &self.interfaces,
            erlang_stats: &self.erlang_stats,
            aprsis_connected: self.aprsis_connected,
            aprsis_server: &self.aprsis_server,
            recent_packets: &self.recent_packets,
            packet_sequence: self.packet_sequence,
            stations,
            stations_heard: self.stations.len(),
            rx_per_min,
            tx_per_min,
        };

        serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string())
    }
}

#[derive(Serialize)]
struct DashboardJson<'a> {
    mycall: &'a str,
    uptime_secs: u64,
    timestamp: u64,
    interfaces: &'a [String],
    erlang_stats: &'a [ErlangChannelSnapshot],
    aprsis_connected: bool,
    aprsis_server: &'a str,
    recent_packets: &'a [PacketSnapshot],
    packet_sequence: u64,
    stations: Vec<StationSnapshot>,
    stations_heard: usize,
    rx_per_min: u64,
    tx_per_min: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dashboard_state_new() {
        let state = DashboardState::new("OH2MQK-1", vec!["radio0".to_string()]);
        assert_eq!(state.mycall, "OH2MQK-1");
        assert_eq!(state.interfaces, vec!["radio0"]);
        assert!(!state.aprsis_connected);
        assert!(state.recent_packets.is_empty());
        assert_eq!(state.packet_sequence, 0);
    }

    #[test]
    fn test_push_packet_increments_sequence() {
        let mut state = DashboardState::new("TEST", vec![]);
        let snap = PacketSnapshot {
            timestamp: 1000,
            source_call: "OH2MQK-1".to_string(),
            dest_call: "APRS".to_string(),
            interface: "radio0".to_string(),
            payload: "!6029.50N/02505.43E>".to_string(),
            raw: "OH2MQK-1>APRS:!6029.50N/02505.43E>".to_string(),
            sequence: 0,
        };
        state.push_packet(snap);
        assert_eq!(state.packet_sequence, 1);
        assert_eq!(state.recent_packets.len(), 1);
        assert_eq!(state.recent_packets[0].sequence, 1);
    }

    #[test]
    fn test_push_packet_ring_buffer() {
        let mut state = DashboardState::new("TEST", vec![]);
        for i in 0..250 {
            let snap = PacketSnapshot {
                timestamp: i,
                source_call: format!("SRC{}", i),
                dest_call: "DST".to_string(),
                interface: "radio0".to_string(),
                payload: "data".to_string(),
                raw: format!("SRC{}>DST:data", i),
                sequence: 0,
            };
            state.push_packet(snap);
        }
        assert_eq!(state.recent_packets.len(), MAX_RECENT_PACKETS);
        assert_eq!(state.packet_sequence, 250);
        // Oldest packet should be sequence 51 (first 50 were evicted)
        assert_eq!(state.recent_packets[0].sequence, 51);
    }

    #[test]
    fn test_record_station() {
        let mut state = DashboardState::new("TEST", vec![]);
        state.record_station("OH2MQK-1", "radio0", Some((60.49, 25.09)));
        assert_eq!(state.stations.len(), 1);

        let entry = state.stations.get("OH2MQK-1").unwrap();
        assert_eq!(entry.heard_count, 1);
        assert_eq!(entry.position, Some((60.49, 25.09)));
    }

    #[test]
    fn test_record_station_updates() {
        let mut state = DashboardState::new("TEST", vec![]);
        state.record_station("OH2MQK-1", "radio0", None);
        state.record_station("OH2MQK-1", "radio0", Some((60.49, 25.09)));
        state.record_station("oh2mqk-1", "radio1", None);

        assert_eq!(state.stations.len(), 1);
        let entry = state.stations.get("OH2MQK-1").unwrap();
        assert_eq!(entry.heard_count, 3);
        assert_eq!(entry.interface, "radio1");
        assert_eq!(entry.position, Some((60.49, 25.09)));
    }

    #[test]
    fn test_to_json_produces_valid_json() {
        let mut state = DashboardState::new("OH2MQK-1", vec!["radio0".to_string()]);
        state.aprsis_connected = true;
        state.aprsis_server = "rotate.aprs2.net:14580".to_string();

        let json = state.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["mycall"], "OH2MQK-1");
        assert_eq!(parsed["aprsis_connected"], true);
        assert_eq!(parsed["aprsis_server"], "rotate.aprs2.net:14580");
        assert!(parsed["uptime_secs"].as_u64().is_some());
    }

    #[test]
    fn test_to_json_with_packets_and_stations() {
        let mut state = DashboardState::new("TEST", vec![]);
        state.push_packet(PacketSnapshot {
            timestamp: 1000,
            source_call: "OH2MQK-1".to_string(),
            dest_call: "APRS".to_string(),
            interface: "radio0".to_string(),
            payload: "test".to_string(),
            raw: "OH2MQK-1>APRS:test".to_string(),
            sequence: 0,
        });
        state.record_station("OH2MQK-1", "radio0", Some((60.49, 25.09)));

        let json = state.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["packet_sequence"], 1);
        assert_eq!(parsed["stations_heard"], 1);
        assert_eq!(parsed["recent_packets"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["stations"].as_array().unwrap().len(), 1);
    }
}
