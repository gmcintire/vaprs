// APRS-IS client - connects to APRS-IS servers, handles authentication,
// heartbeat monitoring, and bidirectional packet flow.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{self, timeout};
use tracing::{debug, error, info, warn};

use crate::config::AprsIsConfig;
use crate::packet::{Packet, SharedPacket};

/// Default heartbeat timeout in seconds. APRS-IS servers send keepalives
/// roughly every 20-30 seconds; 120s gives plenty of margin.
const DEFAULT_HEARTBEAT_TIMEOUT_SECS: u64 = 120;

/// Backoff delay between reconnection attempts.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(10);

/// Timeout for TCP connect attempts.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of packets to drain from write queue in one batch.
const WRITE_BATCH_MAX: usize = 20;

/// Calculate the APRS-IS passcode for a callsign.
///
/// Uses the well-known hash algorithm. Only the base callsign (without SSID)
/// is used. Returns a value in the range 0..=32767.
pub fn aprs_passcode(callsign: &str) -> i16 {
    let call = callsign.split('-').next().unwrap_or(callsign);
    let call_upper = call.to_uppercase();
    let bytes = call_upper.as_bytes();
    let mut hash: i16 = 0x73e2u16 as i16;
    let mut i = 0;
    while i + 1 < bytes.len() {
        hash ^= (bytes[i] as i16) << 8;
        hash ^= bytes[i + 1] as i16;
        i += 2;
    }
    if i < bytes.len() {
        hash ^= (bytes[i] as i16) << 8;
    }
    hash & 0x7FFF
}

/// APRS-IS client that maintains a connection to an APRS-IS server.
pub struct AprsIsClient {
    /// Callsign used for login.
    login: String,
    /// Passcode for authentication (-1 for receive-only).
    passcode: i32,
    /// List of servers to connect to (host:port format).
    servers: Vec<String>,
    /// Optional server-side filter string.
    filter: Option<String>,
    /// How long to wait without server data before considering connection dead.
    heartbeat_timeout: Duration,
}

impl AprsIsClient {
    /// Create a new APRS-IS client from the station callsign and APRS-IS config.
    pub fn new(mycall: &str, config: &AprsIsConfig) -> Self {
        let heartbeat_secs = config
            .heartbeat_timeout
            .unwrap_or(DEFAULT_HEARTBEAT_TIMEOUT_SECS);

        Self {
            login: mycall.to_string(),
            passcode: config.passcode,
            servers: config.servers.clone(),
            filter: config.filter.clone(),
            heartbeat_timeout: Duration::from_secs(heartbeat_secs),
        }
    }

    /// Format the APRS-IS login line.
    ///
    /// Format: `user CALL pass PASSCODE vers vaprs 0.1.0[ filter FILTER]\r\n`
    pub fn login_line(&self) -> String {
        let mut line = format!(
            "user {} pass {} vers vaprs 0.1.0",
            self.login, self.passcode
        );
        if let Some(ref f) = self.filter {
            line.push_str(&format!(" filter {}", f));
        }
        line.push_str("\r\n");
        line
    }

    /// Run the APRS-IS client task.
    ///
    /// Maintains a persistent connection to APRS-IS, reconnecting on failure.
    /// Received packets are sent to `packet_tx`. Packets to transmit are read
    /// from `write_rx`.
    ///
    /// The task runs until `packet_tx` is closed (receiver dropped) or
    /// `write_rx` is closed and the connection drops.
    pub async fn run(
        self,
        packet_tx: mpsc::Sender<SharedPacket>,
        mut write_rx: mpsc::Receiver<String>,
    ) {
        if self.servers.is_empty() {
            error!("APRS-IS: no servers configured");
            return;
        }

        let mut server_idx = 0;
        let mut pending_writes: Vec<String> = Vec::new();

        loop {
            let server = &self.servers[server_idx];
            info!("APRS-IS: connecting to {}", server);

            match self
                .connect_and_run(server, &packet_tx, &mut write_rx, &mut pending_writes)
                .await
            {
                ConnectionResult::Shutdown => {
                    info!("APRS-IS: shutting down");
                    return;
                }
                ConnectionResult::Disconnected(reason) => {
                    warn!("APRS-IS: disconnected from {}: {}", server, reason);
                    // Rotate to next server on failure
                    server_idx = (server_idx + 1) % self.servers.len();
                }
            }

            info!("APRS-IS: reconnecting in {:?}", RECONNECT_BACKOFF);
            // Sleep during backoff, but detect shutdown (channel close) promptly.
            // If we receive a message during backoff, push it to a pending buffer
            // so it can be sent on the next connection - don't drop queued data.
            let shutdown = loop {
                tokio::select! {
                    _ = time::sleep(RECONNECT_BACKOFF) => {
                        break false;
                    }
                    msg = write_rx.recv() => {
                        match msg {
                            Some(data) => {
                                // Buffer the message for the next connection
                                pending_writes.push(data);
                            }
                            None => {
                                // Channel closed => shutdown
                                info!("APRS-IS: write channel closed during backoff, shutting down");
                                break true;
                            }
                        }
                    }
                }
            };
            if shutdown {
                return;
            }
        }
    }

    /// Connect to a single server and run until disconnect or shutdown.
    async fn connect_and_run(
        &self,
        server: &str,
        packet_tx: &mpsc::Sender<SharedPacket>,
        write_rx: &mut mpsc::Receiver<String>,
        pending_writes: &mut Vec<String>,
    ) -> ConnectionResult {
        // TCP connect with timeout
        let stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(server)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                return ConnectionResult::Disconnected(format!("connect failed: {}", e));
            }
            Err(_) => {
                return ConnectionResult::Disconnected("connect timeout".to_string());
            }
        };

        info!("APRS-IS: connected to {}", server);

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        // Send login line
        let login = self.login_line();
        debug!("APRS-IS: sending login: {}", login.trim());
        if let Err(e) = writer.write_all(login.as_bytes()).await {
            return ConnectionResult::Disconnected(format!("login write failed: {}", e));
        }

        // Flush any packets that were buffered during reconnect backoff
        for data in pending_writes.drain(..) {
            let to_send = if data.ends_with("\r\n") {
                data
            } else {
                format!("{}\r\n", data)
            };
            if let Err(e) = writer.write_all(to_send.as_bytes()).await {
                return ConnectionResult::Disconnected(format!("write error: {}", e));
            }
        }
        if let Err(e) = writer.flush().await {
            return ConnectionResult::Disconnected(format!("flush error: {}", e));
        }

        // Main loop: read from server, write from queue, monitor heartbeat
        let mut line_buf = String::new();
        let heartbeat_timeout = self.heartbeat_timeout;

        loop {
            line_buf.clear();

            tokio::select! {
                // Read a line from the server
                result = timeout(heartbeat_timeout, reader.read_line(&mut line_buf)) => {
                    match result {
                        Ok(Ok(0)) => {
                            // EOF - server closed connection
                            return ConnectionResult::Disconnected("server closed connection".to_string());
                        }
                        Ok(Ok(_)) => {
                            let line = line_buf.trim_end();
                            if line.is_empty() {
                                continue;
                            }

                            if line.starts_with('#') {
                                // Server comment/status line
                                debug!("APRS-IS server: {}", line);
                                continue;
                            }

                            // Valid APRS packet line
                            debug!("APRS-IS rx: {}", line);
                            let packet = Arc::new(Packet::new(line, "APRSIS", true));

                            match packet_tx.try_send(packet) {
                                Ok(()) => {}
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    warn!("APRS-IS: packet queue full, dropping packet");
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    // Receiver dropped - shutdown
                                    return ConnectionResult::Shutdown;
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            return ConnectionResult::Disconnected(format!("read error: {}", e));
                        }
                        Err(_) => {
                            // Heartbeat timeout
                            return ConnectionResult::Disconnected("heartbeat timeout".to_string());
                        }
                    }
                }

                // Check for packets to write
                msg = write_rx.recv() => {
                    match msg {
                        Some(data) => {
                            // Write this packet
                            let to_send = if data.ends_with("\r\n") {
                                data
                            } else {
                                format!("{}\r\n", data)
                            };

                            if let Err(e) = writer.write_all(to_send.as_bytes()).await {
                                return ConnectionResult::Disconnected(format!("write error: {}", e));
                            }

                            // Drain any additional queued packets (batch write)
                            let mut batch_count = 1;
                            while batch_count < WRITE_BATCH_MAX {
                                match write_rx.try_recv() {
                                    Ok(data) => {
                                        let to_send = if data.ends_with("\r\n") {
                                            data
                                        } else {
                                            format!("{}\r\n", data)
                                        };
                                        if let Err(e) = writer.write_all(to_send.as_bytes()).await {
                                            return ConnectionResult::Disconnected(
                                                format!("write error: {}", e)
                                            );
                                        }
                                        batch_count += 1;
                                    }
                                    Err(_) => break,
                                }
                            }

                            // Flush the writer after the batch
                            if let Err(e) = writer.flush().await {
                                return ConnectionResult::Disconnected(format!("flush error: {}", e));
                            }

                            if batch_count > 1 {
                                debug!("APRS-IS: wrote batch of {} packets", batch_count);
                            }
                        }
                        None => {
                            // write_rx closed => shutdown
                            return ConnectionResult::Shutdown;
                        }
                    }
                }
            }
        }
    }
}

/// Result of a single connection attempt.
enum ConnectionResult {
    /// Clean shutdown requested.
    Shutdown,
    /// Connection lost with reason.
    Disconnected(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- aprs_passcode tests ----

    #[test]
    fn passcode_known_callsign_n0call() {
        // Well-known test vector
        let code = aprs_passcode("N0CALL");
        assert_eq!(code, 13023);
    }

    #[test]
    fn passcode_strips_ssid() {
        // Passcode should be the same with or without SSID
        let without = aprs_passcode("N0CALL");
        let with = aprs_passcode("N0CALL-1");
        assert_eq!(without, with);
    }

    #[test]
    fn passcode_case_insensitive() {
        let upper = aprs_passcode("N0CALL");
        let lower = aprs_passcode("n0call");
        assert_eq!(upper, lower);
    }

    #[test]
    fn passcode_is_positive() {
        // The high bit is masked off, so result should always be non-negative
        let code = aprs_passcode("TEST");
        assert!(code >= 0);
    }

    #[test]
    fn passcode_odd_length_callsign() {
        // Callsign with odd number of characters - should not panic
        let code = aprs_passcode("W1AW");
        assert!(code >= 0);

        let code2 = aprs_passcode("AA1AA");
        assert!(code2 >= 0);
    }

    #[test]
    fn passcode_single_char_callsign() {
        // Edge case: single character (unusual but should not panic)
        let code = aprs_passcode("A");
        assert!(code >= 0);
    }

    #[test]
    fn passcode_empty_callsign() {
        // Edge case: empty string should not panic
        let code = aprs_passcode("");
        // With empty input, hash stays at 0x73e2 & 0x7FFF = 0x73e2 = 29666
        assert_eq!(code, 0x73e2u16 as i16 & 0x7FFF);
    }

    #[test]
    fn passcode_different_callsigns_differ() {
        let a = aprs_passcode("N0CALL");
        let b = aprs_passcode("W1AW");
        assert_ne!(a, b);
    }

    // ---- AprsIsClient construction tests ----

    fn test_config() -> AprsIsConfig {
        AprsIsConfig {
            passcode: 12345,
            servers: vec!["rotate.aprs2.net:14580".to_string()],
            filter: Some("m/100".to_string()),
            heartbeat_timeout: Some(90),
        }
    }

    fn test_config_minimal() -> AprsIsConfig {
        AprsIsConfig {
            passcode: -1,
            servers: vec!["rotate.aprs2.net:14580".to_string()],
            filter: None,
            heartbeat_timeout: None,
        }
    }

    #[test]
    fn client_new_stores_config() {
        let config = test_config();
        let client = AprsIsClient::new("OH2MQK-1", &config);

        assert_eq!(client.login, "OH2MQK-1");
        assert_eq!(client.passcode, 12345);
        assert_eq!(client.servers, vec!["rotate.aprs2.net:14580"]);
        assert_eq!(client.filter, Some("m/100".to_string()));
        assert_eq!(client.heartbeat_timeout, Duration::from_secs(90));
    }

    #[test]
    fn client_new_default_heartbeat() {
        let config = test_config_minimal();
        let client = AprsIsClient::new("N0CALL", &config);

        assert_eq!(
            client.heartbeat_timeout,
            Duration::from_secs(DEFAULT_HEARTBEAT_TIMEOUT_SECS)
        );
    }

    // ---- login_line tests ----

    #[test]
    fn login_line_with_filter() {
        let config = test_config();
        let client = AprsIsClient::new("OH2MQK-1", &config);
        let line = client.login_line();

        assert_eq!(
            line,
            "user OH2MQK-1 pass 12345 vers vaprs 0.1.0 filter m/100\r\n"
        );
    }

    #[test]
    fn login_line_without_filter() {
        let config = test_config_minimal();
        let client = AprsIsClient::new("N0CALL", &config);
        let line = client.login_line();

        assert_eq!(line, "user N0CALL pass -1 vers vaprs 0.1.0\r\n");
    }

    #[test]
    fn login_line_ends_with_crlf() {
        let config = test_config();
        let client = AprsIsClient::new("TEST-1", &config);
        let line = client.login_line();

        assert!(line.ends_with("\r\n"));
    }

    #[test]
    fn login_line_starts_with_user() {
        let config = test_config();
        let client = AprsIsClient::new("TEST-1", &config);
        let line = client.login_line();

        assert!(line.starts_with("user "));
    }

    #[test]
    fn login_line_contains_vers() {
        let config = test_config();
        let client = AprsIsClient::new("TEST-1", &config);
        let line = client.login_line();

        assert!(line.contains("vers vaprs 0.1.0"));
    }

    #[test]
    fn login_line_receive_only_shows_negative_passcode() {
        let config = test_config_minimal();
        let client = AprsIsClient::new("RX-ONLY", &config);
        let line = client.login_line();

        assert!(line.contains("pass -1"));
    }

    // ---- Integration-style tests using a mock TCP server ----

    use tokio::net::TcpListener;

    /// Helper: start a mock APRS-IS server that sends a greeting and then
    /// echoes received lines back as comment lines.
    async fn mock_server() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        (listener, addr)
    }

    #[tokio::test]
    async fn run_receives_packets_from_server() {
        let (listener, addr) = mock_server().await;

        // Server task: accept, send greeting + packets, then close
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader_half, mut writer) = stream.into_split();

            // Server greeting
            writer.write_all(b"# javAPRSSrvr 4.2.0\r\n").await.unwrap();

            // Read login line
            let mut reader = BufReader::new(reader_half);
            let mut login = String::new();
            reader.read_line(&mut login).await.unwrap();

            // Send verified response
            writer
                .write_all(b"# logresp N0CALL verified\r\n")
                .await
                .unwrap();

            // Send some APRS packets
            writer
                .write_all(b"OH2MQK-1>APRS:!6029.50N/02505.43E>\r\n")
                .await
                .unwrap();
            writer
                .write_all(b"W3ADO-1>APRS,WIDE1-1:=4903.50N/07201.75W-\r\n")
                .await
                .unwrap();
            writer.flush().await.unwrap();

            // Close connection
            drop(writer);
            drop(reader);
        });

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(5),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, mut packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (write_tx, write_rx) = mpsc::channel::<String>(32);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Receive the two packets
        let pkt1 = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timeout waiting for packet 1")
            .expect("channel closed");
        assert_eq!(pkt1.tnc2, "OH2MQK-1>APRS:!6029.50N/02505.43E>");
        assert_eq!(pkt1.source_interface, "APRSIS");

        let pkt2 = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timeout waiting for packet 2")
            .expect("channel closed");
        assert_eq!(pkt2.tnc2, "W3ADO-1>APRS,WIDE1-1:=4903.50N/07201.75W-");

        // Drop write_tx to signal shutdown
        drop(write_tx);

        // Wait for client to finish (with timeout)
        tokio::time::timeout(Duration::from_secs(10), client_task)
            .await
            .expect("client task timeout")
            .expect("client task panicked");

        server.await.expect("server task panicked");
    }

    #[tokio::test]
    async fn run_filters_comment_lines() {
        let (listener, addr) = mock_server().await;

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader_half, mut writer) = stream.into_split();

            writer.write_all(b"# javAPRSSrvr 4.2.0\r\n").await.unwrap();

            let mut reader = BufReader::new(reader_half);
            let mut login = String::new();
            reader.read_line(&mut login).await.unwrap();

            writer
                .write_all(b"# logresp N0CALL verified\r\n")
                .await
                .unwrap();
            // Only comment lines, no real packets
            writer.write_all(b"# server status info\r\n").await.unwrap();
            writer.write_all(b"# another comment\r\n").await.unwrap();
            // One real packet
            writer
                .write_all(b"TEST>APRS:!0000.00N/00000.00E>\r\n")
                .await
                .unwrap();
            writer.flush().await.unwrap();

            drop(writer);
            drop(reader);
        });

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(5),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, mut packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (_write_tx, write_rx) = mpsc::channel::<String>(32);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Should only receive the one real packet, not comments
        let pkt = tokio::time::timeout(Duration::from_secs(5), packet_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");
        assert_eq!(pkt.tnc2, "TEST>APRS:!0000.00N/00000.00E>");

        // Drop to allow client to shut down via channel close detection
        drop(_write_tx);

        tokio::time::timeout(Duration::from_secs(15), client_task)
            .await
            .expect("client task timeout")
            .expect("client task panicked");

        server.await.expect("server panicked");
    }

    #[tokio::test]
    async fn run_sends_packets_to_server() {
        let (listener, addr) = mock_server().await;

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut lines = Vec::new();

            // Read all lines until connection closes
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => lines.push(line.trim().to_string()),
                    Err(_) => break,
                }
            }

            lines
        });

        let config = AprsIsConfig {
            passcode: 12345,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(5),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, _packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (write_tx, write_rx) = mpsc::channel::<String>(32);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Give the client a moment to connect and send login
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Send a packet through the write channel
        write_tx
            .send("TEST>APRS:!0000.00N/00000.00E>".to_string())
            .await
            .unwrap();

        // Give it time to send
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Shutdown
        drop(write_tx);

        tokio::time::timeout(Duration::from_secs(10), client_task)
            .await
            .expect("client task timeout")
            .expect("client task panicked");

        let lines = server.await.expect("server panicked");

        // First line should be the login
        assert!(lines[0].starts_with("user N0CALL pass 12345"));

        // Second line should be our packet
        assert!(lines.len() >= 2);
        assert_eq!(lines[1], "TEST>APRS:!0000.00N/00000.00E>");
    }

    #[tokio::test]
    async fn run_detects_heartbeat_timeout() {
        let (listener, addr) = mock_server().await;

        // Track how many connections the server sees
        let connection_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let count_clone = connection_count.clone();

        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let count = count_clone.clone();
                // Spawn each connection handler so the accept loop continues
                tokio::spawn(async move {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut reader = BufReader::new(stream);
                    let mut login = String::new();
                    let _ = reader.read_line(&mut login).await;
                    // Don't send anything - let heartbeat timeout trigger
                    tokio::time::sleep(Duration::from_secs(60)).await;
                });
            }
        });

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(1), // 1 second timeout for test speed
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, _packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (write_tx, write_rx) = mpsc::channel::<String>(32);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Wait long enough for the heartbeat timeout (1s) + reconnect backoff (10s)
        // + second connection attempt
        tokio::time::sleep(Duration::from_secs(13)).await;

        // Should have connected at least twice (initial + reconnect after timeout)
        let count = connection_count.load(std::sync::atomic::Ordering::SeqCst);
        assert!(count >= 2, "expected at least 2 connections, got {}", count);

        // Signal shutdown
        drop(write_tx);

        tokio::time::timeout(Duration::from_secs(15), client_task)
            .await
            .expect("client task timeout")
            .expect("client task panicked");

        server.abort();
    }

    #[tokio::test]
    async fn run_with_no_servers_returns_immediately() {
        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![],
            filter: None,
            heartbeat_timeout: None,
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, _packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (_write_tx, write_rx) = mpsc::channel::<String>(32);

        // Should return immediately with no servers
        tokio::time::timeout(Duration::from_secs(2), client.run(packet_tx, write_rx))
            .await
            .expect("should return immediately with no servers");
    }

    #[tokio::test]
    async fn run_shuts_down_when_write_channel_closed() {
        let (listener, addr) = mock_server().await;

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader_half, mut writer) = stream.into_split();

            writer.write_all(b"# javAPRSSrvr 4.2.0\r\n").await.unwrap();

            let mut reader = BufReader::new(reader_half);
            let mut login = String::new();
            reader.read_line(&mut login).await.unwrap();

            writer
                .write_all(b"# logresp N0CALL verified\r\n")
                .await
                .unwrap();
            writer.flush().await.unwrap();

            // Keep connection alive
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(30),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, _packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (write_tx, write_rx) = mpsc::channel::<String>(32);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Wait for connection to establish
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Close write channel to signal shutdown
        drop(write_tx);

        // Client should shut down promptly
        tokio::time::timeout(Duration::from_secs(5), client_task)
            .await
            .expect("client should shut down when write channel closes")
            .expect("client task panicked");

        server.abort();
    }

    #[tokio::test]
    async fn run_reconnects_on_server_disconnect() {
        let (listener, addr) = mock_server().await;

        let server = tokio::spawn(async move {
            // First connection: accept, greet, close immediately
            {
                let (stream, _) = listener.accept().await.unwrap();
                let (reader_half, mut writer) = stream.into_split();
                writer.write_all(b"# javAPRSSrvr 4.2.0\r\n").await.unwrap();

                let mut reader = BufReader::new(reader_half);
                let mut login = String::new();
                reader.read_line(&mut login).await.unwrap();
                // Close connection by dropping
            }

            // Second connection: accept, greet, send a packet, then keep alive
            let (stream, _) = listener.accept().await.unwrap();
            let (reader_half, mut writer) = stream.into_split();
            writer.write_all(b"# javAPRSSrvr 4.2.0\r\n").await.unwrap();

            let mut reader = BufReader::new(reader_half);
            let mut login2 = String::new();
            reader.read_line(&mut login2).await.unwrap();

            writer
                .write_all(b"# logresp N0CALL verified\r\n")
                .await
                .unwrap();
            writer.write_all(b"RECONNECT>APRS:test\r\n").await.unwrap();
            writer.flush().await.unwrap();

            // Keep alive briefly
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(5),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, mut packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (write_tx, write_rx) = mpsc::channel::<String>(32);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Should receive the packet from the second connection (after reconnect)
        let pkt = tokio::time::timeout(Duration::from_secs(20), packet_rx.recv())
            .await
            .expect("timeout waiting for reconnect packet")
            .expect("channel closed");
        assert_eq!(pkt.tnc2, "RECONNECT>APRS:test");

        drop(write_tx);

        tokio::time::timeout(Duration::from_secs(10), client_task)
            .await
            .expect("client task timeout")
            .expect("client task panicked");

        server.abort();
    }

    #[tokio::test]
    async fn run_rotates_servers_on_failure() {
        // Two servers, first one will refuse connections
        let (listener2, addr2) = mock_server().await;

        // Use a non-routable address that will fail to connect quickly
        // We'll use port 1 on localhost which is likely closed
        let addr1 = "127.0.0.1:1".to_string();

        let server = tokio::spawn(async move {
            let (stream, _) = listener2.accept().await.unwrap();
            let (reader_half, mut writer) = stream.into_split();

            writer.write_all(b"# javAPRSSrvr 4.2.0\r\n").await.unwrap();

            let mut reader = BufReader::new(reader_half);
            let mut login = String::new();
            reader.read_line(&mut login).await.unwrap();

            writer
                .write_all(b"# logresp N0CALL verified\r\n")
                .await
                .unwrap();
            writer
                .write_all(b"ROTATED>APRS:from server 2\r\n")
                .await
                .unwrap();
            writer.flush().await.unwrap();

            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr1, addr2],
            filter: None,
            heartbeat_timeout: Some(5),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, mut packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (write_tx, write_rx) = mpsc::channel::<String>(32);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Should eventually connect to server 2 and get the packet
        let pkt = tokio::time::timeout(Duration::from_secs(30), packet_rx.recv())
            .await
            .expect("timeout waiting for rotated server packet")
            .expect("channel closed");
        assert_eq!(pkt.tnc2, "ROTATED>APRS:from server 2");

        drop(write_tx);

        tokio::time::timeout(Duration::from_secs(10), client_task)
            .await
            .expect("client task timeout")
            .expect("client task panicked");

        server.abort();
    }

    #[tokio::test]
    async fn write_appends_crlf_if_missing() {
        let (listener, addr) = mock_server().await;

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut lines = Vec::new();

            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => lines.push(line),
                    Err(_) => break,
                }
            }

            lines
        });

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(5),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, _packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (write_tx, write_rx) = mpsc::channel::<String>(32);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;

        // Send without \r\n - client should add it
        write_tx
            .send("TEST>APRS:no crlf".to_string())
            .await
            .unwrap();

        // Send with \r\n already - should not double it
        write_tx
            .send("TEST>APRS:has crlf\r\n".to_string())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(write_tx);

        tokio::time::timeout(Duration::from_secs(10), client_task)
            .await
            .expect("client task timeout")
            .expect("client task panicked");

        let lines = server.await.expect("server panicked");

        // Check that both packets arrived with proper \r\n termination
        // lines[0] is the login line
        assert!(
            lines.len() >= 3,
            "expected at least 3 lines, got {:?}",
            lines
        );
        assert_eq!(lines[1].trim(), "TEST>APRS:no crlf");
        assert_eq!(lines[2].trim(), "TEST>APRS:has crlf");
    }

    #[tokio::test]
    async fn run_shuts_down_when_packet_tx_closed() {
        let (listener, addr) = mock_server().await;

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader_half, mut writer) = stream.into_split();

            writer.write_all(b"# javAPRSSrvr 4.2.0\r\n").await.unwrap();

            let mut reader = BufReader::new(reader_half);
            let mut login = String::new();
            reader.read_line(&mut login).await.unwrap();

            writer
                .write_all(b"# logresp N0CALL verified\r\n")
                .await
                .unwrap();
            // Send a packet that will trigger the shutdown check
            writer.write_all(b"TEST>APRS:trigger\r\n").await.unwrap();
            writer.flush().await.unwrap();

            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(30),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let (packet_tx, packet_rx) = mpsc::channel::<SharedPacket>(32);
        let (_write_tx, write_rx) = mpsc::channel::<String>(32);

        // Drop the receiver immediately - packet_tx.send() should fail
        drop(packet_rx);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Client should detect that packet_tx sends fail and shut down
        tokio::time::timeout(Duration::from_secs(10), client_task)
            .await
            .expect("client should shut down when packet_rx is dropped")
            .expect("client task panicked");

        server.abort();
    }

    #[tokio::test]
    async fn run_drops_packets_when_queue_full() {
        let (listener, addr) = mock_server().await;

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader_half, mut writer) = stream.into_split();

            writer.write_all(b"# javAPRSSrvr 4.2.0\r\n").await.unwrap();

            let mut reader = BufReader::new(reader_half);
            let mut login = String::new();
            reader.read_line(&mut login).await.unwrap();

            writer
                .write_all(b"# logresp N0CALL verified\r\n")
                .await
                .unwrap();

            // Send many packets rapidly - more than the channel capacity
            for i in 0..20 {
                let line = format!("FLOOD-{}>APRS:packet {}\r\n", i, i);
                writer.write_all(line.as_bytes()).await.unwrap();
            }
            writer.flush().await.unwrap();

            // Keep connection alive so the client doesn't reconnect
            tokio::time::sleep(Duration::from_secs(10)).await;
        });

        // Use a very small channel capacity (2) so it fills up quickly
        let (packet_tx, mut packet_rx) = mpsc::channel::<SharedPacket>(2);
        let (write_tx, write_rx) = mpsc::channel::<String>(32);

        let config = AprsIsConfig {
            passcode: -1,
            servers: vec![addr],
            filter: None,
            heartbeat_timeout: Some(5),
        };
        let client = AprsIsClient::new("N0CALL", &config);

        let client_task = tokio::spawn(async move {
            client.run(packet_tx, write_rx).await;
        });

        // Don't read from packet_rx immediately - let the channel fill up.
        // The client should drop packets rather than blocking.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Now drain what we can - we should get some packets (the ones that fit)
        let mut received = Vec::new();
        while let Ok(Some(pkt)) =
            tokio::time::timeout(Duration::from_millis(100), packet_rx.recv()).await
        {
            received.push(pkt);
        }

        // We should have received some packets but not all 20 (channel capacity was 2)
        assert!(
            !received.is_empty(),
            "should have received at least some packets"
        );
        assert!(
            received.len() < 20,
            "should have dropped some packets, but got all {}",
            received.len()
        );

        // The client should still be running (not blocked or crashed)
        assert!(!client_task.is_finished(), "client should still be running");

        drop(write_tx);

        tokio::time::timeout(Duration::from_secs(10), client_task)
            .await
            .expect("client task timeout")
            .expect("client task panicked");

        server.abort();
    }

    #[test]
    fn passcode_deterministic() {
        // The passcode for any callsign should be deterministic
        let first = aprs_passcode("OH2MQK");
        let second = aprs_passcode("OH2MQK");
        assert_eq!(first, second);
    }

    #[test]
    fn passcode_known_w3ado() {
        assert_eq!(aprs_passcode("W3ADO"), 10901);
    }

    #[test]
    fn client_multiple_servers() {
        let config = AprsIsConfig {
            passcode: 12345,
            servers: vec![
                "server1.aprs2.net:14580".to_string(),
                "server2.aprs2.net:14580".to_string(),
            ],
            filter: None,
            heartbeat_timeout: None,
        };
        let client = AprsIsClient::new("TEST", &config);
        assert_eq!(client.servers.len(), 2);
    }
}
