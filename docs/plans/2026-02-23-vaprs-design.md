# vaprs - Rust APRS iGate/Digipeater

## Overview

vaprs is a full Rust reimplementation of [aprx](https://github.com/PhirePhly/aprx/), a multitalented APRS iGate and digipeater. It provides the same functionality as aprx v2.9 with a modern Rust codebase, async I/O via Tokio, TOML-based configuration, and native Debian packaging support.

## Goals

- Feature parity with aprx v2.9 (all subsystems)
- Clean async architecture using Tokio
- TOML configuration format (no legacy aprx.conf compat)
- Native Debian package building via cargo-deb
- Systemd service integration
- Cross-compilable for common Linux targets (x86_64, armhf, aarch64)
- Primary target: Raspberry Pi 3 (ARM Cortex-A53, 1GB RAM)

## Pi 3 Efficiency Constraints

- **Single-threaded Tokio** (`current_thread` flavor) — matches aprx's single-thread design, avoids thread overhead
- **`mpsc` channels with explicit fan-out** instead of `broadcast` (broadcast clones every message)
- **Stack-allocated buffers** where possible — packet buffers use fixed arrays, not heap Vec
- **`SmallVec`/`ArrayVec`** for callsign fields and small collections
- **Inline CRC tables** — same as aprx, no runtime generation
- **Release profile**: LTO enabled, `opt-level = "s"` (size), `codegen-units = 1`, strip symbols
- **Cross-compile targets**: `aarch64-unknown-linux-gnu` (64-bit) or `armv7-unknown-linux-gnueabihf` (32-bit)
- **Minimal dependencies** — avoid heavy crates where stdlib or small crates suffice

## Architecture

### Event Loop

The original aprx uses a single-threaded `poll()` event loop with prepoll/postpoll callbacks. vaprs translates this to Tokio's async runtime where each subsystem runs as an independent async task communicating via channels.

### Data Flow

```
Radio Interfaces (KISS serial/TCP, AX.25, AGWPE)
    │
    ▼
┌─────────────┐     ┌──────────────┐     ┌──────────┐
│  AX.25/TNC2 │────▶│  Router /    │────▶│ APRS-IS  │
│  Decoder    │     │  Dispatcher  │     │ Client   │
└─────────────┘     └──────┬───────┘     └────┬─────┘
                           │                  │
                    ┌──────┴──────┐     ┌─────┴──────┐
                    │ Digipeater  │     │ Tx-iGate   │
                    │ (viscous)   │     │ (3rd-party)│
                    └──────┬──────┘     └─────┬──────┘
                           │                  │
                           ▼                  ▼
                    Radio Interfaces (TX)
```

### Inter-Task Communication

A central router task receives all packets via a single `mpsc` channel and fans out to registered consumers via per-consumer `mpsc` senders. This avoids the clone-per-subscriber overhead of broadcast channels and keeps memory predictable on the Pi 3's 1GB RAM:
- Digipeater engine
- Rx-iGate (RF → APRS-IS)
- Tx-iGate (APRS-IS → RF)
- Duplicate checker
- Telemetry/erlang counters
- Loggers

### Core Packet Type

The `Packet` struct (analogous to aprx's `pbuf_t`) is the central data type:
- Contains both raw AX.25 bytes and TNC2 text representation
- Reference-counted via `Arc` for zero-copy sharing across tasks
- Carries metadata: source interface, timestamps, APRS parse results, flags

## Module Structure

```
src/
├── main.rs                 # CLI args, daemon setup, signal handling
├── config.rs               # TOML config parsing & validation
├── packet.rs               # Core Packet type (Arc-wrapped, like pbuf_t)
├── ax25.rs                 # AX.25 address encoding/decoding, TNC2 conversion
├── kiss.rs                 # KISS protocol framing (FEND/FESC)
├── crc.rs                  # CRC-16, CRC-CCITT, FlexNet CRC
├── aprsis.rs               # APRS-IS client (connect, auth, send/receive)
├── interface/
│   ├── mod.rs              # Interface trait + registry
│   ├── serial.rs           # Serial port KISS/TNC2 (tokio-serial)
│   ├── tcp.rs              # TCP KISS connections
│   ├── ax25_kernel.rs      # Linux kernel AX.25 (cfg linux)
│   └── agwpe.rs            # AGWPE socket interface
├── digipeater/
│   ├── mod.rs              # Digipeater engine
│   ├── viscous.rs          # Viscous delay queue
│   ├── dedupe.rs           # Duplicate detection
│   └── filter.rs           # APRS-IS style filters + regex filters
├── igate.rs                # Rx-iGate + Tx-iGate logic
├── beacon.rs               # Beacon scheduler & formatting
├── telemetry.rs            # APRS telemetry generation
├── erlang.rs               # Channel erlang/byte counting
├── dprsgw.rs               # D-PRS to APRS gateway
├── history.rs              # History database (for Tx-iGate decisions)
├── parse_aprs.rs           # APRS packet parser (position, message, etc.)
├── logging.rs              # RF log, aprx log, syslog integration
└── util/
    ├── mod.rs
    ├── callsign.rs         # Callsign validation, SSID handling
    └── time.rs             # Monotonic time utilities
```

## Configuration Format

TOML-based. See `config/vaprs.toml.example` for the full reference.

```toml
mycall = "N0CALL-1"

[location]
lat = "0000.00N"
lon = "00000.00E"

[aprsis]
passcode = -1
servers = ["rotate.aprs2.net:14580"]
filter = "m/100"
heartbeat_timeout = 120

[logging]
pidfile = "/var/run/vaprs.pid"
rflog = "/var/log/vaprs-rf.log"
aprxlog = "/var/log/vaprs.log"

[[interface]]
type = "serial"
device = "/dev/ttyUSB0"
speed = 19200
protocol = "KISS"
callsign = "N0CALL-1"
tx_ok = false
igate_group = 1

[[interface]]
type = "tcp"
host = "192.0.2.10"
port = 10001
protocol = "KISS"

[[beacon]]
symbol = "R&"
comment = "Rx-only iGate"
cycle_size = "20m"

[[digipeater]]
transmitter = "N0CALL-1"
ratelimit = [60, 120]

[[digipeater.source]]
source = "N0CALL-1"
relay_type = "digipeated"
viscous_delay = 0

[[telemetry]]
transmitter = "N0CALL-1"
sources = ["N0CALL-1"]
```

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| tokio | Async runtime (TCP, timers, tasks, signals) |
| tokio-serial | Async serial port access |
| serde + toml | Config parsing |
| tracing + tracing-subscriber | Structured logging, syslog |
| clap | CLI argument parsing |
| regex | Filter regex support |
| crc | CRC calculations |
| bytes | Efficient byte buffers |
| nix | Unix signals, sockets |
| thiserror | Error types |
| memmap2 | mmap for erlang backing store |
| cargo-deb | Debian package generation |

## Feature Mapping (aprx → vaprs)

| aprx subsystem | vaprs module | Notes |
|---------------|-------------|-------|
| aprx.c (main loop) | main.rs | Tokio runtime replaces poll() loop |
| config.c | config.rs | TOML instead of Apache-style |
| aprsis.c | aprsis.rs | Async TCP with auto-reconnect |
| ttyreader.c | interface/serial.rs | tokio-serial |
| kiss.c | kiss.rs | Same KISS framing logic |
| ax25.c | ax25.rs | Same AX.25 ↔ TNC2 conversion |
| interface.c | interface/mod.rs | Trait-based interface registry |
| digipeater.c | digipeater/mod.rs | Same New-N logic |
| dupecheck.c | digipeater/dedupe.rs | Same hash-based dedup |
| filter.c | digipeater/filter.rs | Same APRS-IS filter syntax |
| igate.c | igate.rs | Same Rx/Tx igate rules |
| beacon.c | beacon.rs | Same randomized scheduling |
| telemetry.c | telemetry.rs | Same APRS telemetry format |
| erlang.c | erlang.rs | Same byte/packet counting |
| historydb.c | history.rs | Same heard-station tracking |
| parse_aprs.c | parse_aprs.rs | Same APRS format parsing |
| dprsgw.c | dprsgw.rs | D-PRS → APRS conversion |
| agwpesocket.c | interface/agwpe.rs | AGWPE socket protocol |
| netax25.c | interface/ax25_kernel.rs | Linux-only, behind cfg gate |
| pbuf.c | packet.rs | Arc<Packet> replaces refcounted pbuf |
| crc.c | crc.rs | Same CRC algorithms |
| cellmalloc.c | (not needed) | Rust allocator handles this |
| aprxpolls.c | (not needed) | Tokio replaces poll management |
| netresolver.c | (tokio built-in) | Tokio's async DNS resolution |
| aprx-stat.c | (future: vaprs-stat) | Separate binary, reads erlang mmap |

## Debian Packaging

Using `cargo-deb` with metadata in `Cargo.toml`:

```toml
[package.metadata.deb]
maintainer = "..."
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

Systemd service file replaces the SysV init script. Includes:
- `vaprs.service` - main service unit
- `vaprs.default` - environment defaults
- logrotate configuration
