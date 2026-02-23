// KISS TCP interface
//
// Connects to a remote TNC over TCP, reads KISS frames, converts to packets
// via the shared run_kiss_loop(), and forwards to the router. Includes
// auto-reconnect with exponential backoff on disconnect or connect failure.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{error, info, warn};

use crate::config::{InterfaceType, Protocol};
use crate::packet::SharedPacket;

use super::serial::{run_kiss_loop, KissLoopExit};
use super::{Interface, InterfaceCommand, InterfaceMetadata};

/// Default watchdog timeout in seconds (log warning if no data received).
const DEFAULT_WATCHDOG_TIMEOUT_SECS: u64 = 600;

/// Default TCP connect timeout in seconds.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Initial backoff delay on connection failure.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Maximum backoff delay between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// KISS TCP interface.
///
/// Connects to a remote TNC over TCP, reads KISS frames, and decodes them
/// to APRS packets. Automatically reconnects with exponential backoff on
/// disconnect or connection failure.
pub struct TcpKissInterface {
    metadata: InterfaceMetadata,
    host: String,
    port: u16,
    protocol: Protocol,
    watchdog_timeout: Duration,
    connect_timeout: Duration,
}

impl TcpKissInterface {
    /// Create a new TCP KISS interface.
    ///
    /// # Arguments
    /// * `callsign` - Station callsign for this interface
    /// * `host` - Remote TNC hostname or IP address
    /// * `port` - Remote TNC TCP port
    /// * `protocol` - KISS protocol variant
    /// * `tx_ok` - Whether transmitting is allowed
    /// * `igate_group` - iGate group number for routing
    pub fn new(
        callsign: String,
        host: String,
        port: u16,
        protocol: Protocol,
        tx_ok: bool,
        igate_group: u8,
    ) -> Self {
        Self {
            metadata: InterfaceMetadata {
                callsign,
                iface_type: InterfaceType::Tcp,
                tx_ok,
                igate_group,
            },
            host,
            port,
            protocol,
            watchdog_timeout: Duration::from_secs(DEFAULT_WATCHDOG_TIMEOUT_SECS),
            connect_timeout: Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS),
        }
    }

    /// Set the watchdog timeout duration.
    ///
    /// If no data is received for this duration, a warning is logged.
    pub fn with_watchdog_timeout(mut self, timeout: Duration) -> Self {
        self.watchdog_timeout = timeout;
        self
    }

    /// Set the TCP connect timeout duration.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
}

impl Interface for TcpKissInterface {
    fn metadata(&self) -> &InterfaceMetadata {
        &self.metadata
    }

    fn run(
        self: Box<Self>,
        packet_tx: mpsc::Sender<SharedPacket>,
        mut cmd_rx: mpsc::Receiver<InterfaceCommand>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let addr = format!("{}:{}", self.host, self.port);
            let mut backoff = INITIAL_BACKOFF;

            info!(
                interface = self.metadata.callsign,
                host = self.host,
                port = self.port,
                protocol = ?self.protocol,
                tx_ok = self.metadata.tx_ok,
                "TCP KISS interface starting"
            );

            loop {
                // Check for shutdown command before attempting connect
                match cmd_rx.try_recv() {
                    Ok(InterfaceCommand::Shutdown) => {
                        info!(
                            interface = self.metadata.callsign,
                            "shutdown received before connect"
                        );
                        break;
                    }
                    Ok(InterfaceCommand::Transmit(_)) => {
                        warn!(
                            interface = self.metadata.callsign,
                            "transmit while disconnected, dropping"
                        );
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        info!(interface = self.metadata.callsign, "command channel closed");
                        break;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {}
                }

                // Attempt to connect with timeout
                info!(
                    interface = self.metadata.callsign,
                    addr = addr,
                    "connecting"
                );

                let connect_result = timeout(self.connect_timeout, TcpStream::connect(&addr)).await;

                match connect_result {
                    Ok(Ok(stream)) => {
                        info!(interface = self.metadata.callsign, addr = addr, "connected");

                        // Reset backoff on successful connect
                        backoff = INITIAL_BACKOFF;

                        // Run the shared KISS event loop
                        let exit_reason = run_kiss_loop(
                            stream,
                            &self.metadata,
                            &self.protocol,
                            self.watchdog_timeout,
                            &packet_tx,
                            &mut cmd_rx,
                        )
                        .await;

                        // Decide whether to reconnect based on exit reason
                        match exit_reason {
                            KissLoopExit::Shutdown
                            | KissLoopExit::ChannelClosed
                            | KissLoopExit::PacketChannelClosed => {
                                // Intentional shutdown or channel gone - stop
                                break;
                            }
                            KissLoopExit::Eof
                            | KissLoopExit::IoError
                            | KissLoopExit::WriteError => {
                                // Connection lost - reconnect
                                warn!(
                                    interface = self.metadata.callsign,
                                    reason = ?exit_reason,
                                    "disconnected, will reconnect"
                                );
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error!(
                            interface = self.metadata.callsign,
                            addr = addr,
                            error = %e,
                            "TCP connect failed"
                        );
                    }
                    Err(_) => {
                        error!(
                            interface = self.metadata.callsign,
                            addr = addr,
                            timeout_secs = self.connect_timeout.as_secs(),
                            "TCP connect timed out"
                        );
                    }
                }

                // Wait with backoff before reconnecting, but listen for shutdown
                info!(
                    interface = self.metadata.callsign,
                    backoff_secs = backoff.as_secs_f64(),
                    "waiting before reconnect"
                );

                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(InterfaceCommand::Shutdown) | None => {
                                info!(
                                    interface = self.metadata.callsign,
                                    "shutdown during reconnect backoff"
                                );
                                break;
                            }
                            Some(InterfaceCommand::Transmit(_)) => {
                                warn!(
                                    interface = self.metadata.callsign,
                                    "transmit while disconnected, dropping"
                                );
                            }
                        }
                    }
                }

                // Exponential backoff: double up to MAX_BACKOFF
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }

            info!(
                interface = self.metadata.callsign,
                "TCP KISS interface stopped"
            );
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax25::encode_ax25_address;
    use crate::kiss::{kiss_encode, KissVariant, FEND};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Build a minimal valid AX.25 UI frame for testing.
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

    /// Wrap AX.25 bytes in a KISS frame.
    fn wrap_in_kiss(ax25_data: &[u8]) -> Vec<u8> {
        kiss_encode(ax25_data, 0x00, KissVariant::Plain)
    }

    /// Bind a TCP listener on a random available port and return (listener, port).
    async fn test_listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    // --- Metadata tests ---

    #[test]
    fn tcp_interface_metadata_correct() {
        let iface = TcpKissInterface::new(
            "OH2MQK-1".to_string(),
            "192.168.1.100".to_string(),
            10001,
            Protocol::Kiss,
            false,
            1,
        );

        let meta = iface.metadata();
        assert_eq!(meta.callsign, "OH2MQK-1");
        assert_eq!(meta.iface_type, InterfaceType::Tcp);
        assert!(!meta.tx_ok);
        assert_eq!(meta.igate_group, 1);
    }

    #[test]
    fn tcp_interface_metadata_tx_enabled() {
        let iface = TcpKissInterface::new(
            "TEST-2".to_string(),
            "10.0.0.1".to_string(),
            8001,
            Protocol::Smack,
            true,
            3,
        );

        let meta = iface.metadata();
        assert!(meta.tx_ok);
        assert_eq!(meta.igate_group, 3);
        assert_eq!(meta.iface_type, InterfaceType::Tcp);
    }

    #[test]
    fn tcp_interface_with_watchdog_timeout() {
        let iface = TcpKissInterface::new(
            "TEST-1".to_string(),
            "localhost".to_string(),
            10001,
            Protocol::Kiss,
            false,
            1,
        )
        .with_watchdog_timeout(Duration::from_secs(300));

        assert_eq!(iface.watchdog_timeout, Duration::from_secs(300));
    }

    #[test]
    fn tcp_interface_with_connect_timeout() {
        let iface = TcpKissInterface::new(
            "TEST-1".to_string(),
            "localhost".to_string(),
            10001,
            Protocol::Kiss,
            false,
            1,
        )
        .with_connect_timeout(Duration::from_secs(10));

        assert_eq!(iface.connect_timeout, Duration::from_secs(10));
    }

    #[test]
    fn tcp_interface_default_timeouts() {
        let iface = TcpKissInterface::new(
            "TEST-1".to_string(),
            "localhost".to_string(),
            10001,
            Protocol::Kiss,
            false,
            1,
        );

        assert_eq!(
            iface.watchdog_timeout,
            Duration::from_secs(DEFAULT_WATCHDOG_TIMEOUT_SECS)
        );
        assert_eq!(
            iface.connect_timeout,
            Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS)
        );
    }

    // --- Connect and process tests ---

    #[tokio::test]
    async fn connect_and_receive_kiss_frame() {
        let (listener, port) = test_listener().await;

        let iface = Box::new(
            TcpKissInterface::new(
                "TCP-TEST".to_string(),
                "127.0.0.1".to_string(),
                port,
                Protocol::Kiss,
                false,
                1,
            )
            .with_connect_timeout(Duration::from_secs(5)),
        );

        let (packet_tx, mut packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_handle = tokio::spawn(iface.run(packet_tx, cmd_rx));

        // Accept the connection and send a KISS frame
        let (mut stream, _) = listener.accept().await.unwrap();
        let ax25 = build_test_ax25_frame("SRC-1", "APRS", b"!6029.50N/02505.43E>");
        let kiss_data = wrap_in_kiss(&ax25);
        stream.write_all(&kiss_data).await.unwrap();

        // Should receive the decoded packet
        let packet = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timed out waiting for packet")
            .expect("channel closed without packet");

        assert!(packet.tnc2.starts_with("SRC-1>APRS:"));
        assert!(packet.is_aprs);
        assert_eq!(packet.source_interface, "TCP-TEST");
        assert_eq!(packet.igate_group, 1);

        // Clean up
        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
    }

    #[tokio::test]
    async fn connect_and_receive_multiple_frames() {
        let (listener, port) = test_listener().await;

        let iface = Box::new(
            TcpKissInterface::new(
                "TCP-MULTI".to_string(),
                "127.0.0.1".to_string(),
                port,
                Protocol::Kiss,
                false,
                1,
            )
            .with_connect_timeout(Duration::from_secs(5)),
        );

        let (packet_tx, mut packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_handle = tokio::spawn(iface.run(packet_tx, cmd_rx));

        let (mut stream, _) = listener.accept().await.unwrap();

        let ax25_1 = build_test_ax25_frame("SRC1", "APRS", b"!packet1");
        let ax25_2 = build_test_ax25_frame("SRC2", "APRS", b"!packet2");
        let mut data = wrap_in_kiss(&ax25_1);
        data.extend(wrap_in_kiss(&ax25_2));
        stream.write_all(&data).await.unwrap();

        let pkt1 = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timed out")
            .expect("no packet");
        assert!(pkt1.tnc2.contains("SRC1"));

        let pkt2 = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timed out")
            .expect("no packet");
        assert!(pkt2.tnc2.contains("SRC2"));

        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
    }

    #[tokio::test]
    async fn transmit_packet_over_tcp() {
        let (listener, port) = test_listener().await;

        let iface = Box::new(
            TcpKissInterface::new(
                "TCP-TX".to_string(),
                "127.0.0.1".to_string(),
                port,
                Protocol::Kiss,
                true,
                1,
            )
            .with_connect_timeout(Duration::from_secs(5)),
        );

        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_handle = tokio::spawn(iface.run(packet_tx, cmd_rx));

        let (mut stream, _) = listener.accept().await.unwrap();

        // Give the interface a moment to start its event loop
        tokio::time::sleep(Duration::from_millis(50)).await;

        let ax25 = build_test_ax25_frame("TEST-1", "APRS", b"!test");
        let packet = Arc::new(crate::packet::Packet {
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

        cmd_tx
            .send(InterfaceCommand::Transmit(packet))
            .await
            .unwrap();

        // Read the transmitted data from the server side
        let mut buf = vec![0u8; 8192];
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
            .await
            .expect("timed out reading")
            .expect("read error");

        let written = &buf[..n];
        assert_eq!(written[0], FEND);
        assert_eq!(written[1], 0x00);
        assert_eq!(*written.last().unwrap(), FEND);

        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
    }

    // --- Reconnect behavior tests ---

    #[tokio::test]
    async fn reconnects_after_server_closes_connection() {
        let (listener, port) = test_listener().await;

        let iface = Box::new(
            TcpKissInterface::new(
                "TCP-RECONN".to_string(),
                "127.0.0.1".to_string(),
                port,
                Protocol::Kiss,
                false,
                1,
            )
            .with_connect_timeout(Duration::from_secs(5)),
        );

        let (packet_tx, mut packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_handle = tokio::spawn(iface.run(packet_tx, cmd_rx));

        // First connection: send a frame then close
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            let ax25 = build_test_ax25_frame("CONN1", "APRS", b"!first");
            stream.write_all(&wrap_in_kiss(&ax25)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(stream); // close connection
        }

        let pkt1 = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timed out")
            .expect("no packet");
        assert!(pkt1.tnc2.contains("CONN1"));

        // Second connection: interface should reconnect
        {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .expect("timed out waiting for reconnect")
                .unwrap();

            let ax25 = build_test_ax25_frame("CONN2", "APRS", b"!second");
            stream.write_all(&wrap_in_kiss(&ax25)).await.unwrap();
        }

        let pkt2 = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timed out")
            .expect("no packet");
        assert!(pkt2.tnc2.contains("CONN2"));

        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), run_handle).await;
    }

    // --- Shutdown tests ---

    #[tokio::test]
    async fn shutdown_during_backoff() {
        // Don't start a listener so connect will fail, triggering backoff
        // Use a port that will refuse connection
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // close immediately so connect fails

        let iface = Box::new(
            TcpKissInterface::new(
                "TCP-SHUT".to_string(),
                "127.0.0.1".to_string(),
                port,
                Protocol::Kiss,
                false,
                1,
            )
            .with_connect_timeout(Duration::from_secs(1)),
        );

        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_handle = tokio::spawn(iface.run(packet_tx, cmd_rx));

        // Wait a bit for the connect attempt to fail and enter backoff
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Send shutdown during backoff
        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();

        // Should exit cleanly
        tokio::time::timeout(Duration::from_secs(5), run_handle)
            .await
            .expect("timed out waiting for shutdown")
            .expect("task panicked");
    }

    #[tokio::test]
    async fn shutdown_while_connected() {
        let (listener, port) = test_listener().await;

        let iface = Box::new(
            TcpKissInterface::new(
                "TCP-SHUTCONN".to_string(),
                "127.0.0.1".to_string(),
                port,
                Protocol::Kiss,
                false,
                1,
            )
            .with_connect_timeout(Duration::from_secs(5)),
        );

        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_handle = tokio::spawn(iface.run(packet_tx, cmd_rx));

        // Accept the connection
        let (_stream, _) = listener.accept().await.unwrap();

        // Give a moment for the event loop to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send shutdown
        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();

        // Should exit cleanly
        tokio::time::timeout(Duration::from_secs(5), run_handle)
            .await
            .expect("timed out waiting for shutdown")
            .expect("task panicked");
    }

    #[tokio::test]
    async fn cmd_channel_close_stops_interface() {
        let (listener, port) = test_listener().await;

        let iface = Box::new(
            TcpKissInterface::new(
                "TCP-CHANCLOSE".to_string(),
                "127.0.0.1".to_string(),
                port,
                Protocol::Kiss,
                false,
                1,
            )
            .with_connect_timeout(Duration::from_secs(5)),
        );

        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_handle = tokio::spawn(iface.run(packet_tx, cmd_rx));

        // Accept the connection
        let (_stream, _) = listener.accept().await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop the command sender
        drop(cmd_tx);

        // Should exit cleanly
        tokio::time::timeout(Duration::from_secs(5), run_handle)
            .await
            .expect("timed out waiting for close")
            .expect("task panicked");
    }

    // --- Connect timeout test ---

    #[tokio::test]
    async fn connect_timeout_triggers_backoff() {
        // Use a non-routable IP to force a connect timeout
        // 192.0.2.1 is TEST-NET-1, should timeout rather than refuse
        let iface = Box::new(
            TcpKissInterface::new(
                "TCP-TIMEOUT".to_string(),
                "192.0.2.1".to_string(),
                10001,
                Protocol::Kiss,
                false,
                1,
            )
            .with_connect_timeout(Duration::from_millis(500)),
        );

        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_handle = tokio::spawn(iface.run(packet_tx, cmd_rx));

        // Wait for the connect timeout to fire
        tokio::time::sleep(Duration::from_millis(800)).await;

        // Send shutdown - should be picked up during backoff
        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();

        tokio::time::timeout(Duration::from_secs(5), run_handle)
            .await
            .expect("timed out waiting for shutdown after connect timeout")
            .expect("task panicked");
    }

    // --- Trait object test ---

    #[test]
    fn tcp_interface_implements_interface_trait() {
        let iface: Box<dyn Interface> = Box::new(TcpKissInterface::new(
            "TCP-TRAIT".to_string(),
            "localhost".to_string(),
            10001,
            Protocol::Kiss,
            false,
            1,
        ));

        assert_eq!(iface.metadata().callsign, "TCP-TRAIT");
        assert_eq!(iface.metadata().iface_type, InterfaceType::Tcp);
    }
}
