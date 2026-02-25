// Minimal HTTP/1.1 server for the dashboard.
//
// Hand-rolled on tokio::net::TcpListener to avoid pulling in a full HTTP
// framework. Serves 5 routes: static assets, JSON API, and SSE stream.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use crate::config::WebConfig;
use crate::web::SharedDashboardState;

/// Embedded static assets.
const INDEX_HTML: &str = include_str!("static/index.html");
const STYLE_CSS: &str = include_str!("static/style.css");
const APP_JS: &str = include_str!("static/app.js");

/// Maximum concurrent SSE connections.
const MAX_SSE_CONNECTIONS: u32 = 5;

/// SSE full state push interval.
const SSE_STATE_INTERVAL_MS: u64 = 5000;

/// SSE poll interval for new packets.
const SSE_POLL_INTERVAL_MS: u64 = 200;

/// Maximum length of an HTTP request line (method + path + version).
const MAX_REQUEST_LINE_LEN: usize = 8192;

/// Maximum number of HTTP headers to accept.
const MAX_HEADER_COUNT: usize = 64;

/// Maximum length of a single HTTP header line.
const MAX_HEADER_LINE_LEN: usize = 8192;

/// Maximum lifetime for an SSE connection (safety net for zombies).
const SSE_MAX_LIFETIME_SECS: u64 = 300;

/// Read a single line from `reader`, limited to `max_len` bytes.
/// Returns `Ok(Some(line))` on success, `Ok(None)` on EOF, or `Err` if the
/// line exceeds the limit or an I/O error occurs.
async fn read_line_bounded<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max_len: usize,
) -> std::io::Result<Option<String>> {
    use tokio::io::AsyncBufReadExt;
    let mut buf = Vec::with_capacity(256);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // EOF
            if buf.is_empty() {
                return Ok(None);
            }
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
        // Find newline in available data
        if let Some(newline_pos) = available.iter().position(|&b| b == b'\n') {
            let to_take = newline_pos + 1; // include the newline
            if buf.len() + to_take > max_len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "line too long",
                ));
            }
            buf.extend_from_slice(&available[..to_take]);
            reader.consume(to_take);
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
        // No newline found in available data
        let available_len = available.len();
        if buf.len() + available_len > max_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
        buf.extend_from_slice(available);
        reader.consume(available_len);
    }
}

/// Parsed HTTP request (method + path only).
#[derive(Debug, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
}

/// Parse the first line of an HTTP request into method and path.
pub fn parse_request_line(line: &str) -> Option<HttpRequest> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some(HttpRequest {
        method: method.to_string(),
        path: path.to_string(),
    })
}

/// Spawn the web server task. Returns the JoinHandle so the caller can abort it.
pub fn spawn_web_server(
    config: &WebConfig,
    state: SharedDashboardState,
) -> tokio::task::JoinHandle<()> {
    let addr = format!("{}:{}", config.listen, config.port);
    tokio::spawn(run_server(addr, state))
}

/// Run the TCP listener accept loop.
async fn run_server(addr: String, state: SharedDashboardState) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => {
            info!(addr = %addr, "web dashboard listening");
            l
        }
        Err(e) => {
            warn!(addr = %addr, error = %e, "failed to bind web dashboard");
            return;
        }
    };

    let sse_count = Arc::new(AtomicU32::new(0));

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "web accept error");
                continue;
            }
        };

        debug!(peer = %peer, "web connection");
        let state = state.clone();
        let sse_count = sse_count.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state, sse_count).await {
                debug!(peer = %peer, error = %e, "web connection error");
            }
        });
    }
}

/// Handle a single HTTP connection.
async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: SharedDashboardState,
    sse_count: Arc<AtomicU32>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Read the request line (bounded)
    let line = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_line_bounded(&mut reader, MAX_REQUEST_LINE_LEN),
    )
    .await
    {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => return Ok(()),
        Ok(Err(_)) => {
            write_response(&mut writer, 400, "text/plain", "Bad Request").await?;
            return Ok(());
        }
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request timeout",
            ));
        }
    };

    let request = match parse_request_line(line.trim()) {
        Some(r) => r,
        None => {
            write_response(&mut writer, 400, "text/plain", "Bad Request").await?;
            return Ok(());
        }
    };

    // Consume remaining headers (bounded)
    let mut header_count = 0;
    loop {
        let header_line = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_line_bounded(&mut reader, MAX_HEADER_LINE_LEN),
        )
        .await
        {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => break,
            Ok(Err(_)) => {
                write_response(&mut writer, 400, "text/plain", "Bad Request").await?;
                return Ok(());
            }
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "header timeout",
                ));
            }
        };

        if header_line.trim().is_empty() {
            break;
        }

        header_count += 1;
        if header_count >= MAX_HEADER_COUNT {
            write_response(&mut writer, 400, "text/plain", "Bad Request").await?;
            return Ok(());
        }
    }

    if request.method != "GET" {
        write_response(&mut writer, 405, "text/plain", "Method Not Allowed").await?;
        return Ok(());
    }

    match request.path.as_str() {
        "/" => {
            write_response(&mut writer, 200, "text/html; charset=utf-8", INDEX_HTML).await?;
        }
        "/style.css" => {
            write_response(&mut writer, 200, "text/css; charset=utf-8", STYLE_CSS).await?;
        }
        "/app.js" => {
            write_response(
                &mut writer,
                200,
                "application/javascript; charset=utf-8",
                APP_JS,
            )
            .await?;
        }
        "/api/state" => {
            let json = {
                let s = state.lock().unwrap();
                s.to_json()
            };
            write_response(&mut writer, 200, "application/json", &json).await?;
        }
        "/api/events" => {
            handle_sse(writer, state, sse_count).await;
        }
        _ => {
            write_response(&mut writer, 404, "text/plain", "Not Found").await?;
        }
    }

    Ok(())
}

/// Write a standard HTTP response.
async fn write_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    status: u16,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Unknown",
    };

    let response = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        status,
        status_text,
        content_type,
        body.len(),
        body,
    );

    writer.write_all(response.as_bytes()).await?;
    writer.flush().await
}

/// Handle an SSE connection.
async fn handle_sse(
    mut writer: tokio::net::tcp::OwnedWriteHalf,
    state: SharedDashboardState,
    sse_count: Arc<AtomicU32>,
) {
    // Check connection limit
    let current = sse_count.fetch_add(1, Ordering::Relaxed);
    if current >= MAX_SSE_CONNECTIONS {
        sse_count.fetch_sub(1, Ordering::Relaxed);
        let _ = write_response(&mut writer, 503, "text/plain", "Too many SSE connections").await;
        return;
    }

    // Send SSE headers
    let headers = "HTTP/1.1 200 OK\r\n\
                   Content-Type: text/event-stream\r\n\
                   Cache-Control: no-cache\r\n\
                   Connection: keep-alive\r\n\
                   \r\n";

    if writer.write_all(headers.as_bytes()).await.is_err() {
        sse_count.fetch_sub(1, Ordering::Relaxed);
        return;
    }

    // Send initial full state
    {
        let json = state.lock().unwrap().to_json();
        if send_sse_event(&mut writer, "state", &json).await.is_err() {
            sse_count.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    }

    let mut last_sequence = {
        let s = state.lock().unwrap();
        s.packet_sequence
    };

    let mut poll_interval =
        tokio::time::interval(std::time::Duration::from_millis(SSE_POLL_INTERVAL_MS));
    let mut state_interval =
        tokio::time::interval(std::time::Duration::from_millis(SSE_STATE_INTERVAL_MS));
    // First tick completes immediately, consume it
    poll_interval.tick().await;
    state_interval.tick().await;

    let sse_deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(SSE_MAX_LIFETIME_SECS);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(sse_deadline) => {
                debug!("SSE connection lifetime expired");
                sse_count.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            _ = poll_interval.tick() => {
                // Clone new packets under lock, serialize outside
                let new_packets = {
                    let s = state.lock().unwrap();
                    if s.packet_sequence > last_sequence {
                        let packets: Vec<_> = s.recent_packets.iter()
                            .filter(|p| p.sequence > last_sequence)
                            .cloned()
                            .collect();
                        last_sequence = s.packet_sequence;
                        packets
                    } else {
                        Vec::new()
                    }
                }; // lock released before serialization

                for pkt in &new_packets {
                    let json = serde_json::to_string(pkt).unwrap_or_default();
                    if send_sse_event(&mut writer, "packet", &json).await.is_err() {
                        sse_count.fetch_sub(1, Ordering::Relaxed);
                        return;
                    }
                }
            }
            _ = state_interval.tick() => {
                // Build JSON under lock (to_json already returns an owned String)
                let json = { state.lock().unwrap().to_json() };
                // Lock released — send over the wire without holding it
                if send_sse_event(&mut writer, "state", &json).await.is_err() {
                    sse_count.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}

/// Format and send a single SSE event.
async fn send_sse_event(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    event: &str,
    data: &str,
) -> std::io::Result<()> {
    let frame = format_sse_event(event, data);
    writer.write_all(frame.as_bytes()).await?;
    writer.flush().await
}

/// Format an SSE event frame.
pub fn format_sse_event(event: &str, data: &str) -> String {
    let mut frame = format!("event: {}\n", event);
    if data.is_empty() {
        frame.push_str("data: \n");
    } else {
        for line in data.lines() {
            frame.push_str(&format!("data: {}\n", line));
        }
    }
    frame.push('\n');
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    #[test]
    fn test_parse_request_line_get() {
        let req = parse_request_line("GET / HTTP/1.1").unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/");
    }

    #[test]
    fn test_parse_request_line_with_path() {
        let req = parse_request_line("GET /api/state HTTP/1.1").unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/api/state");
    }

    #[test]
    fn test_parse_request_line_post() {
        let req = parse_request_line("POST /api/data HTTP/1.1").unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/data");
    }

    #[test]
    fn test_parse_request_line_empty() {
        assert!(parse_request_line("").is_none());
    }

    #[test]
    fn test_parse_request_line_missing_path() {
        assert!(parse_request_line("GET").is_none());
    }

    #[test]
    fn test_format_sse_event_simple() {
        let frame = format_sse_event("state", r#"{"mycall":"TEST"}"#);
        assert_eq!(frame, "event: state\ndata: {\"mycall\":\"TEST\"}\n\n");
    }

    #[test]
    fn test_format_sse_event_multiline_data() {
        let frame = format_sse_event("state", "line1\nline2\nline3");
        assert_eq!(
            frame,
            "event: state\ndata: line1\ndata: line2\ndata: line3\n\n"
        );
    }

    #[test]
    fn test_format_sse_event_empty_data() {
        let frame = format_sse_event("ping", "");
        assert_eq!(frame, "event: ping\ndata: \n\n");
    }

    #[tokio::test]
    async fn test_server_serves_static_assets() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::web::DashboardState::new(
            "TEST-1",
            vec!["radio0".to_string()],
        )));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = state.clone();
        let server_handle = tokio::spawn(async move {
            let sse_count = Arc::new(AtomicU32::new(0));
            // Accept just one connection for the test
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, server_state, sse_count)
                .await
                .unwrap();
        });

        // Connect and request the index page
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let mut response = String::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("text/html"));
        assert!(response.contains("vaprs"));

        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_server_serves_json() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::web::DashboardState::new(
            "TEST-1",
            vec![],
        )));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = state.clone();
        let server_handle = tokio::spawn(async move {
            let sse_count = Arc::new(AtomicU32::new(0));
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, server_state, sse_count)
                .await
                .unwrap();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /api/state HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let mut response = String::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("application/json"));
        assert!(response.contains("TEST-1"));

        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_server_returns_404() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::web::DashboardState::new(
            "TEST-1",
            vec![],
        )));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = state.clone();
        let server_handle = tokio::spawn(async move {
            let sse_count = Arc::new(AtomicU32::new(0));
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, server_state, sse_count)
                .await
                .unwrap();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let mut response = String::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        assert!(response.starts_with("HTTP/1.1 404 Not Found"));

        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_server_rejects_post() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::web::DashboardState::new(
            "TEST-1",
            vec![],
        )));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = state.clone();
        let server_handle = tokio::spawn(async move {
            let sse_count = Arc::new(AtomicU32::new(0));
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, server_state, sse_count)
                .await
                .unwrap();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"POST / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let mut response = String::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        assert!(response.starts_with("HTTP/1.1 405"));

        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_oversized_request_line_returns_400() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::web::DashboardState::new(
            "TEST-1",
            vec![],
        )));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = state.clone();
        let server_handle = tokio::spawn(async move {
            let sse_count = Arc::new(AtomicU32::new(0));
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, server_state, sse_count)
                .await
                .ok();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Send a request line that exceeds MAX_REQUEST_LINE_LEN (8192)
        let huge_path = "X".repeat(9000);
        let request = format!("GET /{} HTTP/1.1\r\n\r\n", huge_path);
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut response = String::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        assert!(
            response.starts_with("HTTP/1.1 400"),
            "oversized request should get 400, got: {}",
            &response[..std::cmp::min(50, response.len())]
        );

        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_too_many_headers_returns_400() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::web::DashboardState::new(
            "TEST-1",
            vec![],
        )));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = state.clone();
        let server_handle = tokio::spawn(async move {
            let sse_count = Arc::new(AtomicU32::new(0));
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, server_state, sse_count)
                .await
                .ok();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Send valid request line followed by >64 headers
        stream.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        for i in 0..70 {
            let header = format!("X-Header-{}: value\r\n", i);
            stream.write_all(header.as_bytes()).await.unwrap();
        }
        stream.write_all(b"\r\n").await.unwrap();

        let mut response = String::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        assert!(
            response.starts_with("HTTP/1.1 400"),
            "too many headers should get 400, got: {}",
            &response[..std::cmp::min(50, response.len())]
        );

        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_bounded_read_normal_request_still_works() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::web::DashboardState::new(
            "TEST-1",
            vec!["radio0".to_string()],
        )));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = state.clone();
        let server_handle = tokio::spawn(async move {
            let sse_count = Arc::new(AtomicU32::new(0));
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, server_state, sse_count)
                .await
                .unwrap();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /api/state HTTP/1.1\r\nHost: localhost\r\nAccept: */*\r\n\r\n")
            .await
            .unwrap();

        let mut response = String::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => response.push_str(&line),
                Err(_) => break,
            }
        }

        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "normal request should still get 200"
        );
        assert!(response.contains("application/json"));

        let _ = server_handle.await;
    }
}
