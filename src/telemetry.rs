// APRS telemetry packet generation.
//
// Generates T# telemetry data packets and the associated parameter
// definition messages (PARM, UNIT, EQNS) that describe what each
// telemetry channel represents.

/// Telemetry sequence counter (wraps 0-999).
pub struct TelemetryCounter {
    sequence: u16,
}

impl TelemetryCounter {
    pub fn new() -> Self {
        Self { sequence: 0 }
    }

    /// Return the next sequence number, advancing the counter.
    /// Wraps from 999 back to 0.
    pub fn next_seq(&mut self) -> u16 {
        let seq = self.sequence;
        self.sequence = (self.sequence + 1) % 1000;
        seq
    }
}

impl Default for TelemetryCounter {
    fn default() -> Self {
        Self::new()
    }
}

/// Format an APRS telemetry data packet.
///
/// Produces: `MYCALL>APRS:T#seq,val1,val2,val3,val4,val5,bbbbbbbb`
///
/// - `sequence`: 0-999 sequence number
/// - `values`: 5 analog values (each 0-255)
/// - `bits`: 8 digital bits (MSB first)
pub fn format_telemetry(mycall: &str, sequence: u16, values: [u8; 5], bits: u8) -> String {
    format!(
        "{}>APRS:T#{:03},{:03},{:03},{:03},{:03},{:03},{:08b}",
        mycall,
        sequence % 1000,
        values[0],
        values[1],
        values[2],
        values[3],
        values[4],
        bits,
    )
}

/// Pad an addressee callsign to 9 characters, left-justified, space-padded.
fn pad_addressee(call: &str) -> String {
    format!("{:<9}", call)
}

/// Format telemetry parameter names message.
///
/// Produces an APRS message-to-self defining what each telemetry channel means.
pub fn format_telemetry_params(mycall: &str, source_call: &str) -> String {
    format!(
        "{}>APRS::{}:PARM.Rx Pkts,Tx Pkts,Rx Bytes,Tx Bytes,Drops",
        mycall,
        pad_addressee(source_call),
    )
}

/// Format telemetry units message.
pub fn format_telemetry_units(mycall: &str) -> String {
    format!(
        "{}>APRS::{}:UNIT.Pkts,Pkts,Bytes,Bytes,Pkts",
        mycall,
        pad_addressee(mycall),
    )
}

/// Format telemetry equation coefficients message.
///
/// Coefficients define how to convert raw telemetry values to real units.
/// Format per channel: `a,b,c` where `real = a * v^2 + b * v + c`.
/// Our channels: Rx Pkts (0,1,0), Tx Pkts (0,1,0), Rx Bytes (0,10,0),
/// Tx Bytes (0,10,0), Drops (0,1,0).
pub fn format_telemetry_eqns(mycall: &str) -> String {
    format!(
        "{}>APRS::{}:EQNS.0,1,0,0,1,0,0,10,0,0,10,0,0,1,0",
        mycall,
        pad_addressee(mycall),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_telemetry() {
        let result = format_telemetry("OH2MQK-1", 42, [10, 5, 128, 64, 3], 0b10100000);
        assert_eq!(result, "OH2MQK-1>APRS:T#042,010,005,128,064,003,10100000");
    }

    #[test]
    fn test_telemetry_sequence_wraps() {
        let mut counter = TelemetryCounter::new();
        // Advance to 999
        for _ in 0..999 {
            counter.next_seq();
        }
        assert_eq!(counter.next_seq(), 999);
        // Should wrap to 0
        assert_eq!(counter.next_seq(), 0);
        assert_eq!(counter.next_seq(), 1);
    }

    #[test]
    fn test_format_telemetry_params() {
        let result = format_telemetry_params("OH2MQK-1", "OH2MQK-1");
        assert_eq!(
            result,
            "OH2MQK-1>APRS::OH2MQK-1 :PARM.Rx Pkts,Tx Pkts,Rx Bytes,Tx Bytes,Drops"
        );
    }

    #[test]
    fn test_format_telemetry_units() {
        let result = format_telemetry_units("OH2MQK-1");
        assert_eq!(
            result,
            "OH2MQK-1>APRS::OH2MQK-1 :UNIT.Pkts,Pkts,Bytes,Bytes,Pkts"
        );
    }

    #[test]
    fn test_format_telemetry_eqns() {
        let result = format_telemetry_eqns("OH2MQK-1");
        assert_eq!(
            result,
            "OH2MQK-1>APRS::OH2MQK-1 :EQNS.0,1,0,0,1,0,0,10,0,0,10,0,0,1,0"
        );
    }

    #[test]
    fn test_telemetry_values_clamped() {
        // Values are u8, so already 0-255. Verify extremes format correctly.
        let result = format_telemetry("TEST-1", 0, [0, 0, 0, 0, 0], 0);
        assert_eq!(result, "TEST-1>APRS:T#000,000,000,000,000,000,00000000");

        let result = format_telemetry("TEST-1", 999, [255, 255, 255, 255, 255], 0xFF);
        assert_eq!(result, "TEST-1>APRS:T#999,255,255,255,255,255,11111111");
    }

    #[test]
    fn test_telemetry_counter_starts_at_zero() {
        let mut counter = TelemetryCounter::new();
        assert_eq!(counter.next_seq(), 0);
    }

    #[test]
    fn test_telemetry_counter_default() {
        let mut counter = TelemetryCounter::default();
        assert_eq!(counter.next_seq(), 0);
    }

    #[test]
    fn test_pad_addressee_short_call() {
        assert_eq!(pad_addressee("N0CALL"), "N0CALL   ");
    }

    #[test]
    fn test_pad_addressee_full_length() {
        assert_eq!(pad_addressee("OH2MQK-15"), "OH2MQK-15");
    }

    #[test]
    fn test_format_telemetry_sequence_wraps_in_format() {
        // Sequence values > 999 should still produce 3-digit output via modulo
        let result = format_telemetry("TEST-1", 1001, [1, 2, 3, 4, 5], 0);
        assert!(result.contains("T#001,"));
    }
}
