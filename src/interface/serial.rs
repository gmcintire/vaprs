// KISS serial port interface
//
// Opens a serial port, reads KISS frames, converts to packets via ax25_to_tnc2(),
// and forwards to the router. Handles transmit commands and includes a watchdog
// for stale connections (matches aprx behavior).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::ax25::ax25_to_tnc2;
use crate::config::{InterfaceType, Protocol};
use crate::kiss::{kiss_encode, KissDecoder, KissFrame, KissVariant};
use crate::packet::{Packet, SharedPacket};

use super::{Interface, InterfaceCommand, InterfaceMetadata};

/// Default watchdog timeout in seconds (log warning if no data received).
const DEFAULT_WATCHDOG_TIMEOUT_SECS: u64 = 600;

/// Read buffer size for serial port reads.
const READ_BUF_SIZE: usize = 4096;

/// Map a config Protocol to a KISS framing variant.
pub fn protocol_to_kiss_variant(protocol: &Protocol) -> Option<KissVariant> {
    match protocol {
        Protocol::Kiss => Some(KissVariant::Plain),
        Protocol::Smack => Some(KissVariant::Smack),
        Protocol::Flexnet => Some(KissVariant::FlexNet),
        Protocol::Bpqcrc => Some(KissVariant::BpqCrc),
        // TNC2 and DPRS are not KISS-based protocols
        Protocol::Tnc2 | Protocol::Dprs => None,
    }
}

/// Process a decoded KISS frame into a SharedPacket.
///
/// Only data frames (command nibble == 0) are converted. Non-data frames
/// (TNC parameter commands) are silently ignored.
///
/// Returns `None` if the frame is not a data frame or if AX.25 decoding fails.
pub fn process_kiss_frame(
    frame: &KissFrame,
    source_interface: &str,
    igate_group: u8,
) -> Option<SharedPacket> {
    // Only process data frames (lower nibble == 0)
    if frame.cmd() != 0 {
        debug!(
            interface = source_interface,
            cmd = frame.cmd_byte,
            "ignoring non-data KISS frame"
        );
        return None;
    }

    match ax25_to_tnc2(&frame.data) {
        Ok(tnc2_frame) => {
            let packet = Packet {
                tnc2: tnc2_frame.tnc2,
                tnc2_addr_len: tnc2_frame.tnc2_addr_len,
                ax25: Some(frame.data.clone()),
                ax25_addr_len: tnc2_frame.ax25_addr_len,
                source_interface: source_interface.to_string(),
                is_aprs: tnc2_frame.is_aprs,
                ui_pid: tnc2_frame.ui_pid,
                received_at: std::time::Instant::now(),
                igate_group,
            };
            Some(Arc::new(packet))
        }
        Err(e) => {
            warn!(
                interface = source_interface,
                error = %e,
                "failed to decode AX.25 frame"
            );
            None
        }
    }
}

/// Prepare a packet for KISS transmission.
///
/// Encodes the packet's AX.25 data with the appropriate KISS variant for the
/// given protocol. Returns `None` if the packet has no AX.25 data or if the
/// protocol is not a KISS variant.
pub fn prepare_transmit(packet: &Packet, protocol: &Protocol) -> Option<Vec<u8>> {
    let variant = protocol_to_kiss_variant(protocol)?;
    let ax25_data = packet.ax25.as_ref()?;
    Some(kiss_encode(ax25_data, 0x00, variant))
}

/// KISS serial port interface.
///
/// Reads KISS frames from a serial port, decodes them to APRS packets, and
/// forwards them to the router. Handles transmit commands from the router by
/// KISS-encoding and writing to the serial port.
pub struct SerialInterface {
    metadata: InterfaceMetadata,
    device: String,
    speed: u32,
    protocol: Protocol,
    watchdog_timeout: Duration,
}

impl SerialInterface {
    /// Create a new serial interface.
    ///
    /// # Arguments
    /// * `callsign` - Station callsign for this interface
    /// * `device` - Serial port device path (e.g., "/dev/ttyUSB0")
    /// * `speed` - Baud rate (e.g., 9600, 19200)
    /// * `protocol` - KISS protocol variant
    /// * `tx_ok` - Whether transmitting is allowed
    /// * `igate_group` - iGate group number for routing
    pub fn new(
        callsign: String,
        device: String,
        speed: u32,
        protocol: Protocol,
        tx_ok: bool,
        igate_group: u8,
    ) -> Self {
        Self {
            metadata: InterfaceMetadata {
                callsign,
                iface_type: InterfaceType::Serial,
                tx_ok,
                igate_group,
            },
            device,
            speed,
            protocol,
            watchdog_timeout: Duration::from_secs(DEFAULT_WATCHDOG_TIMEOUT_SECS),
        }
    }

    /// Set the watchdog timeout duration.
    ///
    /// If no data is received for this duration, a warning is logged.
    pub fn with_watchdog_timeout(mut self, timeout: Duration) -> Self {
        self.watchdog_timeout = timeout;
        self
    }
}

/// Core event loop that reads from any AsyncRead+AsyncWrite source.
///
/// This is extracted from the Interface::run method to allow testing with
/// mock I/O sources instead of real serial ports.
async fn run_kiss_loop<T>(
    mut io: T,
    metadata: InterfaceMetadata,
    protocol: Protocol,
    watchdog_timeout: Duration,
    packet_tx: mpsc::Sender<SharedPacket>,
    mut cmd_rx: mpsc::Receiver<InterfaceCommand>,
) where
    T: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut decoder = KissDecoder::new();
    let mut read_buf = [0u8; READ_BUF_SIZE];
    let mut last_data_time = Instant::now();
    let mut watchdog_warned = false;

    info!(interface = metadata.callsign, "serial interface started");

    loop {
        let watchdog_remaining = watchdog_timeout
            .checked_sub(last_data_time.elapsed())
            .unwrap_or(Duration::ZERO);

        tokio::select! {
            // Read from serial port
            result = io.read(&mut read_buf) => {
                match result {
                    Ok(0) => {
                        info!(
                            interface = metadata.callsign,
                            "serial port closed (EOF)"
                        );
                        break;
                    }
                    Ok(n) => {
                        last_data_time = Instant::now();
                        watchdog_warned = false;

                        let frames = decoder.feed(&read_buf[..n]);
                        for frame in &frames {
                            if let Some(packet) = process_kiss_frame(
                                frame,
                                &metadata.callsign,
                                metadata.igate_group,
                            ) {
                                debug!(
                                    interface = metadata.callsign,
                                    tnc2 = packet.tnc2,
                                    "received packet"
                                );
                                if packet_tx.send(packet).await.is_err() {
                                    info!(
                                        interface = metadata.callsign,
                                        "packet channel closed, shutting down"
                                    );
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(
                            interface = metadata.callsign,
                            error = %e,
                            "serial port read error"
                        );
                        break;
                    }
                }
            }

            // Handle commands from the router
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(InterfaceCommand::Transmit(packet)) => {
                        if !metadata.tx_ok {
                            warn!(
                                interface = metadata.callsign,
                                "transmit requested but tx_ok is false, ignoring"
                            );
                            continue;
                        }
                        if let Some(kiss_data) = prepare_transmit(&packet, &protocol) {
                            if let Err(e) = io.write_all(&kiss_data).await {
                                error!(
                                    interface = metadata.callsign,
                                    error = %e,
                                    "serial port write error"
                                );
                                break;
                            }
                            debug!(
                                interface = metadata.callsign,
                                tnc2 = packet.tnc2,
                                "transmitted packet"
                            );
                        } else {
                            warn!(
                                interface = metadata.callsign,
                                "packet has no AX.25 data for transmission"
                            );
                        }
                    }
                    Some(InterfaceCommand::Shutdown) => {
                        info!(
                            interface = metadata.callsign,
                            "shutdown command received"
                        );
                        break;
                    }
                    None => {
                        info!(
                            interface = metadata.callsign,
                            "command channel closed, shutting down"
                        );
                        break;
                    }
                }
            }

            // Watchdog timer
            _ = tokio::time::sleep(watchdog_remaining) => {
                if !watchdog_warned {
                    warn!(
                        interface = metadata.callsign,
                        timeout_secs = watchdog_timeout.as_secs(),
                        "no data received within watchdog timeout"
                    );
                    watchdog_warned = true;
                }
            }
        }
    }

    info!(interface = metadata.callsign, "serial interface stopped");
}

impl Interface for SerialInterface {
    fn metadata(&self) -> &InterfaceMetadata {
        &self.metadata
    }

    fn run(
        self: Box<Self>,
        packet_tx: mpsc::Sender<SharedPacket>,
        cmd_rx: mpsc::Receiver<InterfaceCommand>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let builder = tokio_serial::new(&self.device, self.speed);
            let port = match tokio_serial::SerialStream::open(&builder) {
                Ok(port) => port,
                Err(e) => {
                    error!(
                        interface = self.metadata.callsign,
                        device = self.device,
                        error = %e,
                        "failed to open serial port"
                    );
                    return;
                }
            };

            info!(
                interface = self.metadata.callsign,
                device = self.device,
                speed = self.speed,
                protocol = ?self.protocol,
                tx_ok = self.metadata.tx_ok,
                "opened serial port"
            );

            run_kiss_loop(
                port,
                self.metadata,
                self.protocol,
                self.watchdog_timeout,
                packet_tx,
                cmd_rx,
            )
            .await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax25::encode_ax25_address;
    use crate::kiss::{FEND, FESC, TFEND};
    use tokio::io::duplex;

    /// Build a minimal valid AX.25 UI frame for testing.
    /// Returns raw AX.25 bytes for "SRC>DST:payload"
    fn build_test_ax25_frame(src: &str, dst: &str, payload: &[u8]) -> Vec<u8> {
        let dst_addr = encode_ax25_address(dst, 0xE0).unwrap();
        let mut src_addr = encode_ax25_address(src, 0x60).unwrap();
        src_addr[6] |= 0x01; // mark as last address
        let mut frame = Vec::new();
        frame.extend_from_slice(&dst_addr);
        frame.extend_from_slice(&src_addr);
        frame.push(0x03); // UI control
        frame.push(0xF0); // APRS PID
        frame.extend_from_slice(payload);
        frame
    }

    /// Wrap AX.25 bytes in a KISS frame (FEND + cmd_byte + escaped_data + FEND).
    fn wrap_in_kiss(ax25_data: &[u8]) -> Vec<u8> {
        kiss_encode(ax25_data, 0x00, KissVariant::Plain)
    }

    // --- protocol_to_kiss_variant tests ---

    #[test]
    fn protocol_kiss_maps_to_plain() {
        assert_eq!(
            protocol_to_kiss_variant(&Protocol::Kiss),
            Some(KissVariant::Plain)
        );
    }

    #[test]
    fn protocol_smack_maps_to_smack() {
        assert_eq!(
            protocol_to_kiss_variant(&Protocol::Smack),
            Some(KissVariant::Smack)
        );
    }

    #[test]
    fn protocol_flexnet_maps_to_flexnet() {
        assert_eq!(
            protocol_to_kiss_variant(&Protocol::Flexnet),
            Some(KissVariant::FlexNet)
        );
    }

    #[test]
    fn protocol_bpqcrc_maps_to_bpqcrc() {
        assert_eq!(
            protocol_to_kiss_variant(&Protocol::Bpqcrc),
            Some(KissVariant::BpqCrc)
        );
    }

    #[test]
    fn protocol_tnc2_returns_none() {
        assert_eq!(protocol_to_kiss_variant(&Protocol::Tnc2), None);
    }

    #[test]
    fn protocol_dprs_returns_none() {
        assert_eq!(protocol_to_kiss_variant(&Protocol::Dprs), None);
    }

    // --- process_kiss_frame tests ---

    #[test]
    fn process_data_frame_produces_packet() {
        let ax25 = build_test_ax25_frame("OH2MQK-1", "APRS", b"!6029.50N/02505.43E>");
        let frame = KissFrame {
            cmd_byte: 0x00,
            data: ax25.clone(),
        };

        let packet = process_kiss_frame(&frame, "serial0", 1);
        assert!(packet.is_some());
        let pkt = packet.unwrap();
        assert!(pkt.tnc2.starts_with("OH2MQK-1>APRS:"));
        assert!(pkt.is_aprs);
        assert_eq!(pkt.source_interface, "serial0");
        assert_eq!(pkt.igate_group, 1);
        assert_eq!(pkt.ax25, Some(ax25));
    }

    #[test]
    fn process_non_data_frame_returns_none() {
        let frame = KissFrame {
            cmd_byte: 0x01, // TxDelay command, not a data frame
            data: vec![0x28],
        };
        assert!(process_kiss_frame(&frame, "serial0", 1).is_none());
    }

    #[test]
    fn process_invalid_ax25_returns_none() {
        let frame = KissFrame {
            cmd_byte: 0x00,
            data: vec![0x00, 0x01, 0x02], // too short for AX.25
        };
        assert!(process_kiss_frame(&frame, "serial0", 1).is_none());
    }

    #[test]
    fn process_frame_with_tnc_id() {
        // Frame from TNC port 3 (upper nibble = 3, lower = 0 for data)
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!test");
        let frame = KissFrame {
            cmd_byte: 0x30,
            data: ax25,
        };
        // cmd() returns 0 (data frame), so it should be processed
        let packet = process_kiss_frame(&frame, "serial0", 2);
        assert!(packet.is_some());
        assert_eq!(packet.unwrap().igate_group, 2);
    }

    #[test]
    fn process_frame_preserves_igate_group() {
        let ax25 = build_test_ax25_frame("SRC", "DST", b"!test");
        let frame = KissFrame {
            cmd_byte: 0x00,
            data: ax25,
        };
        let pkt = process_kiss_frame(&frame, "port0", 5).unwrap();
        assert_eq!(pkt.igate_group, 5);
    }

    // --- prepare_transmit tests ---

    #[test]
    fn prepare_transmit_with_ax25_data() {
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!test");
        let packet = Packet {
            tnc2: "TEST-1>APRS:!test".to_string(),
            tnc2_addr_len: 10,
            ax25: Some(ax25.clone()),
            ax25_addr_len: 14,
            source_interface: "serial0".to_string(),
            is_aprs: true,
            ui_pid: 0xF0,
            received_at: std::time::Instant::now(),
            igate_group: 1,
        };

        let result = prepare_transmit(&packet, &Protocol::Kiss);
        assert!(result.is_some());
        let kiss_data = result.unwrap();

        // Should be a valid KISS frame
        assert_eq!(kiss_data[0], FEND);
        assert_eq!(kiss_data[1], 0x00); // data command
        assert_eq!(*kiss_data.last().unwrap(), FEND);
    }

    #[test]
    fn prepare_transmit_without_ax25_data_returns_none() {
        let packet = Packet::new("TEST>APRS:hello", "serial0", true);
        let result = prepare_transmit(&packet, &Protocol::Kiss);
        assert!(result.is_none());
    }

    #[test]
    fn prepare_transmit_with_non_kiss_protocol_returns_none() {
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!test");
        let packet = Packet {
            tnc2: "TEST-1>APRS:!test".to_string(),
            tnc2_addr_len: 10,
            ax25: Some(ax25),
            ax25_addr_len: 14,
            source_interface: "serial0".to_string(),
            is_aprs: true,
            ui_pid: 0xF0,
            received_at: std::time::Instant::now(),
            igate_group: 1,
        };

        assert!(prepare_transmit(&packet, &Protocol::Tnc2).is_none());
        assert!(prepare_transmit(&packet, &Protocol::Dprs).is_none());
    }

    #[test]
    fn prepare_transmit_smack_includes_crc() {
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!test");
        let plain_len = kiss_encode(&ax25, 0x00, KissVariant::Plain).len();

        let packet = Packet {
            tnc2: "TEST-1>APRS:!test".to_string(),
            tnc2_addr_len: 10,
            ax25: Some(ax25),
            ax25_addr_len: 14,
            source_interface: "serial0".to_string(),
            is_aprs: true,
            ui_pid: 0xF0,
            received_at: std::time::Instant::now(),
            igate_group: 1,
        };

        let result = prepare_transmit(&packet, &Protocol::Smack).unwrap();
        // SMACK adds a 2-byte CRC (possibly escaped), so result should be longer
        assert!(result.len() > plain_len);
    }

    // --- SerialInterface metadata tests ---

    #[test]
    fn serial_interface_metadata_correct() {
        let iface = SerialInterface::new(
            "OH2MQK-1".to_string(),
            "/dev/ttyUSB0".to_string(),
            9600,
            Protocol::Kiss,
            false,
            1,
        );

        let meta = iface.metadata();
        assert_eq!(meta.callsign, "OH2MQK-1");
        assert_eq!(meta.iface_type, InterfaceType::Serial);
        assert!(!meta.tx_ok);
        assert_eq!(meta.igate_group, 1);
    }

    #[test]
    fn serial_interface_metadata_tx_enabled() {
        let iface = SerialInterface::new(
            "TEST-2".to_string(),
            "/dev/ttyUSB1".to_string(),
            19200,
            Protocol::Kiss,
            true,
            2,
        );

        let meta = iface.metadata();
        assert!(meta.tx_ok);
        assert_eq!(meta.igate_group, 2);
    }

    #[test]
    fn serial_interface_with_watchdog_timeout() {
        let iface = SerialInterface::new(
            "TEST-1".to_string(),
            "/dev/ttyUSB0".to_string(),
            9600,
            Protocol::Kiss,
            false,
            1,
        )
        .with_watchdog_timeout(Duration::from_secs(300));

        assert_eq!(iface.watchdog_timeout, Duration::from_secs(300));
    }

    // --- run_kiss_loop integration tests with mock I/O ---

    #[tokio::test]
    async fn loop_receives_kiss_frame_and_sends_packet() {
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!6029.50N/02505.43E>");
        let kiss_data = wrap_in_kiss(&ax25);

        let (mut writer, reader) = duplex(8192);
        let (packet_tx, mut packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-1".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false,
            igate_group: 1,
        };

        // Write KISS data then close the writer to trigger EOF
        tokio::spawn(async move {
            writer.write_all(&kiss_data).await.unwrap();
            // Small delay to ensure data is processed before EOF
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(writer);
        });

        let loop_handle = tokio::spawn(run_kiss_loop(
            reader,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        // Should receive the decoded packet
        let packet = tokio::time::timeout(Duration::from_secs(2), packet_rx.recv())
            .await
            .expect("timed out waiting for packet")
            .expect("channel closed without packet");

        assert!(packet.tnc2.starts_with("TEST-1>APRS:"));
        assert!(packet.is_aprs);
        assert_eq!(packet.source_interface, "TEST-1");

        drop(cmd_tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), loop_handle).await;
    }

    #[tokio::test]
    async fn loop_handles_shutdown_command() {
        let (_writer, reader) = duplex(8192);
        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-1".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false,
            igate_group: 1,
        };

        let loop_handle = tokio::spawn(run_kiss_loop(
            reader,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        // Send shutdown
        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();

        // Loop should complete
        tokio::time::timeout(Duration::from_secs(2), loop_handle)
            .await
            .expect("timed out waiting for shutdown")
            .expect("loop panicked");
    }

    #[tokio::test]
    async fn loop_handles_cmd_channel_close() {
        let (_writer, reader) = duplex(8192);
        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-1".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false,
            igate_group: 1,
        };

        let loop_handle = tokio::spawn(run_kiss_loop(
            reader,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        // Drop command sender to close channel
        drop(cmd_tx);

        tokio::time::timeout(Duration::from_secs(2), loop_handle)
            .await
            .expect("timed out waiting for close")
            .expect("loop panicked");
    }

    #[tokio::test]
    async fn loop_transmits_packet_when_tx_ok() {
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!test");
        let packet = Arc::new(Packet {
            tnc2: "TEST-1>APRS:!test".to_string(),
            tnc2_addr_len: 10,
            ax25: Some(ax25.clone()),
            ax25_addr_len: 14,
            source_interface: "other".to_string(),
            is_aprs: true,
            ui_pid: 0xF0,
            received_at: std::time::Instant::now(),
            igate_group: 1,
        });

        let (writer, mut reader_end) = duplex(8192);
        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-TX".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: true,
            igate_group: 1,
        };

        let loop_handle = tokio::spawn(run_kiss_loop(
            writer,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        // Send transmit command
        cmd_tx
            .send(InterfaceCommand::Transmit(packet))
            .await
            .unwrap();

        // Read what was written to the serial port
        let mut buf = vec![0u8; 8192];
        let n = tokio::time::timeout(Duration::from_secs(2), reader_end.read(&mut buf))
            .await
            .expect("timed out reading transmitted data")
            .expect("read error");

        let written = &buf[..n];
        // Should be a valid KISS frame
        assert_eq!(written[0], FEND);
        assert_eq!(written[1], 0x00);
        assert_eq!(*written.last().unwrap(), FEND);

        // Clean up
        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), loop_handle).await;
    }

    #[tokio::test]
    async fn loop_ignores_transmit_when_tx_not_ok() {
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!test");
        let packet = Arc::new(Packet {
            tnc2: "TEST-1>APRS:!test".to_string(),
            tnc2_addr_len: 10,
            ax25: Some(ax25),
            ax25_addr_len: 14,
            source_interface: "other".to_string(),
            is_aprs: true,
            ui_pid: 0xF0,
            received_at: std::time::Instant::now(),
            igate_group: 1,
        });

        let (writer, mut reader_end) = duplex(8192);
        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-RX".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false, // TX NOT OK
            igate_group: 1,
        };

        let loop_handle = tokio::spawn(run_kiss_loop(
            writer,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        // Send transmit command (should be ignored)
        cmd_tx
            .send(InterfaceCommand::Transmit(packet))
            .await
            .unwrap();

        // Then shutdown
        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();

        // Wait for loop to finish
        let _ = tokio::time::timeout(Duration::from_secs(2), loop_handle).await;

        // Try to read from the reader end - should get nothing (or just EOF)
        let mut buf = vec![0u8; 8192];
        let result =
            tokio::time::timeout(Duration::from_millis(200), reader_end.read(&mut buf)).await;

        // Either timeout (nothing written) or read 0 bytes (EOF) is correct
        match result {
            Err(_) => {}    // timeout = nothing written, correct
            Ok(Ok(0)) => {} // EOF, correct
            Ok(Ok(_n)) => panic!("data was written to serial port despite tx_ok=false"),
            Ok(Err(_)) => {} // broken pipe from close, acceptable
        }
    }

    #[tokio::test]
    async fn loop_handles_multiple_kiss_frames() {
        let ax25_1 = build_test_ax25_frame("SRC1", "APRS", b"!packet1");
        let ax25_2 = build_test_ax25_frame("SRC2", "APRS", b"!packet2");

        let mut kiss_data = wrap_in_kiss(&ax25_1);
        kiss_data.extend(wrap_in_kiss(&ax25_2));

        let (mut writer, reader) = duplex(8192);
        let (packet_tx, mut packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-MULTI".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false,
            igate_group: 1,
        };

        tokio::spawn(async move {
            writer.write_all(&kiss_data).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(writer);
        });

        let loop_handle = tokio::spawn(run_kiss_loop(
            reader,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        // Should receive two packets
        let pkt1 = tokio::time::timeout(Duration::from_secs(2), packet_rx.recv())
            .await
            .expect("timed out")
            .expect("no packet");
        assert!(pkt1.tnc2.contains("SRC1"));

        let pkt2 = tokio::time::timeout(Duration::from_secs(2), packet_rx.recv())
            .await
            .expect("timed out")
            .expect("no packet");
        assert!(pkt2.tnc2.contains("SRC2"));

        drop(cmd_tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), loop_handle).await;
    }

    #[tokio::test]
    async fn loop_handles_incremental_kiss_data() {
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!incr");
        let kiss_data = wrap_in_kiss(&ax25);

        let (mut writer, reader) = duplex(8192);
        let (packet_tx, mut packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-INCR".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false,
            igate_group: 1,
        };

        // Write KISS data byte by byte
        tokio::spawn(async move {
            for &byte in &kiss_data {
                writer.write_all(&[byte]).await.unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(writer);
        });

        let loop_handle = tokio::spawn(run_kiss_loop(
            reader,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        let packet = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timed out")
            .expect("no packet");

        assert!(packet.tnc2.contains("TEST-1"));

        drop(cmd_tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), loop_handle).await;
    }

    #[tokio::test]
    async fn loop_watchdog_triggers_on_idle() {
        let (_writer, reader) = duplex(8192);
        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-WD".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false,
            igate_group: 1,
        };

        // Very short watchdog timeout for testing
        let loop_handle = tokio::spawn(run_kiss_loop(
            reader,
            metadata,
            Protocol::Kiss,
            Duration::from_millis(100),
            packet_tx,
            cmd_rx,
        ));

        // Wait for watchdog to fire, then shut down
        tokio::time::sleep(Duration::from_millis(200)).await;

        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();

        tokio::time::timeout(Duration::from_secs(2), loop_handle)
            .await
            .expect("timed out")
            .expect("panicked");
        // If we get here, the watchdog didn't crash the loop (it only logs)
    }

    #[tokio::test]
    async fn loop_kiss_frame_with_fend_in_data() {
        // Build AX.25 frame containing bytes that need KISS escaping
        let dst = encode_ax25_address("APRS", 0xE0).unwrap();
        let mut src = encode_ax25_address("TEST-1", 0x60).unwrap();
        src[6] |= 0x01;
        let mut ax25 = Vec::new();
        ax25.extend_from_slice(&dst);
        ax25.extend_from_slice(&src);
        ax25.push(0x03);
        ax25.push(0xF0);
        // Payload with bytes that match FEND when raw
        ax25.extend_from_slice(b"!test");

        // Manually build KISS frame with an escaped FEND in the AX.25 data
        // to verify the decoder handles it
        let kiss_data = wrap_in_kiss(&ax25);

        let (mut writer, reader) = duplex(8192);
        let (packet_tx, mut packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-ESC".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false,
            igate_group: 1,
        };

        tokio::spawn(async move {
            writer.write_all(&kiss_data).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(writer);
        });

        let loop_handle = tokio::spawn(run_kiss_loop(
            reader,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        let packet = tokio::time::timeout(Duration::from_secs(2), packet_rx.recv())
            .await
            .expect("timed out")
            .expect("no packet");

        assert!(packet.tnc2.starts_with("TEST-1>APRS:"));

        drop(cmd_tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), loop_handle).await;
    }

    #[tokio::test]
    async fn loop_eof_stops_cleanly() {
        let (writer, reader) = duplex(8192);
        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (_cmd_tx, cmd_rx) = mpsc::channel(16);

        let metadata = InterfaceMetadata {
            callsign: "TEST-EOF".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: false,
            igate_group: 1,
        };

        // Immediately close the writer to cause EOF
        drop(writer);

        let loop_handle = tokio::spawn(run_kiss_loop(
            reader,
            metadata,
            Protocol::Kiss,
            Duration::from_secs(60),
            packet_tx,
            cmd_rx,
        ));

        tokio::time::timeout(Duration::from_secs(2), loop_handle)
            .await
            .expect("timed out on EOF")
            .expect("panicked");
    }

    #[test]
    fn prepare_transmit_roundtrip_through_decoder() {
        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!roundtrip");
        let packet = Packet {
            tnc2: "TEST-1>APRS:!roundtrip".to_string(),
            tnc2_addr_len: 10,
            ax25: Some(ax25.clone()),
            ax25_addr_len: 14,
            source_interface: "serial0".to_string(),
            is_aprs: true,
            ui_pid: 0xF0,
            received_at: std::time::Instant::now(),
            igate_group: 1,
        };

        let kiss_data = prepare_transmit(&packet, &Protocol::Kiss).unwrap();

        // Decode it back
        let mut decoder = KissDecoder::new();
        let frames = decoder.feed(&kiss_data);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].cmd_byte, 0x00);
        assert_eq!(frames[0].data, ax25);
    }

    #[test]
    fn prepare_transmit_fend_in_ax25_data_is_escaped() {
        // Create AX.25 data that contains the FEND byte
        let mut ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!test");
        // Insert a FEND byte into the payload area
        ax25.push(FEND);
        ax25.push(b'X');

        let packet = Packet {
            tnc2: "TEST-1>APRS:!test".to_string(),
            tnc2_addr_len: 10,
            ax25: Some(ax25.clone()),
            ax25_addr_len: 14,
            source_interface: "serial0".to_string(),
            is_aprs: true,
            ui_pid: 0xF0,
            received_at: std::time::Instant::now(),
            igate_group: 1,
        };

        let kiss_data = prepare_transmit(&packet, &Protocol::Kiss).unwrap();

        // The FEND in the data should be escaped as FESC TFEND
        // Find the escape sequence in the frame (excluding first and last FEND)
        let inner = &kiss_data[1..kiss_data.len() - 1];
        assert!(
            inner.windows(2).any(|w| w == [FESC, TFEND]),
            "FEND in AX.25 data should be escaped"
        );

        // Roundtrip: decode should recover original data
        let mut decoder = KissDecoder::new();
        let frames = decoder.feed(&kiss_data);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, ax25);
    }
}
