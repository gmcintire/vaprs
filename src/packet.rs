// Core packet type for APRS frame handling

use std::sync::Arc;
use std::time::Instant;

pub const MAX_AX25_LEN: usize = 2000;
pub const MAX_TNC2_LEN: usize = 2800;

#[derive(Debug, Clone)]
pub struct Packet {
    pub tnc2: String,
    pub tnc2_addr_len: usize,
    pub ax25: Option<Vec<u8>>,
    pub ax25_addr_len: usize,
    pub source_interface: String,
    pub is_aprs: bool,
    pub ui_pid: i16,
    pub received_at: Instant,
    pub igate_group: u8,
}

pub type SharedPacket = Arc<Packet>;

impl Packet {
    pub fn new(tnc2: &str, source_interface: &str, is_aprs: bool) -> Self {
        let addr_len = tnc2.find(':').unwrap_or(0);
        Self {
            tnc2: tnc2.to_string(),
            tnc2_addr_len: addr_len,
            ax25: None,
            ax25_addr_len: 0,
            source_interface: source_interface.to_string(),
            is_aprs,
            ui_pid: if is_aprs { 0xF0 } else { -1 },
            received_at: Instant::now(),
            igate_group: 0,
        }
    }

    pub fn payload(&self) -> &str {
        if self.tnc2_addr_len + 1 < self.tnc2.len() {
            &self.tnc2[self.tnc2_addr_len + 1..]
        } else {
            ""
        }
    }

    pub fn addresses(&self) -> &str {
        &self.tnc2[..self.tnc2_addr_len]
    }

    pub fn source_call(&self) -> &str {
        self.tnc2.split('>').next().unwrap_or("")
    }

    pub fn dest_call(&self) -> &str {
        let after_gt = self.tnc2.split('>').nth(1).unwrap_or("");
        after_gt.split([',', ':']).next().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_creation() {
        let pkt = Packet::new(
            "OH2MQK-1>APRS:!6029.50N/02505.43E>",
            "port0",
            true,
        );
        assert_eq!(pkt.source_interface, "port0");
        assert!(pkt.is_aprs);
        assert_eq!(pkt.tnc2_addr_len, 13); // "OH2MQK-1>APRS" is 13 chars
    }

    #[test]
    fn test_packet_payload() {
        let pkt = Packet::new("TEST>APRS:hello world", "port0", true);
        assert_eq!(pkt.payload(), "hello world");
    }

    #[test]
    fn test_packet_addresses() {
        let pkt = Packet::new("SRC>DST,VIA1,VIA2:payload", "port0", false);
        assert_eq!(pkt.addresses(), "SRC>DST,VIA1,VIA2");
    }

    #[test]
    fn test_packet_source_call() {
        let pkt = Packet::new("OH2MQK-1>APRS,WIDE1-1:test", "port0", true);
        assert_eq!(pkt.source_call(), "OH2MQK-1");
    }

    #[test]
    fn test_packet_dest_call() {
        let pkt = Packet::new("OH2MQK-1>APRS,WIDE1-1:test", "port0", true);
        assert_eq!(pkt.dest_call(), "APRS");
    }

    #[test]
    fn test_packet_arc_sharing() {
        let pkt = Packet::new("TEST>APRS:test", "port0", true);
        let shared = std::sync::Arc::new(pkt);
        let clone = shared.clone();
        assert_eq!(shared.tnc2, clone.tnc2);
    }
}
