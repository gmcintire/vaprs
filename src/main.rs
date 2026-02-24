use clap::Parser;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use vaprs::aprsis::AprsIsClient;
use vaprs::beacon::BeaconScheduler;
use vaprs::config::{Config, InterfaceType, Protocol};
use vaprs::igate;
use vaprs::interface::tcp::TcpKissInterface;
use vaprs::interface::{Interface, InterfaceHandle, InterfaceRegistry};
use vaprs::logging;
use vaprs::packet::SharedPacket;
use vaprs::router::Router;

/// Default PID file path when running as a daemon.
const PID_FILE_PATH: &str = "/var/run/vaprs.pid";

/// Channel buffer size for the main packet router.
const ROUTER_CHANNEL_SIZE: usize = 256;

/// Channel buffer size for consumer channels.
const CONSUMER_CHANNEL_SIZE: usize = 128;

/// Channel buffer size for interface command channels.
const CMD_CHANNEL_SIZE: usize = 32;

/// Channel buffer size for APRS-IS write channel.
const APRSIS_WRITE_CHANNEL_SIZE: usize = 64;

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

    // Load and validate config
    if !std::path::Path::new(&cli.config).exists() {
        eprintln!("Configuration file not found: {}", cli.config);
        std::process::exit(1);
    }

    // Warn if config file is world-writable (security risk)
    check_config_permissions(&cli.config);

    let config = match Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load configuration: {}", e);
            std::process::exit(1);
        }
    };

    // Set up logging
    let _guards = logging::init_logging(&config, cli.debug, foreground);

    info!(
        config = cli.config,
        mycall = config.mycall,
        "vaprs starting"
    );

    // Create PID file (if not foreground)
    let pid_file_created = if !foreground {
        match write_pid_file(PID_FILE_PATH) {
            Ok(()) => {
                info!(path = PID_FILE_PATH, "PID file created");
                true
            }
            Err(e) => {
                warn!(path = PID_FILE_PATH, error = %e, "failed to create PID file");
                false
            }
        }
    } else {
        false
    };

    // Build and run the Tokio runtime
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            error!(error = %e, "failed to create Tokio runtime");
            std::process::exit(1);
        });

    rt.block_on(async_main(config, cli.erlang));

    // Clean up PID file
    if pid_file_created {
        if let Err(e) = std::fs::remove_file(PID_FILE_PATH) {
            warn!(path = PID_FILE_PATH, error = %e, "failed to remove PID file");
        }
    }

    info!("vaprs stopped");
}

/// Async entry point - runs all tasks until shutdown signal.
async fn async_main(config: Config, erlang_enabled: bool) {
    // Create the central packet channel and router
    let (packet_tx, packet_rx) = mpsc::channel::<SharedPacket>(ROUTER_CHANNEL_SIZE);
    let mut router = Router::new(packet_rx);

    // Set up the interface registry
    let mut registry = InterfaceRegistry::new();

    // Set up Erlang monitor
    let mut erlang_monitor = vaprs::erlang::ErlangMonitor::new();

    // Set up APRS-IS write channel
    let (aprsis_write_tx, aprsis_write_rx) = mpsc::channel::<String>(APRSIS_WRITE_CHANNEL_SIZE);

    // Register a consumer for the iGate (Rx-iGate: RF -> APRS-IS)
    let mut igate_rx = router.add_consumer("igate", CONSUMER_CHANNEL_SIZE);

    // Spawn radio interfaces from config
    for (idx, iface_cfg) in config.interfaces.iter().enumerate() {
        let callsign = iface_cfg
            .callsign
            .as_deref()
            .unwrap_or(&config.mycall)
            .to_string();
        let iface_name = format!("{}_{}", callsign, idx);
        erlang_monitor.add_channel(&iface_name);

        match iface_cfg.iface_type {
            InterfaceType::Serial => {
                let device = match &iface_cfg.device {
                    Some(d) => d.clone(),
                    None => {
                        warn!(
                            interface = iface_name,
                            "serial interface missing device, skipping"
                        );
                        continue;
                    }
                };
                let speed = iface_cfg.speed.unwrap_or(9600);
                let protocol = iface_cfg.protocol.clone().unwrap_or(Protocol::Kiss);

                let iface = Box::new(vaprs::interface::serial::SerialInterface::new(
                    callsign,
                    device,
                    speed,
                    protocol,
                    iface_cfg.tx_ok,
                    iface_cfg.igate_group,
                ));

                let (cmd_tx, cmd_rx) = mpsc::channel(CMD_CHANNEL_SIZE);
                let handle = InterfaceHandle::new(iface.metadata().clone(), cmd_tx);
                registry.register(handle);

                let iface_packet_tx = packet_tx.clone();
                tokio::spawn(async move {
                    iface.run(iface_packet_tx, cmd_rx).await;
                });

                info!(interface = iface_name, "serial interface spawned");
            }
            InterfaceType::Tcp => {
                let host = match &iface_cfg.host {
                    Some(h) => h.clone(),
                    None => {
                        warn!(
                            interface = iface_name,
                            "TCP interface missing host, skipping"
                        );
                        continue;
                    }
                };
                let port = match iface_cfg.port {
                    Some(p) => p,
                    None => {
                        warn!(
                            interface = iface_name,
                            "TCP interface missing port, skipping"
                        );
                        continue;
                    }
                };
                let protocol = iface_cfg.protocol.clone().unwrap_or(Protocol::Kiss);

                let iface = Box::new(TcpKissInterface::new(
                    callsign,
                    host,
                    port,
                    protocol,
                    iface_cfg.tx_ok,
                    iface_cfg.igate_group,
                ));

                let (cmd_tx, cmd_rx) = mpsc::channel(CMD_CHANNEL_SIZE);
                let handle = InterfaceHandle::new(iface.metadata().clone(), cmd_tx);
                registry.register(handle);

                let iface_packet_tx = packet_tx.clone();
                tokio::spawn(async move {
                    iface.run(iface_packet_tx, cmd_rx).await;
                });

                info!(interface = iface_name, "TCP interface spawned");
            }
            _ => {
                warn!(
                    interface = iface_name,
                    iface_type = ?iface_cfg.iface_type,
                    "unsupported interface type, skipping"
                );
            }
        }
    }

    // Spawn APRS-IS client
    let aprsis_handle = if let Some(ref aprsis_cfg) = config.aprsis {
        let client = AprsIsClient::new(&config.mycall, aprsis_cfg);
        let aprsis_packet_tx = packet_tx.clone();
        erlang_monitor.add_channel("APRSIS");
        let handle = tokio::spawn(async move {
            client.run(aprsis_packet_tx, aprsis_write_rx).await;
        });
        info!("APRS-IS client spawned");
        Some(handle)
    } else {
        // No APRS-IS config, just drop the write receiver
        drop(aprsis_write_rx);
        info!("no APRS-IS configuration, skipping");
        None
    };

    // Set up beacon scheduler
    let mut beacon_scheduler = BeaconScheduler::from_config(&config);
    info!(
        beacons = beacon_scheduler.len(),
        "beacon scheduler configured"
    );

    // Spawn the Rx-iGate consumer task
    let gate_call = config.mycall.clone();
    let igate_aprsis_tx = aprsis_write_tx.clone();
    let igate_handle = tokio::spawn(async move {
        while let Some(packet) = igate_rx.recv().await {
            // Only gate packets from RF interfaces (not from APRS-IS)
            if packet.source_interface == "APRSIS" {
                continue;
            }

            if let Some(gated_line) = igate::gate_to_aprsis(&packet, &gate_call) {
                if igate_aprsis_tx.send(gated_line).await.is_err() {
                    break;
                }
            }
        }
    });

    // Spawn beacon timer task
    let beacon_aprsis_tx = aprsis_write_tx.clone();
    let beacon_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let fired = beacon_scheduler.poll();
            for (text, mode, _transmitter) in fired {
                match mode {
                    vaprs::config::BeaconMode::Aprsis | vaprs::config::BeaconMode::Both => {
                        if beacon_aprsis_tx.send(text.clone()).await.is_err() {
                            return;
                        }
                    }
                    vaprs::config::BeaconMode::Radio => {
                        // RF-only beacon would need transmitter lookup; log for now
                        info!(beacon = text, "RF beacon (transmitter not yet wired)");
                    }
                }
            }
        }
    });

    // Spawn Erlang rotation timer (if enabled)
    let erlang_handle = if erlang_enabled {
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                erlang_monitor.rotate_all();
            }
        }))
    } else {
        drop(erlang_monitor);
        None
    };

    // Drop the original packet_tx so the router will stop when all interfaces stop
    drop(packet_tx);

    // Spawn the router
    let router_handle = tokio::spawn(async move {
        router.run().await;
    });

    // Wait for shutdown signal
    info!("all tasks running, waiting for shutdown signal");
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            info!("received shutdown signal");
        }
        Err(e) => {
            error!(error = %e, "failed to listen for shutdown signal");
        }
    }

    // Graceful shutdown: send shutdown to all interfaces
    info!("shutting down interfaces");
    registry.shutdown_all().await;

    // Drop the APRS-IS write channel to signal the client to stop
    drop(aprsis_write_tx);

    // Wait for tasks to complete (with timeout)
    let shutdown_timeout = std::time::Duration::from_secs(10);

    if let Some(handle) = aprsis_handle {
        let _ = tokio::time::timeout(shutdown_timeout, handle).await;
    }

    // Abort background tasks
    igate_handle.abort();
    beacon_handle.abort();
    if let Some(handle) = erlang_handle {
        handle.abort();
    }

    let _ = tokio::time::timeout(shutdown_timeout, router_handle).await;

    info!("shutdown complete");
}

/// Write the current process PID to a file.
///
/// Uses create_new to fail if the file already exists (another instance running).
fn write_pid_file(path: &str) -> std::io::Result<()> {
    use std::io::Write;
    let pid = std::process::id();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    writeln!(file, "{}", pid)
}

/// Check config file permissions and warn if world-writable.
#[cfg(unix)]
fn check_config_permissions(path: &str) {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.mode();
        if mode & 0o002 != 0 {
            eprintln!(
                "WARNING: config file {} is world-writable (mode {:o}). \
                 This is a security risk — it may contain APRS-IS credentials \
                 and beacon exec commands.",
                path,
                mode & 0o777
            );
        }
        if mode & 0o020 != 0 {
            eprintln!(
                "WARNING: config file {} is group-writable (mode {:o}).",
                path,
                mode & 0o777
            );
        }
    }
}

#[cfg(not(unix))]
fn check_config_permissions(_path: &str) {}
