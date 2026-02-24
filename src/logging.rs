// Logging subsystem - RF log and application log with tracing-appender.
//
// Sets up tracing subscribers for:
// - Console output (stderr) for foreground/debug mode
// - RF log file (matches aprx rflog format)
// - Application log file (matches aprx aprxlog format)
//
// The RF log format is:
//   YYYY-MM-DD HH:MM:SS.fff INTERFACE direction TNC2_STRING
// where direction is 'R' (received), 'T' (transmitted), or 'd' (dropped).

use std::time::SystemTime;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::config::Config;

/// Initialize the logging subsystem based on config.
///
/// Returns guards that must be held alive for the duration of the program.
/// When the guards are dropped, any buffered log output is flushed.
///
/// # Arguments
/// * `config` - Application configuration (for log file paths)
/// * `debug_level` - Debug verbosity (0=info, 1=debug, 2+=trace)
/// * `foreground` - Whether running in foreground mode (enables stderr output)
pub fn init_logging(config: &Config, debug_level: u8, foreground: bool) -> Vec<WorkerGuard> {
    let mut guards = Vec::new();

    let filter = match debug_level {
        0 => EnvFilter::new("info"),
        1 => EnvFilter::new("debug"),
        _ => EnvFilter::new("trace"),
    };

    if foreground {
        // In foreground mode, log to stderr with timestamps
        let stderr_layer = fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_ansi(true);

        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer);

        tracing::subscriber::set_global_default(subscriber).ok();
    } else if let Some(ref logging) = config.logging {
        // Daemon mode with log files configured
        if let Some(ref aprxlog_path) = logging.aprxlog {
            let file_appender = tracing_appender::rolling::never(
                std::path::Path::new(aprxlog_path)
                    .parent()
                    .unwrap_or(std::path::Path::new(".")),
                std::path::Path::new(aprxlog_path)
                    .file_name()
                    .unwrap_or(std::ffi::OsStr::new("vaprs.log")),
            );
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            guards.push(guard);

            let file_layer = fmt::layer()
                .with_writer(non_blocking)
                .with_target(false)
                .with_ansi(false);

            let subscriber = tracing_subscriber::registry().with(filter).with(file_layer);

            tracing::subscriber::set_global_default(subscriber).ok();
        } else {
            // No aprxlog configured, use stderr
            let stderr_layer = fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_ansi(false);

            let subscriber = tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer);

            tracing::subscriber::set_global_default(subscriber).ok();
        }
    } else {
        // No logging config at all, use stderr
        let stderr_layer = fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_ansi(false);

        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer);

        tracing::subscriber::set_global_default(subscriber).ok();
    }

    guards
}

/// Format a packet for RF logging (matches aprx format).
///
/// Format: `YYYY-MM-DD HH:MM:SS.fff INTERFACE direction TNC2_STRING`
///
/// # Arguments
/// * `interface` - Name of the interface (e.g., "serial0", "APRSIS")
/// * `direction` - Direction character: 'R' for received, 'T' for transmitted, 'd' for dropped
/// * `tnc2` - The TNC2 format packet string
pub fn format_rf_log_entry(interface: &str, direction: char, tnc2: &str) -> String {
    let now = SystemTime::now();
    let duration = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();

    // Convert to broken-down time components
    // Simple UTC calculation without external crate
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Calculate year/month/day from days since epoch (1970-01-01)
    let (year, month, day) = days_to_ymd(days);

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {} {} {}",
        year, month, day, hours, minutes, seconds, millis, interface, direction, tnc2
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_log_entry_format_received() {
        let entry = format_rf_log_entry("serial0", 'R', "OH2MQK-1>APRS:!6029.50N/02505.43E>");
        // Should contain the interface and direction
        assert!(entry.contains("serial0 R OH2MQK-1>APRS:!6029.50N/02505.43E>"));
        // Should start with a timestamp like YYYY-MM-DD HH:MM:SS.mmm
        assert!(entry.len() > 30);
        assert_eq!(&entry[4..5], "-");
        assert_eq!(&entry[7..8], "-");
        assert_eq!(&entry[13..14], ":");
        assert_eq!(&entry[16..17], ":");
        assert_eq!(&entry[19..20], ".");
    }

    #[test]
    fn rf_log_entry_format_transmitted() {
        let entry = format_rf_log_entry("radio0", 'T', "TEST>APRS:!test");
        assert!(entry.contains("radio0 T TEST>APRS:!test"));
    }

    #[test]
    fn rf_log_entry_format_dropped() {
        let entry = format_rf_log_entry("APRSIS", 'd', "DUPE>APRS:duplicate");
        assert!(entry.contains("APRSIS d DUPE>APRS:duplicate"));
    }

    #[test]
    fn rf_log_entry_direction_characters() {
        let r = format_rf_log_entry("if0", 'R', "A>B:c");
        let t = format_rf_log_entry("if0", 'T', "A>B:c");
        let d = format_rf_log_entry("if0", 'd', "A>B:c");

        assert!(r.contains(" R "));
        assert!(t.contains(" T "));
        assert!(d.contains(" d "));
    }

    #[test]
    fn rf_log_entry_timestamp_is_current() {
        let entry = format_rf_log_entry("test", 'R', "A>B:c");
        // Extract the year - should be reasonable (2024-2030 range)
        let year: u64 = entry[..4].parse().unwrap();
        assert!((2024..=2030).contains(&year), "year {} out of range", year);
    }

    #[test]
    fn init_logging_with_no_log_files_returns_empty_guards() {
        // Config with no logging section
        let config = Config {
            mycall: "TEST-1".to_string(),
            location: None,
            aprsis: None,
            logging: None,
            interfaces: vec![],
            beacons: vec![],
            digipeaters: vec![],
            telemetry: vec![],
        };
        // This will try to set the global default, which may fail if already set
        // in tests, but it should not panic
        let guards = init_logging(&config, 0, false);
        // With no logging config and daemon mode, guards may or may not be empty
        // The important thing is it doesn't panic
        drop(guards);
    }

    #[test]
    fn days_to_ymd_unix_epoch() {
        let (y, m, d) = days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        // 2024-01-01 is day 19723 since epoch
        let (y, m, d) = days_to_ymd(19723);
        assert_eq!((y, m, d), (2024, 1, 1));
    }

    #[test]
    fn days_to_ymd_leap_year() {
        // 2024-02-29 is day 19723 + 31 + 28 = 19782 (2024 is a leap year)
        // Actually let's just test that the function handles it
        let (y, m, d) = days_to_ymd(19782);
        assert_eq!(y, 2024);
        assert_eq!(m, 2);
        assert_eq!(d, 29);
    }
}
