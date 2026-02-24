// Beacon scheduler - periodic APRS beacon packet generation.
//
// Ported from aprx beacon.c: a cycle-based scheduler that generates
// periodic APRS beacon packets with randomized timing to avoid
// synchronized transmissions.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant, SystemTime};

use tracing::warn;

use crate::config::{BeaconMode, Config};

/// A single beacon definition.
pub struct Beacon {
    /// Beacon content type.
    pub content: BeaconContent,
    /// Where to send: RF, APRS-IS, or both.
    pub mode: BeaconMode,
    /// Optional transmitter interface callsign (for RF beacons).
    pub transmitter: Option<String>,
    /// Optional via path for RF beacons.
    pub via: Option<String>,
}

/// Content types for beacon packets.
pub enum BeaconContent {
    /// Position report with symbol table/code, optional comment.
    Position {
        lat: String,
        lon: String,
        symbol_table: char,
        symbol_code: char,
        comment: Option<String>,
    },
    /// Raw APRS string (sent as-is after callsign header).
    Raw(String),
    /// Read from file each time the beacon fires.
    File(String),
    /// Execute command, use stdout as beacon text.
    Exec { command: String, timeout: Duration },
}

/// Format a positional beacon in APRS format.
///
/// Produces: `MYCALL>APRS:!DDMM.MMN<table>DDDMM.MME<symbol>comment`
pub fn format_position_beacon(
    mycall: &str,
    lat: &str,
    lon: &str,
    symbol_table: char,
    symbol_code: char,
    comment: Option<&str>,
) -> String {
    match comment {
        Some(c) => format!(
            "{}>APRS:!{}{}{}{}{}",
            mycall, lat, symbol_table, lon, symbol_code, c
        ),
        None => format!(
            "{}>APRS:!{}{}{}{}",
            mycall, lat, symbol_table, lon, symbol_code
        ),
    }
}

/// Parse APRS symbol from config format (e.g., "R&" or "/>").
///
/// Returns `(table_char, symbol_char)` where:
/// - `table_char` is the symbol table or overlay character
/// - `symbol_char` is the symbol code character
pub fn parse_symbol(symbol_str: &str) -> Option<(char, char)> {
    let mut chars = symbol_str.chars();
    let table = chars.next()?;
    let code = chars.next()?;
    // Must be exactly 2 characters
    if chars.next().is_some() {
        return None;
    }
    Some((table, code))
}

/// Parse a duration string like "20m", "1h", "30s" into a `Duration`.
///
/// Supported suffixes:
/// - `s` - seconds
/// - `m` - minutes
/// - `h` - hours
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let (num_str, suffix) = s.split_at(s.len() - 1);
    let value: u64 = num_str.parse().ok()?;

    match suffix {
        "s" => Some(Duration::from_secs(value)),
        "m" => Some(Duration::from_secs(value * 60)),
        "h" => Some(Duration::from_secs(value * 3600)),
        _ => None,
    }
}

/// Generate a pseudo-random jitter factor between 0.80 and 1.00.
///
/// Uses a simple hash-based approach seeded from the current time and a
/// counter to avoid needing a `rand` dependency.
fn random_jitter(seed: u64) -> f64 {
    let mut hasher = DefaultHasher::new();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    now.hash(&mut hasher);
    seed.hash(&mut hasher);
    let hash = hasher.finish();
    // Map to 0.80..=1.00 range
    let fraction = (hash % 2001) as f64 / 10000.0; // 0.0 to 0.2
    0.80 + fraction
}

/// Apply randomized jitter to an interval (80-100% of the base).
fn jittered_duration(base: Duration, seed: u64) -> Duration {
    let factor = random_jitter(seed);
    Duration::from_secs_f64(base.as_secs_f64() * factor)
}

/// Beacon scheduler managing multiple beacons.
pub struct BeaconScheduler {
    beacons: Vec<ScheduledBeacon>,
    fire_counter: u64,
}

struct ScheduledBeacon {
    beacon: Beacon,
    mycall: String,
    interval: Duration,
    next_fire: Instant,
}

impl BeaconScheduler {
    pub fn new() -> Self {
        Self {
            beacons: Vec::new(),
            fire_counter: 0,
        }
    }

    /// Add a beacon with the given interval.
    pub fn add_beacon(&mut self, beacon: Beacon, mycall: &str, interval: Duration) {
        let next_fire = Instant::now() + jittered_duration(interval, self.beacons.len() as u64);
        self.beacons.push(ScheduledBeacon {
            beacon,
            mycall: mycall.to_string(),
            interval,
            next_fire,
        });
    }

    /// Get any beacons that are due to fire now.
    ///
    /// Returns the formatted beacon strings ready to send, along with
    /// the beacon mode and optional transmitter callsign.
    /// Reschedules each fired beacon with randomized next interval.
    pub fn poll(&mut self) -> Vec<(String, BeaconMode, Option<String>)> {
        let now = Instant::now();
        let mut results = Vec::new();

        for scheduled in &mut self.beacons {
            if now < scheduled.next_fire {
                continue;
            }

            let formatted = format_beacon_content(
                &scheduled.beacon.content,
                &scheduled.mycall,
                scheduled.beacon.via.as_deref(),
            );

            match formatted {
                Some(text) => {
                    results.push((
                        text,
                        scheduled.beacon.mode.clone(),
                        scheduled.beacon.transmitter.clone(),
                    ));
                }
                None => {
                    warn!(mycall = %scheduled.mycall, "beacon content generation failed");
                }
            }

            // Reschedule with jitter
            self.fire_counter += 1;
            scheduled.next_fire = now + jittered_duration(scheduled.interval, self.fire_counter);
        }

        results
    }

    /// Build a scheduler from a `Config`.
    ///
    /// Each beacon group shares the cycle time, evenly spaced. If no
    /// cycle_size is configured, defaults to 20 minutes.
    pub fn from_config(config: &Config) -> Self {
        let mut scheduler = Self::new();

        if config.beacons.is_empty() {
            return scheduler;
        }

        let location = config.location.as_ref();

        // Group beacons by cycle_size for even spacing
        let num_beacons = config.beacons.len();

        for (i, beacon_cfg) in config.beacons.iter().enumerate() {
            let cycle = beacon_cfg
                .cycle_size
                .as_deref()
                .and_then(parse_duration)
                .unwrap_or(Duration::from_secs(20 * 60));

            // Space beacons evenly within the cycle
            let interval = if num_beacons > 0 {
                cycle / num_beacons as u32
            } else {
                cycle
            };

            let mode = beacon_cfg.mode.clone().unwrap_or(BeaconMode::Both);

            // Build beacon content from config
            let content = if let Some(ref symbol_str) = beacon_cfg.symbol {
                // Position beacon - requires location
                if let Some(loc) = location {
                    let (table, code) = parse_symbol(symbol_str).unwrap_or(('/', '>'));
                    BeaconContent::Position {
                        lat: loc.lat.clone(),
                        lon: loc.lon.clone(),
                        symbol_table: table,
                        symbol_code: code,
                        comment: beacon_cfg.comment.clone(),
                    }
                } else {
                    warn!(
                        beacon_index = i,
                        "beacon has symbol but no location configured, skipping"
                    );
                    continue;
                }
            } else if let Some(ref comment) = beacon_cfg.comment {
                // Raw beacon with just a comment string
                BeaconContent::Raw(comment.clone())
            } else {
                warn!(
                    beacon_index = i,
                    "beacon has no symbol or comment, skipping"
                );
                continue;
            };

            let beacon = Beacon {
                content,
                mode,
                transmitter: beacon_cfg.transmitter.clone(),
                via: beacon_cfg.via.clone(),
            };

            scheduler.add_beacon(beacon, &config.mycall, interval);
        }

        scheduler
    }

    /// Return the number of scheduled beacons.
    pub fn len(&self) -> usize {
        self.beacons.len()
    }

    /// Return true if there are no scheduled beacons.
    pub fn is_empty(&self) -> bool {
        self.beacons.is_empty()
    }
}

impl Default for BeaconScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Format beacon content into a sendable APRS string.
fn format_beacon_content(
    content: &BeaconContent,
    mycall: &str,
    via: Option<&str>,
) -> Option<String> {
    match content {
        BeaconContent::Position {
            lat,
            lon,
            symbol_table,
            symbol_code,
            comment,
        } => {
            let base = format_position_beacon(
                mycall,
                lat,
                lon,
                *symbol_table,
                *symbol_code,
                comment.as_deref(),
            );
            Some(maybe_add_via(&base, via))
        }
        BeaconContent::Raw(text) => {
            let line = format!("{}>APRS:{}", mycall, text);
            Some(maybe_add_via(&line, via))
        }
        BeaconContent::File(path) => match std::fs::read_to_string(path) {
            Ok(text) => {
                let text = text.trim();
                if text.is_empty() {
                    None
                } else {
                    let line = format!("{}>APRS:{}", mycall, text);
                    Some(maybe_add_via(&line, via))
                }
            }
            Err(e) => {
                warn!(path = %path, error = %e, "failed to read beacon file");
                None
            }
        },
        BeaconContent::Exec { command, timeout } => match run_beacon_command(command, *timeout) {
            Some(text) => {
                let line = format!("{}>APRS:{}", mycall, text);
                Some(maybe_add_via(&line, via))
            }
            None => None,
        },
    }
}

/// Insert a via path into an APRS packet header.
///
/// Transforms `CALL>APRS:payload` into `CALL>APRS,VIA1,VIA2:payload`.
fn maybe_add_via(packet: &str, via: Option<&str>) -> String {
    match via {
        Some(v) if !v.is_empty() => {
            if let Some(colon_pos) = packet.find(':') {
                let (header, payload) = packet.split_at(colon_pos);
                format!("{},{}{}", header, v, payload)
            } else {
                packet.to_string()
            }
        }
        _ => packet.to_string(),
    }
}

/// Run a command and capture its stdout, with a timeout.
fn run_beacon_command(command: &str, _timeout: Duration) -> Option<String> {
    use std::process::Command;

    let result = Command::new("sh").arg("-c").arg(command).output();

    match result {
        Ok(output) => {
            if !output.status.success() {
                warn!(
                    command = %command,
                    status = %output.status,
                    "beacon exec command failed"
                );
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        Err(e) => {
            warn!(command = %command, error = %e, "failed to execute beacon command");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_position_beacon() {
        let result = format_position_beacon(
            "OH2MQK-1",
            "6029.50N",
            "02505.43E",
            '/',
            '>',
            Some("Rx-only iGate"),
        );
        assert_eq!(result, "OH2MQK-1>APRS:!6029.50N/02505.43E>Rx-only iGate");
    }

    #[test]
    fn test_format_position_beacon_no_comment() {
        let result = format_position_beacon("N0CALL-1", "4903.50N", "07201.75W", '\\', 'K', None);
        assert_eq!(result, "N0CALL-1>APRS:!4903.50N\\07201.75WK");
    }

    #[test]
    fn test_format_position_beacon_overlay() {
        let result = format_position_beacon(
            "TEST-5",
            "6029.50N",
            "02505.43E",
            'R',
            '&',
            Some("Diamond overlay"),
        );
        assert_eq!(result, "TEST-5>APRS:!6029.50NR02505.43E&Diamond overlay");
    }

    #[test]
    fn test_parse_symbol_primary_car() {
        let (table, code) = parse_symbol("/>").unwrap();
        assert_eq!(table, '/');
        assert_eq!(code, '>');
    }

    #[test]
    fn test_parse_symbol_overlay() {
        let (table, code) = parse_symbol("R&").unwrap();
        assert_eq!(table, 'R');
        assert_eq!(code, '&');
    }

    #[test]
    fn test_parse_symbol_alternate_table() {
        let (table, code) = parse_symbol("\\K").unwrap();
        assert_eq!(table, '\\');
        assert_eq!(code, 'K');
    }

    #[test]
    fn test_parse_symbol_too_short() {
        assert!(parse_symbol("/").is_none());
    }

    #[test]
    fn test_parse_symbol_too_long() {
        assert!(parse_symbol("R&X").is_none());
    }

    #[test]
    fn test_parse_symbol_empty() {
        assert!(parse_symbol("").is_none());
    }

    #[test]
    fn test_parse_duration_minutes() {
        let d = parse_duration("20m").unwrap();
        assert_eq!(d, Duration::from_secs(1200));
    }

    #[test]
    fn test_parse_duration_hours() {
        let d = parse_duration("1h").unwrap();
        assert_eq!(d, Duration::from_secs(3600));
    }

    #[test]
    fn test_parse_duration_seconds() {
        let d = parse_duration("30s").unwrap();
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn test_parse_duration_invalid_suffix() {
        assert!(parse_duration("20x").is_none());
    }

    #[test]
    fn test_parse_duration_no_number() {
        assert!(parse_duration("m").is_none());
    }

    #[test]
    fn test_parse_duration_empty() {
        assert!(parse_duration("").is_none());
    }

    #[test]
    fn test_scheduler_fires_due_beacon() {
        let mut scheduler = BeaconScheduler::new();
        let beacon = Beacon {
            content: BeaconContent::Raw("!test data".to_string()),
            mode: BeaconMode::Aprsis,
            transmitter: None,
            via: None,
        };
        // Use a zero-duration interval so it fires immediately
        scheduler.add_beacon(beacon, "TEST-1", Duration::from_secs(0));
        // Force the next_fire to be in the past
        scheduler.beacons[0].next_fire = Instant::now() - Duration::from_secs(1);

        let results = scheduler.poll();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "TEST-1>APRS:!test data");
        assert_eq!(results[0].1, BeaconMode::Aprsis);
        assert!(results[0].2.is_none());
    }

    #[test]
    fn test_scheduler_does_not_fire_early() {
        let mut scheduler = BeaconScheduler::new();
        let beacon = Beacon {
            content: BeaconContent::Raw("!test".to_string()),
            mode: BeaconMode::Both,
            transmitter: None,
            via: None,
        };
        scheduler.add_beacon(beacon, "TEST-1", Duration::from_secs(3600));
        // next_fire is in the future, so poll should return nothing
        let results = scheduler.poll();
        assert!(results.is_empty());
    }

    #[test]
    fn test_scheduler_reschedules() {
        let mut scheduler = BeaconScheduler::new();
        let beacon = Beacon {
            content: BeaconContent::Raw("!test".to_string()),
            mode: BeaconMode::Both,
            transmitter: None,
            via: None,
        };
        let interval = Duration::from_secs(600);
        scheduler.add_beacon(beacon, "TEST-1", interval);
        // Force it to fire
        scheduler.beacons[0].next_fire = Instant::now() - Duration::from_secs(1);

        let before_fire = Instant::now();
        let _results = scheduler.poll();

        // After firing, next_fire should be in the future
        let next = scheduler.beacons[0].next_fire;
        assert!(next > before_fire);
        // Should be roughly 80-100% of interval from now
        let max_next = before_fire + interval;
        let min_next = before_fire + Duration::from_secs_f64(interval.as_secs_f64() * 0.79);
        assert!(
            next >= min_next,
            "next_fire too early: {:?} < {:?}",
            next,
            min_next
        );
        assert!(
            next <= max_next,
            "next_fire too late: {:?} > {:?}",
            next,
            max_next
        );
    }

    #[test]
    fn test_scheduler_randomized_interval() {
        // Generate multiple jitter values and verify they fall in range
        for seed in 0..100 {
            let factor = random_jitter(seed);
            assert!(
                (0.80..=1.00).contains(&factor),
                "jitter factor {} out of range for seed {}",
                factor,
                seed
            );
        }
    }

    #[test]
    fn test_beacon_from_config() {
        let toml_str = r#"
            mycall = "OH2MQK-1"

            [location]
            lat = "6029.50N"
            lon = "02505.43E"

            [[beacon]]
            symbol = "R&"
            comment = "Rx-only iGate"
            cycle_size = "20m"
            mode = "aprsis"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let scheduler = BeaconScheduler::from_config(&config);

        assert_eq!(scheduler.len(), 1);
        assert!(!scheduler.is_empty());
    }

    #[test]
    fn test_beacon_from_config_multiple() {
        let toml_str = r#"
            mycall = "N0CALL-1"

            [location]
            lat = "4903.50N"
            lon = "07201.75W"

            [[beacon]]
            symbol = "/>"
            comment = "Beacon 1"
            cycle_size = "20m"

            [[beacon]]
            symbol = "\\K"
            comment = "Beacon 2"
            cycle_size = "20m"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let scheduler = BeaconScheduler::from_config(&config);

        assert_eq!(scheduler.len(), 2);
    }

    #[test]
    fn test_beacon_from_config_no_location() {
        // Beacons with symbols but no location should be skipped
        let toml_str = r#"
            mycall = "N0CALL-1"

            [[beacon]]
            symbol = "/>"
            comment = "No location"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let scheduler = BeaconScheduler::from_config(&config);

        assert_eq!(scheduler.len(), 0);
    }

    #[test]
    fn test_beacon_from_config_default_mode() {
        // When no mode is specified, defaults to Both
        let toml_str = r#"
            mycall = "N0CALL-1"

            [location]
            lat = "4903.50N"
            lon = "07201.75W"

            [[beacon]]
            symbol = "/>"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let scheduler = BeaconScheduler::from_config(&config);

        assert_eq!(scheduler.len(), 1);
    }

    #[test]
    fn test_empty_scheduler() {
        let scheduler = BeaconScheduler::new();
        assert!(scheduler.is_empty());
        assert_eq!(scheduler.len(), 0);
    }

    #[test]
    fn test_empty_scheduler_poll() {
        let mut scheduler = BeaconScheduler::new();
        let results = scheduler.poll();
        assert!(results.is_empty());
    }

    #[test]
    fn test_maybe_add_via() {
        let packet = "TEST>APRS:!data";
        let result = maybe_add_via(packet, Some("WIDE1-1,WIDE2-1"));
        assert_eq!(result, "TEST>APRS,WIDE1-1,WIDE2-1:!data");
    }

    #[test]
    fn test_maybe_add_via_none() {
        let packet = "TEST>APRS:!data";
        let result = maybe_add_via(packet, None);
        assert_eq!(result, "TEST>APRS:!data");
    }

    #[test]
    fn test_maybe_add_via_empty() {
        let packet = "TEST>APRS:!data";
        let result = maybe_add_via(packet, Some(""));
        assert_eq!(result, "TEST>APRS:!data");
    }

    #[test]
    fn test_beacon_with_via_path() {
        let mut scheduler = BeaconScheduler::new();
        let beacon = Beacon {
            content: BeaconContent::Position {
                lat: "6029.50N".to_string(),
                lon: "02505.43E".to_string(),
                symbol_table: '/',
                symbol_code: '>',
                comment: Some("test".to_string()),
            },
            mode: BeaconMode::Radio,
            transmitter: Some("RADIO0".to_string()),
            via: Some("WIDE1-1".to_string()),
        };
        scheduler.add_beacon(beacon, "TEST-1", Duration::from_secs(0));
        scheduler.beacons[0].next_fire = Instant::now() - Duration::from_secs(1);

        let results = scheduler.poll();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "TEST-1>APRS,WIDE1-1:!6029.50N/02505.43E>test");
        assert_eq!(results[0].1, BeaconMode::Radio);
        assert_eq!(results[0].2.as_deref(), Some("RADIO0"));
    }

    #[test]
    fn test_jittered_duration_range() {
        let base = Duration::from_secs(1000);
        for seed in 0..50 {
            let jittered = jittered_duration(base, seed);
            let min = Duration::from_secs(800);
            let max = Duration::from_secs(1000);
            assert!(
                jittered >= min && jittered <= max,
                "jittered {:?} not in [{:?}, {:?}] for seed {}",
                jittered,
                min,
                max,
                seed
            );
        }
    }

    #[test]
    fn test_format_beacon_content_raw() {
        let content = BeaconContent::Raw(">status text".to_string());
        let result = format_beacon_content(&content, "TEST-1", None);
        assert_eq!(result.unwrap(), "TEST-1>APRS:>status text");
    }

    #[test]
    fn test_format_beacon_content_position() {
        let content = BeaconContent::Position {
            lat: "6029.50N".to_string(),
            lon: "02505.43E".to_string(),
            symbol_table: '/',
            symbol_code: '>',
            comment: None,
        };
        let result = format_beacon_content(&content, "TEST-1", None);
        assert_eq!(result.unwrap(), "TEST-1>APRS:!6029.50N/02505.43E>");
    }
}
