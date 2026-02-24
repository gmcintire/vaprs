# vaprs

APRS iGate and digipeater for Linux, written in Rust. A reimplementation of [aprx](https://github.com/PhirePhly/aprx).

## Features

- **Rx/Tx iGate** — bidirectional gateway between RF and APRS-IS
- **Digipeater** — New-N paradigm with viscous delay, duplicate detection, and filtering
- **Interfaces** — Serial KISS, TCP KISS, AGWPE, Linux kernel AX.25
- **Beacons** — Position, raw, file, and exec beacons with configurable intervals
- **Telemetry & monitoring** — APRS telemetry generation and per-interface Erlang statistics
- **D-PRS gateway** — D-STAR position reporting to APRS

## Installing

Packages are available for Debian/Ubuntu (amd64, arm64, armhf):

```sh
curl -fsSL https://vaprs.w5isp.com/gpg.key | sudo gpg --dearmor -o /usr/share/keyrings/vaprs.gpg
echo "deb [signed-by=/usr/share/keyrings/vaprs.gpg] https://vaprs.w5isp.com stable main" | sudo tee /etc/apt/sources.list.d/vaprs.list
sudo apt update && sudo apt install vaprs
```

Updates are picked up automatically by `apt upgrade`.

## Building from source

```sh
cargo build --release
```

The release binary is optimized for size (`opt-level = "s"`, LTO, single codegen unit) — suitable for Raspberry Pi and similar SBCs.

## Building .deb packages

```sh
cargo install cargo-deb
cargo deb
```

Cross-compile for ARM:

```sh
cargo deb --target aarch64-unknown-linux-gnu
cargo deb --target armv7-unknown-linux-gnueabihf
```

## Configuration

vaprs uses TOML configuration. See [`config/vaprs.toml.example`](config/vaprs.toml.example) for a complete example.

```sh
vaprs -f /etc/vaprs/vaprs.toml
```

Location can be specified as decimal degrees or traditional APRS format:

```toml
[location]
lat = 33.45
lon = -96.78

# Or APRS format (DDMM.MMN / DDDMM.MMW):
# lat = "3327.00N"
# lon = "09646.80W"
```

## Usage

```
vaprs [OPTIONS]

Options:
  -f <FILE>    Configuration file path
  -d           Enable debug logging
  -v           Enable verbose logging
  -e           Enable Erlang monitoring
  -i           Run in foreground (don't daemonize)
  -L           Log APRS-IS traffic
```

## License

GPL-2.0-or-later
