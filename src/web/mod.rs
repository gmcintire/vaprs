pub mod server;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use serde_json;

use crate::erlang::ErlangChannelSnapshot;
use crate::igate::GateResult;

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

/// iGate statistics counters for the dashboard.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IgateStats {
    pub rx_from_rf: u64,
    pub gated_to_aprsis: u64,
    pub dropped_total: u64,
    pub dropped_query: u64,
    pub dropped_forbidden_source: u64,
    pub dropped_forbidden_dest: u64,
    pub dropped_forbidden_via: u64,
    pub dropped_depth_exceeded: u64,
    pub unique_stations_gated: u64,
}

/// Dashboard state shared between the web server and the rest of the system.
pub struct DashboardState {
    pub mycall: String,
    pub started_at: Instant,
    pub interfaces: Vec<String>,
    pub erlang_stats: Vec<ErlangChannelSnapshot>,
    pub aprsis_connected: bool,
    pub aprsis_server: String,
    pub recent_packets: VecDeque<PacketSnapshot>,
    pub packet_sequence: u64,
    pub stations: HashMap<String, StationEntry>,
    pub igate_stats: IgateStats,
    /// Set of callsigns that have been gated (not serialized).
    stations_gated_set: HashSet<String>,
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
            recent_packets: VecDeque::new(),
            packet_sequence: 0,
            stations: HashMap::new(),
            igate_stats: IgateStats::default(),
            stations_gated_set: HashSet::new(),
        }
    }

    /// Push a new packet snapshot, maintaining ring buffer size.
    pub fn push_packet(&mut self, mut snapshot: PacketSnapshot) {
        self.packet_sequence += 1;
        snapshot.sequence = self.packet_sequence;
        if self.recent_packets.len() >= MAX_RECENT_PACKETS {
            self.recent_packets.pop_front();
        }
        self.recent_packets.push_back(snapshot);
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

    /// Record the result of an iGate decision for a packet from RF.
    pub fn record_igate_result(&mut self, source_call: &str, result: &GateResult) {
        self.igate_stats.rx_from_rf += 1;
        match result {
            GateResult::Gated(_) => {
                self.igate_stats.gated_to_aprsis += 1;
                if self.stations_gated_set.insert(source_call.to_uppercase()) {
                    self.igate_stats.unique_stations_gated = self.stations_gated_set.len() as u64;
                }
            }
            GateResult::DroppedQuery => {
                self.igate_stats.dropped_total += 1;
                self.igate_stats.dropped_query += 1;
            }
            GateResult::DroppedForbiddenSource => {
                self.igate_stats.dropped_total += 1;
                self.igate_stats.dropped_forbidden_source += 1;
            }
            GateResult::DroppedForbiddenDest => {
                self.igate_stats.dropped_total += 1;
                self.igate_stats.dropped_forbidden_dest += 1;
            }
            GateResult::DroppedForbiddenVia => {
                self.igate_stats.dropped_total += 1;
                self.igate_stats.dropped_forbidden_via += 1;
            }
            GateResult::DroppedDepthExceeded => {
                self.igate_stats.dropped_total += 1;
                self.igate_stats.dropped_depth_exceeded += 1;
            }
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

        // Compute packets per minute from erlang stats.
        // Use the current (in-progress) window for live counts; fall back to
        // last_1min if the current window is empty (just after a rotation).
        let (rx_per_min, tx_per_min) = self.erlang_stats.iter().fold((0u64, 0u64), |acc, ch| {
            let rx = if ch.current.rx_packets > 0 {
                ch.current.rx_packets
            } else {
                ch.last_1min.rx_packets
            };
            let tx = if ch.current.tx_packets > 0 {
                ch.current.tx_packets
            } else {
                ch.last_1min.tx_packets
            };
            (acc.0 + rx, acc.1 + tx)
        });

        let state = DashboardJson {
            version: env!("CARGO_PKG_VERSION"),
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
            igate_stats: &self.igate_stats,
        };

        serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string())
    }
}

#[derive(Serialize)]
struct DashboardJson<'a> {
    version: &'a str,
    mycall: &'a str,
    uptime_secs: u64,
    timestamp: u64,
    interfaces: &'a [String],
    erlang_stats: &'a [ErlangChannelSnapshot],
    aprsis_connected: bool,
    aprsis_server: &'a str,
    recent_packets: &'a VecDeque<PacketSnapshot>,
    packet_sequence: u64,
    stations: Vec<StationSnapshot>,
    stations_heard: usize,
    rx_per_min: u64,
    tx_per_min: u64,
    igate_stats: &'a IgateStats,
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
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["aprsis_connected"], true);
        assert_eq!(parsed["aprsis_server"], "rotate.aprs2.net:14580");
        assert!(parsed["uptime_secs"].as_u64().is_some());
    }

    #[test]
    fn test_record_igate_gated() {
        let mut state = DashboardState::new("TEST", vec![]);
        let result = GateResult::Gated("TEST>APRS,qAR,TEST:data".to_string());
        state.record_igate_result("OH2MQK", &result);

        assert_eq!(state.igate_stats.rx_from_rf, 1);
        assert_eq!(state.igate_stats.gated_to_aprsis, 1);
        assert_eq!(state.igate_stats.unique_stations_gated, 1);
        assert_eq!(state.igate_stats.dropped_total, 0);
    }

    #[test]
    fn test_record_igate_drops() {
        let mut state = DashboardState::new("TEST", vec![]);
        state.record_igate_result("A", &GateResult::DroppedQuery);
        state.record_igate_result("B", &GateResult::DroppedForbiddenSource);
        state.record_igate_result("C", &GateResult::DroppedForbiddenDest);
        state.record_igate_result("D", &GateResult::DroppedForbiddenVia);
        state.record_igate_result("E", &GateResult::DroppedDepthExceeded);

        assert_eq!(state.igate_stats.rx_from_rf, 5);
        assert_eq!(state.igate_stats.gated_to_aprsis, 0);
        assert_eq!(state.igate_stats.dropped_total, 5);
        assert_eq!(state.igate_stats.dropped_query, 1);
        assert_eq!(state.igate_stats.dropped_forbidden_source, 1);
        assert_eq!(state.igate_stats.dropped_forbidden_dest, 1);
        assert_eq!(state.igate_stats.dropped_forbidden_via, 1);
        assert_eq!(state.igate_stats.dropped_depth_exceeded, 1);
    }

    #[test]
    fn test_record_igate_unique_stations() {
        let mut state = DashboardState::new("TEST", vec![]);
        let result = GateResult::Gated("line".to_string());
        state.record_igate_result("OH2MQK", &result);
        state.record_igate_result("oh2mqk", &result); // same station, different case
        state.record_igate_result("KB1ABC", &result);

        assert_eq!(state.igate_stats.gated_to_aprsis, 3);
        assert_eq!(state.igate_stats.unique_stations_gated, 2);
    }

    #[test]
    fn test_igate_stats_in_json() {
        let mut state = DashboardState::new("TEST", vec![]);
        let result = GateResult::Gated("line".to_string());
        state.record_igate_result("OH2MQK", &result);
        state.record_igate_result("X", &GateResult::DroppedQuery);

        let json = state.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let ig = &parsed["igate_stats"];
        assert_eq!(ig["rx_from_rf"], 2);
        assert_eq!(ig["gated_to_aprsis"], 1);
        assert_eq!(ig["dropped_total"], 1);
        assert_eq!(ig["dropped_query"], 1);
        assert_eq!(ig["unique_stations_gated"], 1);
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
