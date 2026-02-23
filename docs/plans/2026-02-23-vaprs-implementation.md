# vaprs Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a complete Rust reimplementation of aprx (APRS iGate/digipeater) optimized for Raspberry Pi 3.

**Architecture:** Single-threaded Tokio runtime with async tasks per subsystem, communicating via mpsc channels through a central router. Packets flow from radio interfaces through AX.25/KISS decoding, to the router, then fan out to igate, digipeater, and logging subsystems.

**Tech Stack:** Rust, Tokio (current_thread), serde/toml, clap, tokio-serial, tracing, cargo-deb

---

## Stage 1: Project Skeleton + Core Types

### Task 1.1: Initialize Cargo project and dependencies

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `.cargo/config.toml`
- Create: `rust-toolchain.toml`

**Step 1: Create the Cargo project**

Run: `cargo init --name vaprs /Users/graham/dev/vaprs`

**Step 2: Write Cargo.toml with all dependencies and Pi 3 optimizations**

```toml
[package]
name = "vaprs"
version = "0.1.0"
edition = "2021"
description = "APRS iGate and digipeater"
license = "GPL-2.0-or-later"

[dependencies]
tokio = { version = "1", features = ["rt", "net", "io-util", "time", "signal", "sync", "macros", "fs"] }
tokio-serial = "5"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"
clap = { version = "4", features = ["derive"] }
thiserror = "2"
bytes = "1"
smallvec = { version = "1", features = ["serde"] }
regex = "1"
nix = { version = "0.29", features = ["signal", "fs"] }
memmap2 = "0.9"

[dev-dependencies]
assert_matches = "1"

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
strip = true
panic = "abort"

[package.metadata.deb]
maintainer = "Graham <graham@example.com>"
section = "hamradio"
priority = "optional"
depends = "$auto"
assets = [
    ["target/release/vaprs", "usr/sbin/", "755"],
    ["config/vaprs.toml.example", "etc/vaprs/vaprs.toml", "644"],
    ["debian/vaprs.service", "lib/systemd/system/", "644"],
]
conf-files = ["/etc/vaprs/vaprs.toml"]
```

**Step 3: Create .cargo/config.toml for cross-compilation**

```toml
# Uncomment for Pi 3 cross-compilation
# [target.aarch64-unknown-linux-gnu]
# linker = "aarch64-linux-gnu-gcc"
#
# [target.armv7-unknown-linux-gnueabihf]
# linker = "arm-linux-gnueabihf-gcc"
```

**Step 4: Create rust-toolchain.toml**

```toml
[toolchain]
channel = "stable"
```

**Step 5: Write minimal main.rs that compiles**

```rust
fn main() {
    println!("vaprs starting");
}
```

**Step 6: Write empty lib.rs**

```rust
pub mod config;
```

**Step 7: Create stub config module**

Create `src/config.rs`:
```rust
// Configuration parsing - TOML based
```

**Step 8: Verify it compiles**

Run: `cargo build`
Expected: Compiles successfully

**Step 9: Commit**

```bash
git init && git add -A && git commit -m "init: cargo project with dependencies and Pi 3 release profile"
```

---

### Task 1.2: CLI argument parsing and daemon setup

**Files:**
- Modify: `src/main.rs`

**Step 1: Write the test for CLI parsing**

Create `tests/cli_test.rs`:
```rust
use std::process::Command;

#[test]
fn test_version_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_vaprs"))
        .arg("--version")
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("vaprs"));
}

#[test]
fn test_help_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_vaprs"))
        .arg("--help")
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
}

#[test]
fn test_missing_config_exits_with_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_vaprs"))
        .arg("-f")
        .arg("/nonexistent/config.toml")
        .output()
        .expect("failed to execute");
    assert!(!output.status.success());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --test cli_test`
Expected: FAIL (no CLI parsing yet)

**Step 3: Implement CLI with clap**

```rust
// src/main.rs
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "vaprs", version, about = "APRS iGate and digipeater")]
struct Cli {
    /// Configuration file path
    #[arg(short = 'f', long = "config", default_value = "/etc/vaprs/vaprs.toml")]
    config: String,

    /// Increase debug output (repeatable: -d, -dd, -ddd)
    #[arg(short = 'd', long = "debug", action = clap::ArgAction::Count)]
    debug: u8,

    /// Verbose output to stdout
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Output erlang data to syslog
    #[arg(short = 'e', long = "erlang")]
    erlang: bool,

    /// Keep in foreground (no daemonize)
    #[arg(short = 'i', long = "foreground")]
    foreground: bool,

    /// Log APRS-IS traffic
    #[arg(short = 'L', long = "log-aprsis")]
    log_aprsis: bool,
}

fn main() {
    let cli = Cli::parse();

    let foreground = cli.foreground || cli.debug > 0 || cli.verbose || cli.erlang;

    // Validate config file exists
    if !std::path::Path::new(&cli.config).exists() {
        eprintln!("Configuration file not found: {}", cli.config);
        std::process::exit(1);
    }

    println!("vaprs starting with config: {}", cli.config);
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --test cli_test`
Expected: PASS

**Step 5: Commit**

```bash
git add -A && git commit -m "feat: CLI argument parsing with clap"
```

---

### Task 1.3: TOML configuration parsing

**Files:**
- Modify: `src/config.rs`
- Create: `config/vaprs.toml.example`

**Step 1: Write config tests**

Add to `src/config.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
            mycall = "N0CALL-1"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.mycall, "N0CALL-1");
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
            mycall = "OH2MQK-1"

            [location]
            lat = "6029.50N"
            lon = "02505.43E"

            [aprsis]
            passcode = 12345
            servers = ["rotate.aprs2.net:14580"]
            filter = "m/100"
            heartbeat_timeout = 120

            [logging]
            rflog = "/var/log/vaprs-rf.log"
            aprxlog = "/var/log/vaprs.log"

            [[interface]]
            type = "serial"
            device = "/dev/ttyUSB0"
            speed = 19200
            protocol = "KISS"
            callsign = "OH2MQK-1"
            tx_ok = false
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.mycall, "OH2MQK-1");
        assert_eq!(config.aprsis.as_ref().unwrap().passcode, 12345);
        assert_eq!(config.interfaces.len(), 1);
        assert_eq!(config.interfaces[0].device.as_deref(), Some("/dev/ttyUSB0"));
    }

    #[test]
    fn test_parse_interface_types() {
        let toml = r#"
            mycall = "TEST-1"

            [[interface]]
            type = "serial"
            device = "/dev/ttyUSB0"
            speed = 9600
            protocol = "KISS"

            [[interface]]
            type = "tcp"
            host = "192.168.1.100"
            port = 10001
            protocol = "KISS"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.interfaces.len(), 2);
        assert_eq!(config.interfaces[0].iface_type, InterfaceType::Serial);
        assert_eq!(config.interfaces[1].iface_type, InterfaceType::Tcp);
    }

    #[test]
    fn test_default_values() {
        let toml = r#"
            mycall = "N0CALL-1"

            [[interface]]
            type = "serial"
            device = "/dev/ttyUSB0"
            speed = 9600
            protocol = "KISS"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let iface = &config.interfaces[0];
        assert!(!iface.tx_ok);
        assert!(iface.telem_to_is);
        assert_eq!(iface.igate_group, 1);
    }

    #[test]
    fn test_parse_digipeater_config() {
        let toml = r#"
            mycall = "N0CALL-1"

            [[digipeater]]
            transmitter = "N0CALL-1"
            ratelimit = [60, 120]

            [[digipeater.source]]
            source = "N0CALL-1"
            relay_type = "digipeated"
            viscous_delay = 0
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.digipeaters.len(), 1);
        assert_eq!(config.digipeaters[0].transmitter, "N0CALL-1");
        assert_eq!(config.digipeaters[0].sources.len(), 1);
    }

    #[test]
    fn test_parse_beacon_config() {
        let toml = r#"
            mycall = "N0CALL-1"

            [[beacon]]
            symbol = "R&"
            comment = "Rx-only iGate"
            cycle_size = "20m"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.beacons.len(), 1);
        assert_eq!(config.beacons[0].symbol.as_deref(), Some("R&"));
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test config::tests`
Expected: FAIL (types don't exist yet)

**Step 3: Implement config types**

```rust
// src/config.rs
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub mycall: String,
    #[serde(default)]
    pub location: Option<LocationConfig>,
    #[serde(default)]
    pub aprsis: Option<AprsIsConfig>,
    #[serde(default)]
    pub logging: Option<LoggingConfig>,
    #[serde(default, rename = "interface")]
    pub interfaces: Vec<InterfaceConfig>,
    #[serde(default, rename = "beacon")]
    pub beacons: Vec<BeaconConfig>,
    #[serde(default, rename = "digipeater")]
    pub digipeaters: Vec<DigipeaterConfig>,
    #[serde(default, rename = "telemetry")]
    pub telemetry: Vec<TelemetryConfig>,
}

#[derive(Debug, Deserialize)]
pub struct LocationConfig {
    pub lat: String,
    pub lon: String,
}

#[derive(Debug, Deserialize)]
pub struct AprsIsConfig {
    pub login: Option<String>,
    #[serde(default = "default_passcode")]
    pub passcode: i32,
    #[serde(default = "default_servers")]
    pub servers: Vec<String>,
    pub filter: Option<String>,
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout: u32,
}

fn default_passcode() -> i32 { -1 }
fn default_servers() -> Vec<String> { vec!["rotate.aprs2.net:14580".into()] }
fn default_heartbeat_timeout() -> u32 { 120 }

#[derive(Debug, Deserialize)]
pub struct LoggingConfig {
    pub pidfile: Option<String>,
    pub rflog: Option<String>,
    pub aprxlog: Option<String>,
    pub dprslog: Option<String>,
    pub erlangfile: Option<String>,
    pub syslog_facility: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceType {
    Serial,
    Tcp,
    Ax25,
    Agwpe,
    Null,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Protocol {
    Kiss,
    Smack,
    Flexnet,
    #[serde(alias = "BPQCRC")]
    BpqCrc,
    #[serde(alias = "TNC2")]
    Tnc2,
    #[serde(alias = "DPRS")]
    Dprs,
}

#[derive(Debug, Deserialize)]
pub struct InterfaceConfig {
    #[serde(rename = "type")]
    pub iface_type: InterfaceType,
    pub device: Option<String>,
    pub speed: Option<u32>,
    pub protocol: Protocol,
    pub callsign: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub agwpe_port: Option<String>,
    #[serde(default)]
    pub tx_ok: bool,
    #[serde(default = "default_igate_group")]
    pub igate_group: u8,
    #[serde(default = "default_true")]
    pub telem_to_is: bool,
    #[serde(default)]
    pub telem_to_rf: bool,
    pub aliases: Option<Vec<String>>,
    pub initstring: Option<String>,
    pub timeout: Option<u32>,
}

fn default_igate_group() -> u8 { 1 }
fn default_true() -> bool { true }

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BeaconMode {
    Aprsis,
    Radio,
    Both,
}

#[derive(Debug, Deserialize)]
pub struct BeaconConfig {
    pub mode: Option<BeaconMode>,
    pub cycle_size: Option<String>,
    pub symbol: Option<String>,
    pub lat: Option<String>,
    pub lon: Option<String>,
    pub comment: Option<String>,
    pub interface: Option<String>,
    pub srccall: Option<String>,
    pub dstcall: Option<String>,
    pub via: Option<String>,
    pub raw: Option<String>,
    pub file: Option<String>,
    pub exec: Option<String>,
    pub timeout: Option<u32>,
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub item: Option<String>,
    pub object: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelayType {
    Digipeated,
    DigipeatedDirectOnly,
    ThirdParty,
}

#[derive(Debug, Deserialize)]
pub struct DigipeaterSourceConfig {
    pub source: String,
    #[serde(default = "default_relay_type")]
    pub relay_type: RelayType,
    #[serde(default)]
    pub viscous_delay: u8,
    pub ratelimit: Option<[u32; 2]>,
    #[serde(default)]
    pub filters: Vec<String>,
    pub via_path: Option<String>,
    pub msg_path: Option<String>,
    #[serde(default)]
    pub source_regex: Vec<String>,
    #[serde(default)]
    pub destination_regex: Vec<String>,
    #[serde(default)]
    pub via_regex: Vec<String>,
    #[serde(default)]
    pub data_regex: Vec<String>,
}

fn default_relay_type() -> RelayType { RelayType::Digipeated }

#[derive(Debug, Deserialize)]
pub struct DigipeaterConfig {
    pub transmitter: String,
    pub ratelimit: Option<[u32; 2]>,
    pub srcratelimit: Option<[u32; 2]>,
    #[serde(default, rename = "source")]
    pub sources: Vec<DigipeaterSourceConfig>,
}

#[derive(Debug, Deserialize)]
pub struct TelemetryConfig {
    pub transmitter: String,
    pub via: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

impl Config {
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test config::tests`
Expected: PASS

**Step 5: Write the example config file**

Create `config/vaprs.toml.example` with the full documented example from the design doc.

**Step 6: Commit**

```bash
git add -A && git commit -m "feat: TOML configuration parsing with all aprx config options"
```

---

### Task 1.4: Error types

**Files:**
- Create: `src/error.rs`
- Modify: `src/lib.rs`

**Step 1: Define error types**

```rust
// src/error.rs
use thiserror::Error;

#[derive(Error, Debug)]
pub enum VaprsError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("APRS-IS connection error: {0}")]
    AprsIs(String),

    #[error("AX.25 frame error: {0}")]
    Ax25(String),

    #[error("KISS framing error: {0}")]
    Kiss(String),

    #[error("invalid callsign: {0}")]
    Callsign(String),

    #[error("interface error on {interface}: {message}")]
    Interface {
        interface: String,
        message: String,
    },
}

pub type Result<T> = std::result::Result<T, VaprsError>;
```

**Step 2: Update lib.rs**

```rust
pub mod config;
pub mod error;
```

**Step 3: Verify it compiles**

Run: `cargo build`

**Step 4: Commit**

```bash
git add -A && git commit -m "feat: error types with thiserror"
```

---

## Stage 2: AX.25, KISS, and CRC

### Task 2.1: CRC implementations

**Files:**
- Create: `src/crc.rs`
- Modify: `src/lib.rs`

**Step 1: Write CRC tests (from known aprx test vectors)**

```rust
// src/crc.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc16_empty() {
        assert_eq!(crc16(&[]), 0);
    }

    #[test]
    fn test_crc16_known_value() {
        // SMACK CRC-16 of a known KISS frame
        let data = [0x80, 0x01, 0x02, 0x03];
        let crc = crc16(&data);
        // When correct CRC appended, full buffer CRC should be 0
        let mut with_crc = data.to_vec();
        with_crc.push((crc & 0xFF) as u8);
        with_crc.push(((crc >> 8) & 0xFF) as u8);
        assert_eq!(crc16(&with_crc), 0);
    }

    #[test]
    fn test_crc_ccitt() {
        // AX.25 FCS: "TEST" with init 0xFFFF
        let data = b"TEST";
        let crc = crc_ccitt(0xFFFF, data);
        assert_ne!(crc, 0); // just verify it runs
    }

    #[test]
    fn test_crc_flex_known_value() {
        let data = [0x20, 0x01, 0x02, 0x03];
        let crc = crc_flex(&data);
        // When correct CRC appended, result should be 0x7070
        let mut with_crc = data.to_vec();
        with_crc.push(((crc >> 8) & 0xFF) as u8);
        with_crc.push((crc & 0xFF) as u8);
        assert_eq!(crc_flex(&with_crc), 0x7070);
    }

    #[test]
    fn test_crc16_check_valid() {
        let data = [0x00, 0x41, 0x42];
        let crc = crc16(&data);
        let mut buf = data.to_vec();
        buf.push((crc & 0xFF) as u8);
        buf.push(((crc >> 8) & 0xFF) as u8);
        assert!(check_crc16(&buf));
    }

    #[test]
    fn test_crc16_check_invalid() {
        let buf = [0x00, 0x41, 0x42, 0xFF, 0xFF];
        // Very unlikely this is valid
        // We just verify the function runs without panic
        let _ = check_crc16(&buf);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test crc::tests`
Expected: FAIL

**Step 3: Implement CRC functions (direct port of aprx crc.c)**

```rust
// src/crc.rs

/// CRC-16 table (polynomial 0xA001) - used by SMACK KISS variant
pub static CRC16_TABLE: [u16; 256] = [
    0x0000, 0xc0c1, 0xc181, 0x0140, 0xc301, 0x03c0, 0x0280, 0xc241,
    0xc601, 0x06c0, 0x0780, 0xc741, 0x0500, 0xc5c1, 0xc481, 0x0440,
    0xcc01, 0x0cc0, 0x0d80, 0xcd41, 0x0f00, 0xcfc1, 0xce81, 0x0e40,
    0x0a00, 0xcac1, 0xcb81, 0x0b40, 0xc901, 0x09c0, 0x0880, 0xc841,
    0xd801, 0x18c0, 0x1980, 0xd941, 0x1b00, 0xdbc1, 0xda81, 0x1a40,
    0x1e00, 0xdec1, 0xdf81, 0x1f40, 0xdd01, 0x1dc0, 0x1c80, 0xdc41,
    0x1400, 0xd4c1, 0xd581, 0x1540, 0xd701, 0x17c0, 0x1680, 0xd641,
    0xd201, 0x12c0, 0x1380, 0xd341, 0x1100, 0xd1c1, 0xd081, 0x1040,
    0xf001, 0x30c0, 0x3180, 0xf141, 0x3300, 0xf3c1, 0xf281, 0x3240,
    0x3600, 0xf6c1, 0xf781, 0x3740, 0xf501, 0x35c0, 0x3480, 0xf441,
    0x3c00, 0xfcc1, 0xfd81, 0x3d40, 0xff01, 0x3fc0, 0x3e80, 0xfe41,
    0xfa01, 0x3ac0, 0x3b80, 0xfb41, 0x3900, 0xf9c1, 0xf881, 0x3840,
    0x2800, 0xe8c1, 0xe981, 0x2940, 0xeb01, 0x2bc0, 0x2a80, 0xea41,
    0xee01, 0x2ec0, 0x2f80, 0xef41, 0x2d00, 0xedc1, 0xec81, 0x2c40,
    0xe401, 0x24c0, 0x2580, 0xe541, 0x2700, 0xe7c1, 0xe681, 0x2640,
    0x2200, 0xe2c1, 0xe381, 0x2340, 0xe101, 0x21c0, 0x2080, 0xe041,
    0xa001, 0x60c0, 0x6180, 0xa141, 0x6300, 0xa3c1, 0xa281, 0x6240,
    0x6600, 0xa6c1, 0xa781, 0x6740, 0xa501, 0x65c0, 0x6480, 0xa441,
    0x6c00, 0xacc1, 0xad81, 0x6d40, 0xaf01, 0x6fc0, 0x6e80, 0xae41,
    0xaa01, 0x6ac0, 0x6b80, 0xab41, 0x6900, 0xa9c1, 0xa881, 0x6840,
    0x7800, 0xb8c1, 0xb981, 0x7940, 0xbb01, 0x7bc0, 0x7a80, 0xba41,
    0xbe01, 0x7ec0, 0x7f80, 0xbf41, 0x7d00, 0xbdc1, 0xbc81, 0x7c40,
    0xb401, 0x74c0, 0x7580, 0xb541, 0x7700, 0xb7c1, 0xb681, 0x7640,
    0x7200, 0xb2c1, 0xb381, 0x7340, 0xb101, 0x71c0, 0x7080, 0xb041,
    0x5000, 0x90c1, 0x9181, 0x5140, 0x9301, 0x53c0, 0x5280, 0x9241,
    0x9601, 0x56c0, 0x5780, 0x9741, 0x5500, 0x95c1, 0x9481, 0x5440,
    0x9c01, 0x5cc0, 0x5d80, 0x9d41, 0x5f00, 0x9fc1, 0x9e81, 0x5e40,
    0x5a00, 0x9ac1, 0x9b81, 0x5b40, 0x9901, 0x59c0, 0x5880, 0x9841,
    0x8801, 0x48c0, 0x4980, 0x8941, 0x4b00, 0x8bc1, 0x8a81, 0x4a40,
    0x4e00, 0x8ec1, 0x8f81, 0x4f40, 0x8d01, 0x4dc0, 0x4c80, 0x8c41,
    0x4400, 0x84c1, 0x8581, 0x4540, 0x8701, 0x47c0, 0x4680, 0x8641,
    0x8201, 0x42c0, 0x4380, 0x8341, 0x4100, 0x81c1, 0x8081, 0x4040,
];

/// CRC-CCITT table (polynomial 0x8408) - used by AX.25 FCS
pub static CRC_CCITT_TABLE: [u16; 256] = [
    0x0000, 0x1189, 0x2312, 0x329b, 0x4624, 0x57ad, 0x6536, 0x74bf,
    0x8c48, 0x9dc1, 0xaf5a, 0xbed3, 0xca6c, 0xdbe5, 0xe97e, 0xf8f7,
    0x1081, 0x0108, 0x3393, 0x221a, 0x56a5, 0x472c, 0x75b7, 0x643e,
    0x9cc9, 0x8d40, 0xbfdb, 0xae52, 0xdaed, 0xcb64, 0xf9ff, 0xe876,
    0x2102, 0x308b, 0x0210, 0x1399, 0x6726, 0x76af, 0x4434, 0x55bd,
    0xad4a, 0xbcc3, 0x8e58, 0x9fd1, 0xeb6e, 0xfae7, 0xc87c, 0xd9f5,
    0x3183, 0x200a, 0x1291, 0x0318, 0x77a7, 0x662e, 0x54b5, 0x453c,
    0xbdcb, 0xac42, 0x9ed9, 0x8f50, 0xfbef, 0xea66, 0xd8fd, 0xc974,
    0x4204, 0x538d, 0x6116, 0x709f, 0x0420, 0x15a9, 0x2732, 0x36bb,
    0xce4c, 0xdfc5, 0xed5e, 0xfcd7, 0x8868, 0x99e1, 0xab7a, 0xbaf3,
    0x5285, 0x430c, 0x7197, 0x601e, 0x14a1, 0x0528, 0x37b3, 0x263a,
    0xdecd, 0xcf44, 0xfddf, 0xec56, 0x98e9, 0x8960, 0xbbfb, 0xaa72,
    0x6306, 0x728f, 0x4014, 0x519d, 0x2522, 0x34ab, 0x0630, 0x17b9,
    0xef4e, 0xfec7, 0xcc5c, 0xddd5, 0xa96a, 0xb8e3, 0x8a78, 0x9bf1,
    0x7387, 0x620e, 0x5095, 0x411c, 0x35a3, 0x242a, 0x16b1, 0x0738,
    0xffcf, 0xee46, 0xdcdd, 0xcd54, 0xb9eb, 0xa862, 0x9af9, 0x8b70,
    0x8408, 0x9581, 0xa71a, 0xb693, 0xc22c, 0xd3a5, 0xe13e, 0xf0b7,
    0x0840, 0x19c9, 0x2b52, 0x3adb, 0x4e64, 0x5fed, 0x6d76, 0x7cff,
    0x9489, 0x8500, 0xb79b, 0xa612, 0xd2ad, 0xc324, 0xf1bf, 0xe036,
    0x18c1, 0x0948, 0x3bd3, 0x2a5a, 0x5ee5, 0x4f6c, 0x7df7, 0x6c7e,
    0xa50a, 0xb483, 0x8618, 0x9791, 0xe32e, 0xf2a7, 0xc03c, 0xd1b5,
    0x2942, 0x38cb, 0x0a50, 0x1bd9, 0x6f66, 0x7eef, 0x4c74, 0x5dfd,
    0xb58b, 0xa402, 0x9699, 0x8710, 0xf3af, 0xe226, 0xd0bd, 0xc134,
    0x39c3, 0x284a, 0x1ad1, 0x0b58, 0x7fe7, 0x6e6e, 0x5cf5, 0x4d7c,
    0xc60c, 0xd785, 0xe51e, 0xf497, 0x8028, 0x91a1, 0xa33a, 0xb2b3,
    0x4a44, 0x5bcd, 0x6956, 0x78df, 0x0c60, 0x1de9, 0x2f72, 0x3efb,
    0xd68d, 0xc704, 0xf59f, 0xe416, 0x90a9, 0x8120, 0xb3bb, 0xa232,
    0x5ac5, 0x4b4c, 0x79d7, 0x685e, 0x1ce1, 0x0d68, 0x3ff3, 0x2e7a,
    0xe70e, 0xf687, 0xc41c, 0xd595, 0xa12a, 0xb0a3, 0x8238, 0x93b1,
    0x6b46, 0x7acf, 0x4854, 0x59dd, 0x2d62, 0x3ceb, 0x0e70, 0x1ff9,
    0xf78f, 0xe606, 0xd49d, 0xc514, 0xb1ab, 0xa022, 0x92b9, 0x8330,
    0x7bc7, 0x6a4e, 0x58d5, 0x495c, 0x3de3, 0x2c6a, 0x1ef1, 0x0f78,
];

/// FlexNet CRC table
pub static CRC_FLEX_TABLE: [u16; 256] = [
    0x0f87, 0x1e0e, 0x2c95, 0x3d1c, 0x49a3, 0x582a, 0x6ab1, 0x7b38,
    0x83cf, 0x9246, 0xa0dd, 0xb154, 0xc5eb, 0xd462, 0xe6f9, 0xf770,
    0x1f06, 0x0e8f, 0x3c14, 0x2d9d, 0x5922, 0x48ab, 0x7a30, 0x6bb9,
    0x934e, 0x82c7, 0xb05c, 0xa1d5, 0xd56a, 0xc4e3, 0xf678, 0xe7f1,
    0x2e85, 0x3f0c, 0x0d97, 0x1c1e, 0x68a1, 0x7928, 0x4bb3, 0x5a3a,
    0xa2cd, 0xb344, 0x81df, 0x9056, 0xe4e9, 0xf560, 0xc7fb, 0xd672,
    0x3e04, 0x2f8d, 0x1d16, 0x0c9f, 0x7820, 0x69a9, 0x5b32, 0x4abb,
    0xb24c, 0xa3c5, 0x915e, 0x80d7, 0xf468, 0xe5e1, 0xd77a, 0xc6f3,
    0x4d83, 0x5c0a, 0x6e91, 0x7f18, 0x0ba7, 0x1a2e, 0x28b5, 0x393c,
    0xc1cb, 0xd042, 0xe2d9, 0xf350, 0x87ef, 0x9666, 0xa4fd, 0xb574,
    0x5d02, 0x4c8b, 0x7e10, 0x6f99, 0x1b26, 0x0aaf, 0x3834, 0x29bd,
    0xd14a, 0xc0c3, 0xf258, 0xe3d1, 0x976e, 0x86e7, 0xb47c, 0xa5f5,
    0x6c81, 0x7d08, 0x4f93, 0x5e1a, 0x2aa5, 0x3b2c, 0x09b7, 0x183e,
    0xe0c9, 0xf140, 0xc3db, 0xd252, 0xa6ed, 0xb764, 0x85ff, 0x9476,
    0x7c00, 0x6d89, 0x5f12, 0x4e9b, 0x3a24, 0x2bad, 0x1936, 0x08bf,
    0xf048, 0xe1c1, 0xd35a, 0xc2d3, 0xb66c, 0xa7e5, 0x957e, 0x84f7,
    0x8b8f, 0x9a06, 0xa89d, 0xb914, 0xcdab, 0xdc22, 0xeeb9, 0xff30,
    0x07c7, 0x164e, 0x24d5, 0x355c, 0x41e3, 0x506a, 0x62f1, 0x7378,
    0x9b0e, 0x8a87, 0xb81c, 0xa995, 0xdd2a, 0xcca3, 0xfe38, 0xefb1,
    0x1746, 0x06cf, 0x3454, 0x25dd, 0x5162, 0x40eb, 0x7270, 0x63f9,
    0xaa8d, 0xbb04, 0x899f, 0x9816, 0xeca9, 0xfd20, 0xcfbb, 0xde32,
    0x26c5, 0x374c, 0x05d7, 0x145e, 0x60e1, 0x7168, 0x43f3, 0x527a,
    0xba0c, 0xab85, 0x991e, 0x8897, 0xfc28, 0xeda1, 0xdf3a, 0xceb3,
    0x3644, 0x27cd, 0x1556, 0x04df, 0x7060, 0x61e9, 0x5372, 0x42fb,
    0xc98b, 0xd802, 0xea99, 0xfb10, 0x8faf, 0x9e26, 0xacbd, 0xbd34,
    0x45c3, 0x544a, 0x66d1, 0x7758, 0x03e7, 0x126e, 0x20f5, 0x317c,
    0xd90a, 0xc883, 0xfa18, 0xeb91, 0x9f2e, 0x8ea7, 0xbc3c, 0xadb5,
    0x5542, 0x44cb, 0x7650, 0x67d9, 0x1366, 0x02ef, 0x3074, 0x21fd,
    0xe889, 0xf900, 0xcb9b, 0xda12, 0xaead, 0xbf24, 0x8dbf, 0x9c36,
    0x64c1, 0x7548, 0x47d3, 0x565a, 0x22e5, 0x336c, 0x01f7, 0x107e,
    0xf808, 0xe981, 0xdb1a, 0xca93, 0xbe2c, 0xafa5, 0x9d3e, 0x8cb7,
    0x7440, 0x65c9, 0x5752, 0x46db, 0x3264, 0x23ed, 0x1176, 0x00ff,
];

/// Calculate CRC-16 (SMACK variant). Polynomial: X^16 + X^15 + X^2 + 1
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc = ((crc >> 8) & 0xff) ^ CRC16_TABLE[(crc ^ b as u16) as usize & 0xFF];
    }
    crc
}

/// Check CRC-16: returns true if the data (including appended CRC) is valid
pub fn check_crc16(data: &[u8]) -> bool {
    crc16(data) == 0
}

/// Calculate CRC-CCITT (AX.25 FCS). Polynomial: X^16 + X^12 + X^5 + 1
pub fn crc_ccitt(init: u16, data: &[u8]) -> u16 {
    let mut crc = init;
    for &b in data {
        crc = (crc >> 8) ^ CRC_CCITT_TABLE[((crc ^ b as u16) & 0xff) as usize];
    }
    crc
}

/// Calculate FlexNet CRC
pub fn crc_flex(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in data {
        crc = (crc << 8) ^ CRC_FLEX_TABLE[(((crc >> 8) ^ b as u16) & 0xff) as usize];
    }
    crc
}
```

**Step 4: Run tests**

Run: `cargo test crc::tests`
Expected: PASS

**Step 5: Commit**

```bash
git add -A && git commit -m "feat: CRC-16, CRC-CCITT, and FlexNet CRC implementations"
```

---

### Task 2.2: AX.25 address encoding/decoding

**Files:**
- Create: `src/ax25.rs`
- Modify: `src/lib.rs`

**Step 1: Write AX.25 tests**

Tests should cover:
- Encoding a callsign string ("OH2MQK-1") to 7-byte AX.25 format
- Decoding 7-byte AX.25 back to string
- Full AX.25 frame to TNC2 monitor format conversion
- Handling of SSID, H-bit (digipeated marker)
- Bad callsign rejection (spaces, invalid chars, SSID > 15)
- A real AX.25 frame from the aprx source comments

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_callsign_no_ssid() {
        let ax25 = encode_ax25_address("OH2MQK", 0x60).unwrap();
        assert_eq!(ax25[0], b'O' << 1);
        assert_eq!(ax25[1], b'H' << 1);
        assert_eq!(ax25[5], b' ' << 1); // padded
        assert_eq!(ax25[6] & 0x1E, 0); // SSID 0
    }

    #[test]
    fn test_encode_callsign_with_ssid() {
        let ax25 = encode_ax25_address("OH2MQK-15", 0x60).unwrap();
        assert_eq!((ax25[6] >> 1) & 0x0F, 15);
    }

    #[test]
    fn test_decode_callsign() {
        let ax25 = encode_ax25_address("OH2MQK-1", 0x60).unwrap();
        let (call, ssid_byte) = decode_ax25_address(&ax25, false).unwrap();
        assert_eq!(call, "OH2MQK-1");
        assert_eq!(ssid_byte & 0x80, 0); // no H-bit
    }

    #[test]
    fn test_decode_with_hbit() {
        let mut ax25 = encode_ax25_address("WIDE1-1", 0x60).unwrap();
        ax25[6] |= 0x80; // set H-bit
        let (call, _) = decode_ax25_address(&ax25, true).unwrap();
        assert_eq!(call, "WIDE1-1*");
    }

    #[test]
    fn test_ssid_zero_not_printed() {
        let ax25 = encode_ax25_address("APRS", 0xE0).unwrap();
        let (call, _) = decode_ax25_address(&ax25, false).unwrap();
        assert_eq!(call, "APRS");
    }

    #[test]
    fn test_invalid_callsign_lowercase() {
        assert!(encode_ax25_address("oh2mqk", 0x60).is_err());
    }

    #[test]
    fn test_invalid_ssid_too_high() {
        assert!(encode_ax25_address("TEST-16", 0x60).is_err());
    }

    #[test]
    fn test_ax25_frame_to_tnc2() {
        // Build a minimal AX.25 UI frame: SRC>DST with control=0x03, PID=0xF0
        let dst = encode_ax25_address("APRS", 0xE0).unwrap();
        let src_bytes = encode_ax25_address("OH2MQK-1", 0x61).unwrap(); // last addr bit set
        let mut frame = Vec::new();
        frame.extend_from_slice(&dst);
        frame.extend_from_slice(&src_bytes);
        frame.push(0x03); // UI control
        frame.push(0xF0); // APRS PID
        frame.extend_from_slice(b"!6029.50N/02505.43E>");

        let result = ax25_to_tnc2(&frame).unwrap();
        assert!(result.tnc2.starts_with("OH2MQK-1>APRS:"));
        assert!(result.tnc2.contains("!6029.50N/02505.43E>"));
        assert!(result.is_aprs);
    }

    #[test]
    fn test_ax25_frame_with_via() {
        let dst = encode_ax25_address("APRS", 0xE0).unwrap();
        let src = encode_ax25_address("OH2MQK-1", 0x60).unwrap();
        let via = encode_ax25_address("WIDE1-1", 0x61).unwrap(); // last addr
        let mut frame = Vec::new();
        frame.extend_from_slice(&dst);
        frame.extend_from_slice(&src);
        frame.extend_from_slice(&via);
        frame.push(0x03);
        frame.push(0xF0);
        frame.extend_from_slice(b"!test");

        let result = ax25_to_tnc2(&frame).unwrap();
        assert!(result.tnc2.starts_with("OH2MQK-1>APRS,WIDE1-1:"));
    }

    #[test]
    fn test_frame_too_short() {
        let frame = vec![0u8; 10]; // less than 16 bytes minimum
        assert!(ax25_to_tnc2(&frame).is_err());
    }
}
```

**Step 2: Run tests, verify fail**

Run: `cargo test ax25::tests`

**Step 3: Implement AX.25 module**

The implementation should provide:
- `encode_ax25_address(callsign: &str, ssid_flags: u8) -> Result<[u8; 7]>`
- `decode_ax25_address(ax25: &[u8; 7], mark_hbit: bool) -> Result<(String, u8)>`
- `ax25_to_tnc2(frame: &[u8]) -> Result<Tnc2Frame>` with a `Tnc2Frame` struct containing the TNC2 string, address lengths, is_aprs flag, and UI PID

Port the logic directly from aprx `ax25.c`: source address comes from bytes 7-13, destination from 0-6, via addresses follow, control byte must be 0x03 for UI, PID 0xF0 for APRS.

**Step 4: Run tests, verify pass**

Run: `cargo test ax25::tests`

**Step 5: Commit**

```bash
git add -A && git commit -m "feat: AX.25 address encoding/decoding and TNC2 conversion"
```

---

### Task 2.3: KISS protocol framing

**Files:**
- Create: `src/kiss.rs`
- Modify: `src/lib.rs`

**Step 1: Write KISS tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kiss_encode_simple() {
        let data = vec![0x01, 0x02, 0x03];
        let frame = kiss_encode(&data, 0x00, KissVariant::Plain);
        assert_eq!(frame[0], FEND);
        assert_eq!(frame[1], 0x00); // cmd byte
        assert_eq!(frame[2], 0x01);
        assert_eq!(frame[3], 0x02);
        assert_eq!(frame[4], 0x03);
        assert_eq!(frame[5], FEND);
    }

    #[test]
    fn test_kiss_encode_escapes_fend() {
        let data = vec![FEND];
        let frame = kiss_encode(&data, 0x00, KissVariant::Plain);
        // Should contain FESC TFEND instead of FEND
        assert!(frame.windows(2).any(|w| w == [FESC, TFEND]));
        // Should not contain bare FEND in the data portion
        assert_eq!(frame.iter().filter(|&&b| b == FEND).count(), 2); // only start and end
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
        // Multiple FENDs between frames should be handled
        let frame = vec![FEND, FEND, FEND, 0x00, 0x41, FEND];
        let mut decoder = KissDecoder::new();
        let result = decoder.feed(&frame);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_kiss_decode_incremental() {
        // Feed data byte by byte (simulates slow serial reads)
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
        let data = vec![
            FEND, 0x00, 0x41, FEND,
            FEND, 0x00, 0x42, FEND,
        ];
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
        assert!(frame.len() > 6); // data + cmd + 2 CRC bytes + 2 FENDs
    }
}
```

**Step 2: Run tests, verify fail**

**Step 3: Implement KISS module**

Key types:
- `KissVariant` enum: `Plain`, `Smack`, `FlexNet`, `BpqCrc`
- `KissFrame` struct: `cmd_byte: u8`, `data: Vec<u8>`, methods `tnc_id()` and `cmd()`
- `KissDecoder` struct: stateful decoder with `feed(&[u8]) -> Vec<KissFrame>`
- `kiss_encode(data: &[u8], cmd_byte: u8, variant: KissVariant) -> Vec<u8>`

The decoder is a state machine matching aprx's `KISSSTATE_SYNCHUNT / COLLECTING / KISSFESC` states.

**Step 4: Run tests, verify pass**

**Step 5: Commit**

```bash
git add -A && git commit -m "feat: KISS protocol encoder/decoder with SMACK, FlexNet, BPQ variants"
```

---

### Task 2.4: Core packet type

**Files:**
- Create: `src/packet.rs`
- Modify: `src/lib.rs`

**Step 1: Write packet tests**

```rust
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
    }

    #[test]
    fn test_packet_arc_sharing() {
        let pkt = Packet::new("TEST>APRS:test", "port0", true);
        let shared = std::sync::Arc::new(pkt);
        let clone = shared.clone();
        assert_eq!(shared.tnc2, clone.tnc2);
    }
}
```

**Step 2: Implement packet type**

```rust
// src/packet.rs
use std::sync::Arc;
use std::time::Instant;

/// Maximum AX.25 frame size (same as aprx)
pub const MAX_AX25_LEN: usize = 2000;
/// Maximum TNC2 text size
pub const MAX_TNC2_LEN: usize = 2800;

/// Core packet type, shared across subsystems via Arc<Packet>
#[derive(Debug, Clone)]
pub struct Packet {
    /// TNC2 monitor format text (e.g., "SRC>DST,VIA:payload")
    pub tnc2: String,
    /// TNC2 address portion length (up to but not including ':')
    pub tnc2_addr_len: usize,
    /// Raw AX.25 frame bytes (if available)
    pub ax25: Option<Vec<u8>>,
    /// AX.25 address field length
    pub ax25_addr_len: usize,
    /// Source interface name
    pub source_interface: String,
    /// Whether this is an APRS packet (UI frame with PID 0xF0)
    pub is_aprs: bool,
    /// AX.25 UI PID value (-1 if not UI frame)
    pub ui_pid: i16,
    /// When this packet was received
    pub received_at: Instant,
    /// Interface group for igate tracking
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

    /// The payload portion of the TNC2 string (after ':')
    pub fn payload(&self) -> &str {
        if self.tnc2_addr_len + 1 < self.tnc2.len() {
            &self.tnc2[self.tnc2_addr_len + 1..]
        } else {
            ""
        }
    }

    /// The address portion of the TNC2 string (before ':')
    pub fn addresses(&self) -> &str {
        &self.tnc2[..self.tnc2_addr_len]
    }

    /// Extract source callsign from TNC2 format
    pub fn source_call(&self) -> &str {
        self.tnc2.split('>').next().unwrap_or("")
    }

    /// Extract destination callsign from TNC2 format
    pub fn dest_call(&self) -> &str {
        let after_gt = self.tnc2.split('>').nth(1).unwrap_or("");
        after_gt.split([',', ':']).next().unwrap_or("")
    }
}
```

**Step 3: Run tests, verify pass**

**Step 4: Commit**

```bash
git add -A && git commit -m "feat: core Packet type with Arc sharing"
```

---

## Stage 3: Interfaces (Serial/TCP KISS)

### Task 3.1: Interface trait and registry

**Files:**
- Create: `src/interface/mod.rs`
- Modify: `src/lib.rs`

Define the `Interface` trait that serial, TCP, AX.25, and AGWPE implementations will implement. Create the interface registry that maps callsigns to interfaces and manages the collection of all interfaces.

The trait needs:
- `async fn run(self, packet_tx: mpsc::Sender<SharedPacket>, cmd_rx: mpsc::Receiver<InterfaceCommand>)`
- Associated metadata: callsign, interface type, tx capability, igate group

The registry is a `Vec<InterfaceHandle>` where each handle holds the interface metadata plus channel senders for commanding the interface.

**Commit after tests pass.**

---

### Task 3.2: KISS serial port interface

**Files:**
- Create: `src/interface/serial.rs`

Implement async serial port reading using `tokio-serial`. The task:
1. Opens the serial port with configured baud rate and settings
2. Reads bytes into a `KissDecoder`
3. For each complete KISS frame, runs `ax25_to_tnc2()` to convert
4. Sends resulting `SharedPacket` to the router via the mpsc sender
5. Receives transmit commands and calls `kiss_encode()` + serial write

Include a watchdog: if nothing is read for `timeout` seconds, log a warning (matches aprx behavior).

Test with a mock serial port (bytes in, packets out).

**Commit after tests pass.**

---

### Task 3.3: TCP KISS interface

**Files:**
- Create: `src/interface/tcp.rs`

Same as serial but uses `tokio::net::TcpStream`. Includes:
- Auto-reconnect on disconnect with backoff
- Same KISS decoding pipeline
- Configurable read timeout

**Commit after tests pass.**

---

## Stage 4: APRS-IS Client

### Task 4.1: APRS-IS connection and authentication

**Files:**
- Create: `src/aprsis.rs`

Implement the APRS-IS client as an async task:
1. DNS resolution of server hostname
2. TCP connect with timeout
3. Send login line: `user CALL pass PASSCODE vers vaprs VERSION filter FILTER\r\n`
4. Parse server response (lines starting with `#`)
5. Heartbeat monitoring (configurable timeout, default 120s)
6. Auto-reconnect on disconnect with 10-second backoff
7. Read incoming packets, send to router
8. Write outgoing packets from a queue (mpsc receiver)
9. Write buffer management (matches aprx's wrbuf compaction)

Tests should verify:
- Login line formatting
- Passcode validation (the semi-public APRS-IS passcode algorithm)
- Heartbeat timeout detection
- Queue overflow handling (drop packets when buffer full, like aprx)
- Comment line filtering (lines starting with `#`)

**Commit after tests pass.**

---

## Stage 5: Rx-iGate

### Task 5.1: Rx-iGate packet filtering and forwarding

**Files:**
- Create: `src/igate.rs`

Port the igate_to_aprsis() logic from aprx igate.c:
- Forbidden source callsigns: WIDE*, RELAY*, TRACE*, TCPIP*, TCPXX*, NOCALL*, N0CALL*
- Forbidden destination callsigns: TCPIP*, TCPXX*, NOGATE*, RFONLY*, NOCALL*, N0CALL*
- Forbidden via callsigns: RFONLY, NOGATE, TCPIP, TCPXX
- Drop packets with payload starting with '?'
- Handle 3rd-party frames (payload starting with '}') by recursively processing inner frame
- Append q-construct: `,qAR,gatecallsign`
- Forward to APRS-IS write queue

Tests should use known packet strings and verify pass/reject decisions.

**Commit after tests pass.**

---

## Stage 6: Digipeater

### Task 6.1: Duplicate detection

**Files:**
- Create: `src/digipeater/dedupe.rs`

Port aprx's dupecheck: hash-based with configurable store time. Key is (addresses + payload), value is timestamp and seen count.

### Task 6.2: Digipeater engine

**Files:**
- Create: `src/digipeater/mod.rs`

Port the New-N paradigm digipeating from aprx digipeater.c. Handle WIDE1-1, WIDE2-2 etc. via path manipulation. Token bucket rate limiting per transmitter and per source.

### Task 6.3: Viscous delay

**Files:**
- Create: `src/digipeater/viscous.rs`

Delayed processing queue: hold packets for N seconds, drop if heard again during the delay period.

### Task 6.4: APRS-IS style filters

**Files:**
- Create: `src/digipeater/filter.rs`

Port the filter syntax: `a/lat/lon/lat/lon` (area), `b/CALL` (budlist), `m/dist` (my range), `f/CALL/dist` (friend range), regex filters on source/dest/via/data.

**Commit each sub-task separately.**

---

## Stage 7: Tx-iGate

### Task 7.1: History database

**Files:**
- Create: `src/history.rs`

Port aprx's historydb: track recently heard stations with coordinates and timestamps. Used to decide if a station heard on APRS-IS should be gated to RF (the station must have been heard on RF recently).

### Task 7.2: Tx-iGate logic

**Files:**
- Modify: `src/igate.rs`

Port igate_from_aprsis() from aprx igate.c:
- Check forbidden addresses (TCPXX, NOGATE, RFONLY, qAX)
- Drop 3rd-party frames from APRS-IS
- Verify receiving station heard recently on RF (historydb)
- Verify sending station NOT heard recently on RF
- Format as 3rd-party: `}FROMCALL>TOCALL,TCPIP,IGATECALL*:payload`
- Send to transmitter interface

**Commit after tests pass.**

---

## Stage 8: Beacons + Telemetry + Erlang

### Task 8.1: Beacon scheduler

**Files:**
- Create: `src/beacon.rs`

Port beacon.c: circular queue with randomized intervals (80-100% of cycle_size / beacon_count). Supports positional beacons, file-based, exec-based, raw format.

### Task 8.2: Telemetry

**Files:**
- Create: `src/telemetry.rs`

APRS telemetry packet generation sent every 20 minutes.

### Task 8.3: Erlang monitoring

**Files:**
- Create: `src/erlang.rs`

Byte/packet counting per interface at 1min/10min/60min intervals. Backing store via mmap (memmap2 crate).

**Commit each sub-task separately.**

---

## Stage 9: Advanced Features

### Task 9.1: AGWPE interface

**Files:**
- Create: `src/interface/agwpe.rs`

Port agwpesocket.c: AGWPE socket protocol for connecting to AGWPE-compatible software TNCs.

### Task 9.2: D-PRS gateway

**Files:**
- Create: `src/dprsgw.rs`

Port dprsgw.c: D-STAR D-PRS to APRS conversion.

### Task 9.3: Linux kernel AX.25

**Files:**
- Create: `src/interface/ax25_kernel.rs`

Port netax25.c behind `#[cfg(target_os = "linux")]`: promiscuous AX.25 socket listener.

### Task 9.4: APRS message parser

**Files:**
- Create: `src/parse_aprs.rs`

Port parse_aprs.c: extract position, message, object, item data from APRS packets. Used by filter and historydb.

**Commit each sub-task separately.**

---

## Stage 10: Main Event Loop + Integration

### Task 10.1: Router/dispatcher task

**Files:**
- Modify: `src/main.rs`
- Create: `src/router.rs`

Central task that:
1. Receives packets from all interfaces via single mpsc channel
2. Fans out to registered consumers (igate, digipeater, logging, erlang)
3. Each consumer has its own mpsc receiver

### Task 10.2: Logging subsystem

**Files:**
- Create: `src/logging.rs`

RF log (rflog), aprx log, DPRS log with file rotation support via tracing-appender. Matches aprx log format for compatibility with existing log analysis tools.

### Task 10.3: Wire up main.rs

**Files:**
- Modify: `src/main.rs`

Complete main.rs:
1. Parse CLI args
2. Load and validate config
3. Set up tracing/logging
4. Create PID file
5. Daemonize (if not foreground)
6. Signal handling (SIGTERM, SIGINT, SIGHUP → graceful shutdown)
7. Start Tokio `current_thread` runtime
8. Spawn all subsystem tasks
9. Await shutdown signal
10. Clean up (remove PID file)

### Task 10.4: Integration test

**Files:**
- Create: `tests/integration_test.rs`

End-to-end test: feed KISS frames to a mock serial port, verify they appear on a mock APRS-IS connection with correct formatting.

**Commit after tests pass.**

---

## Stage 11: Packaging

### Task 11.1: Systemd service file

**Files:**
- Create: `debian/vaprs.service`
- Create: `debian/vaprs.default`

```ini
# debian/vaprs.service
[Unit]
Description=vaprs APRS iGate and Digipeater
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=-/etc/default/vaprs
ExecStart=/usr/sbin/vaprs -i -f /etc/vaprs/vaprs.toml
Restart=on-failure
RestartSec=10
User=vaprs
Group=vaprs
ProtectSystem=strict
ReadWritePaths=/var/log /var/run
CapabilityBoundingSet=CAP_NET_RAW

[Install]
WantedBy=multi-user.target
```

### Task 11.2: Logrotate config

**Files:**
- Create: `debian/vaprs.logrotate`

```
/var/log/vaprs*.log {
    weekly
    rotate 12
    compress
    delaycompress
    missingok
    notifempty
    copytruncate
}
```

### Task 11.3: cargo-deb metadata and build

Verify `cargo deb` produces a valid `.deb` package. Test install on a Pi 3 (or ARM Docker container).

### Task 11.4: Cross-compilation CI

**Files:**
- Create: `.github/workflows/build.yml` (if using GitHub Actions)

Build for `aarch64-unknown-linux-gnu` and `armv7-unknown-linux-gnueabihf`, produce `.deb` artifacts.

**Commit all packaging files together.**

---

## Execution Notes

- Each stage is independently testable and produces working commits
- Stages 1-5 produce a functional Rx-only iGate (the most common use case)
- Stage 6 adds digipeating capability
- Stages 7-9 add advanced features
- Stage 10 wires everything together
- Stage 11 produces distributable packages
- Total: ~24,000 lines of C being rewritten in approximately 8,000-12,000 lines of Rust (Rust is more concise, and cellmalloc/aprxpolls/netresolver are unnecessary)
