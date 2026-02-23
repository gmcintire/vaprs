// KISS protocol framing encoder/decoder
// Ported from aprx kiss.c

use crate::crc;

/// KISS special bytes
pub const FEND: u8 = 0xC0;
pub const FESC: u8 = 0xDB;
pub const TFEND: u8 = 0xDC;
pub const TFESC: u8 = 0xDD;

/// Maximum KISS frame data size before discarding
const MAX_FRAME_LEN: usize = 2000;

/// KISS protocol variant for CRC handling
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KissVariant {
    Plain,
    Smack,
    FlexNet,
    BpqCrc,
}

/// A decoded KISS frame with command byte and payload data
#[derive(Debug, Clone)]
pub struct KissFrame {
    pub cmd_byte: u8,
    pub data: Vec<u8>,
}

impl KissFrame {
    /// Extract the TNC ID (upper 4 bits of command byte)
    pub fn tnc_id(&self) -> u8 {
        (self.cmd_byte >> 4) & 0x0F
    }

    /// Extract the command type (lower 4 bits of command byte)
    pub fn cmd(&self) -> u8 {
        self.cmd_byte & 0x0F
    }
}

/// Escape a byte for KISS framing: FEND -> FESC TFEND, FESC -> FESC TFESC
fn kiss_escape_byte(out: &mut Vec<u8>, byte: u8) {
    match byte {
        FEND => {
            out.push(FESC);
            out.push(TFEND);
        }
        FESC => {
            out.push(FESC);
            out.push(TFESC);
        }
        _ => out.push(byte),
    }
}

/// Encode data into a KISS frame with the specified variant.
///
/// Frame format: FEND | cmd_byte | escaped(data) [| escaped(CRC)] | FEND
pub fn kiss_encode(data: &[u8], cmd_byte: u8, variant: KissVariant) -> Vec<u8> {
    // Estimate capacity: FEND + cmd + data (worst case 2x) + CRC (4 bytes worst case) + FEND
    let mut frame = Vec::with_capacity(2 + data.len() * 2 + 4);

    frame.push(FEND);
    frame.push(cmd_byte);

    // Escape and append data bytes
    for &byte in data {
        kiss_escape_byte(&mut frame, byte);
    }

    // Append variant-specific CRC (escaped)
    match variant {
        KissVariant::Plain => {}
        KissVariant::Smack => {
            // CRC-16 over cmd_byte + data, appended little-endian
            let mut crc_data = Vec::with_capacity(1 + data.len());
            crc_data.push(cmd_byte);
            crc_data.extend_from_slice(data);
            let crc_val = crc::crc16(&crc_data);
            kiss_escape_byte(&mut frame, (crc_val & 0xFF) as u8);
            kiss_escape_byte(&mut frame, ((crc_val >> 8) & 0xFF) as u8);
        }
        KissVariant::FlexNet => {
            // FlexNet CRC over cmd_byte + data, appended big-endian
            let mut crc_data = Vec::with_capacity(1 + data.len());
            crc_data.push(cmd_byte);
            crc_data.extend_from_slice(data);
            let crc_val = crc::crc_flex(&crc_data);
            kiss_escape_byte(&mut frame, ((crc_val >> 8) & 0xFF) as u8);
            kiss_escape_byte(&mut frame, (crc_val & 0xFF) as u8);
        }
        KissVariant::BpqCrc => {
            // BPQ: XOR of all data bytes
            let mut xor: u8 = 0;
            for &byte in data {
                xor ^= byte;
            }
            kiss_escape_byte(&mut frame, xor);
        }
    }

    frame.push(FEND);
    frame
}

/// Internal state for the KISS decoder state machine
enum KissState {
    /// Looking for FEND to start a frame
    SyncHunt,
    /// Inside a frame, collecting bytes
    Collecting,
    /// After FESC, waiting for TFEND or TFESC
    Escaped,
}

/// Stateful KISS frame decoder (state machine)
///
/// Feed bytes in with `feed()`, which returns any complete frames decoded.
/// Handles incremental/streaming input and shared FENDs between frames.
pub struct KissDecoder {
    state: KissState,
    buffer: Vec<u8>,
}

impl KissDecoder {
    pub fn new() -> Self {
        Self {
            state: KissState::SyncHunt,
            buffer: Vec::with_capacity(512),
        }
    }

    /// Try to emit a frame from the current buffer contents.
    /// Returns Some(frame) if buffer has at least 1 byte (cmd_byte + data).
    fn emit_frame(&mut self) -> Option<KissFrame> {
        if self.buffer.is_empty() {
            return None;
        }

        let cmd_byte = self.buffer[0];
        let data = self.buffer[1..].to_vec();
        self.buffer.clear();

        Some(KissFrame { cmd_byte, data })
    }

    /// Feed bytes into the decoder, returning any complete frames.
    pub fn feed(&mut self, data: &[u8]) -> Vec<KissFrame> {
        let mut frames = Vec::new();

        for &byte in data {
            match self.state {
                KissState::SyncHunt => {
                    if byte == FEND {
                        self.state = KissState::Collecting;
                        self.buffer.clear();
                    }
                    // Discard everything else in SyncHunt
                }
                KissState::Collecting => match byte {
                    FEND => {
                        // End of frame (or start of next, or consecutive FENDs)
                        if let Some(frame) = self.emit_frame() {
                            frames.push(frame);
                        }
                        // Stay in Collecting: this FEND also starts the next frame
                    }
                    FESC => {
                        self.state = KissState::Escaped;
                    }
                    _ => {
                        self.buffer.push(byte);
                        if self.buffer.len() > MAX_FRAME_LEN {
                            // Oversized frame: discard and hunt for sync
                            self.buffer.clear();
                            self.state = KissState::SyncHunt;
                        }
                    }
                },
                KissState::Escaped => {
                    match byte {
                        TFEND => self.buffer.push(FEND),
                        TFESC => self.buffer.push(FESC),
                        _ => {} // Invalid escape sequence: discard
                    }
                    self.state = KissState::Collecting;
                }
            }
        }

        frames
    }
}

impl Default for KissDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kiss_encode_simple() {
        let data = vec![0x01, 0x02, 0x03];
        let frame = kiss_encode(&data, 0x00, KissVariant::Plain);
        assert_eq!(frame[0], FEND);
        assert_eq!(frame[1], 0x00);
        assert_eq!(frame[2], 0x01);
        assert_eq!(frame[3], 0x02);
        assert_eq!(frame[4], 0x03);
        assert_eq!(frame[5], FEND);
    }

    #[test]
    fn test_kiss_encode_escapes_fend() {
        let data = vec![FEND];
        let frame = kiss_encode(&data, 0x00, KissVariant::Plain);
        assert!(frame.windows(2).any(|w| w == [FESC, TFEND]));
        assert_eq!(frame.iter().filter(|&&b| b == FEND).count(), 2);
    }

    #[test]
    fn test_kiss_encode_escapes_fesc() {
        let data = vec![FESC];
        let frame = kiss_encode(&data, 0x00, KissVariant::Plain);
        assert!(frame.windows(2).any(|w| w == [FESC, TFESC]));
    }

    #[test]
    fn test_kiss_decode_simple() {
        let frame = vec![FEND, 0x00, 0x41, 0x42, 0x43, FEND];
        let mut decoder = KissDecoder::new();
        let result = decoder.feed(&frame);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].cmd_byte, 0x00);
        assert_eq!(result[0].data, vec![0x41, 0x42, 0x43]);
    }

    #[test]
    fn test_kiss_decode_with_escapes() {
        let frame = vec![FEND, 0x00, FESC, TFEND, FESC, TFESC, FEND];
        let mut decoder = KissDecoder::new();
        let result = decoder.feed(&frame);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].data, vec![FEND, FESC]);
    }

    #[test]
    fn test_kiss_decode_consecutive_fends() {
        let frame = vec![FEND, FEND, FEND, 0x00, 0x41, FEND];
        let mut decoder = KissDecoder::new();
        let result = decoder.feed(&frame);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_kiss_decode_incremental() {
        let frame = vec![FEND, 0x00, 0x41, 0x42, FEND];
        let mut decoder = KissDecoder::new();
        let mut results = Vec::new();
        for &b in &frame {
            results.extend(decoder.feed(&[b]));
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].data, vec![0x41, 0x42]);
    }

    #[test]
    fn test_kiss_decode_two_frames() {
        let data = vec![FEND, 0x00, 0x41, FEND, FEND, 0x00, 0x42, FEND];
        let mut decoder = KissDecoder::new();
        let result = decoder.feed(&data);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_kiss_decode_shared_fend() {
        // Some TNCs use a single FEND between frames
        let data = vec![FEND, 0x00, 0x41, FEND, 0x00, 0x42, FEND];
        let mut decoder = KissDecoder::new();
        let result = decoder.feed(&data);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_kiss_tncid_extraction() {
        let frame = vec![FEND, 0x30, 0x41, FEND]; // tncid = 3
        let mut decoder = KissDecoder::new();
        let result = decoder.feed(&frame);
        assert_eq!(result[0].tnc_id(), 3);
        assert_eq!(result[0].cmd(), 0);
    }

    #[test]
    fn test_kiss_encode_smack() {
        let data = vec![0x01, 0x02, 0x03];
        let frame = kiss_encode(&data, 0x80, KissVariant::Smack);
        // Should have CRC-16 appended before final FEND
        assert!(frame.len() > 6);
    }

    #[test]
    fn test_kiss_roundtrip() {
        // Encode then decode should give back original data
        let original = vec![0x41, 0x42, FEND, FESC, 0x43];
        let encoded = kiss_encode(&original, 0x00, KissVariant::Plain);
        let mut decoder = KissDecoder::new();
        let decoded = decoder.feed(&encoded);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].data, original);
        assert_eq!(decoded[0].cmd_byte, 0x00);
    }

    #[test]
    fn test_kiss_oversized_frame_discarded() {
        // A frame larger than 2000 bytes should be discarded
        let mut data = vec![FEND, 0x00];
        data.extend(vec![0x41; 2100]);
        data.push(FEND);
        let mut decoder = KissDecoder::new();
        let result = decoder.feed(&data);
        assert_eq!(result.len(), 0);
    }
}
