// Configuration parsing - TOML based

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceType {
    Serial,
    Tcp,
    Ax25,
    Agwpe,
    Null,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Protocol {
    Kiss,
    Smack,
    Flexnet,
    #[serde(rename = "BPQCRC")]
    Bpqcrc,
    #[serde(rename = "TNC2")]
    Tnc2,
    #[serde(rename = "DPRS")]
    Dprs,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawLocationConfig")]
pub struct LocationConfig {
    /// Latitude in APRS format: DDMM.MMN or DDMM.MMS
    pub lat: String,
    /// Longitude in APRS format: DDDMM.MME or DDDMM.MMW
    pub lon: String,
}

/// Raw config before conversion — accepts either decimal degrees or APRS format.
#[derive(Deserialize)]
struct RawLocationConfig {
    lat: toml::Value,
    lon: toml::Value,
}

/// Convert decimal degrees latitude to APRS DDMM.MMN/S format.
fn decimal_lat_to_aprs(deg: f64) -> Result<String, String> {
    if !(-90.0..=90.0).contains(&deg) {
        return Err(format!("latitude {deg} out of range -90..90"));
    }
    let hemi = if deg >= 0.0 { 'N' } else { 'S' };
    let deg = deg.abs();
    let d = deg as u32;
    let m = (deg - d as f64) * 60.0;
    Ok(format!("{:02}{:05.2}{}", d, m, hemi))
}

/// Convert decimal degrees longitude to APRS DDDMM.MME/W format.
fn decimal_lon_to_aprs(deg: f64) -> Result<String, String> {
    if !(-180.0..=180.0).contains(&deg) {
        return Err(format!("longitude {deg} out of range -180..180"));
    }
    let hemi = if deg >= 0.0 { 'E' } else { 'W' };
    let deg = deg.abs();
    let d = deg as u32;
    let m = (deg - d as f64) * 60.0;
    Ok(format!("{:03}{:05.2}{}", d, m, hemi))
}

/// Returns true if the string looks like APRS DDMM.MM[NS] or DDDMM.MM[EW] format.
fn is_aprs_format(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 6 {
        return false;
    }
    let last = s.as_bytes()[s.len() - 1];
    matches!(last, b'N' | b'S' | b'E' | b'W')
        && s[..s.len() - 1]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
}

impl TryFrom<RawLocationConfig> for LocationConfig {
    type Error = String;

    fn try_from(raw: RawLocationConfig) -> Result<Self, String> {
        let lat = match raw.lat {
            toml::Value::Float(f) => decimal_lat_to_aprs(f)?,
            toml::Value::Integer(i) => decimal_lat_to_aprs(i as f64)?,
            toml::Value::String(s) => {
                if is_aprs_format(&s) {
                    s
                } else {
                    let f: f64 = s.parse().map_err(|_| format!("invalid latitude: {s}"))?;
                    decimal_lat_to_aprs(f)?
                }
            }
            other => return Err(format!("invalid latitude type: {other}")),
        };
        let lon = match raw.lon {
            toml::Value::Float(f) => decimal_lon_to_aprs(f)?,
            toml::Value::Integer(i) => decimal_lon_to_aprs(i as f64)?,
            toml::Value::String(s) => {
                if is_aprs_format(&s) {
                    s
                } else {
                    let f: f64 = s.parse().map_err(|_| format!("invalid longitude: {s}"))?;
                    decimal_lon_to_aprs(f)?
                }
            }
            other => return Err(format!("invalid longitude type: {other}")),
        };
        Ok(LocationConfig { lat, lon })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AprsIsConfig {
    pub passcode: i32,
    #[serde(default)]
    pub servers: Vec<String>,
    pub filter: Option<String>,
    pub heartbeat_timeout: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    pub rflog: Option<String>,
    pub aprxlog: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InterfaceConfig {
    #[serde(rename = "type")]
    pub iface_type: InterfaceType,
    pub device: Option<String>,
    pub speed: Option<u32>,
    pub protocol: Option<Protocol>,
    pub callsign: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,

    /// Whether transmitting is allowed on this interface
    #[serde(default)]
    pub tx_ok: bool,

    /// Whether telemetry is forwarded to APRS-IS
    #[serde(default = "default_true")]
    pub telem_to_is: bool,

    /// iGate group number for this interface
    #[serde(default = "default_igate_group")]
    pub igate_group: u8,
}

fn default_true() -> bool {
    true
}

fn default_igate_group() -> u8 {
    1
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BeaconMode {
    Radio,
    Aprsis,
    Both,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconConfig {
    pub symbol: Option<String>,
    pub comment: Option<String>,
    #[serde(rename = "cycle_size")]
    pub cycle_size: Option<String>,
    pub mode: Option<BeaconMode>,
    pub transmitter: Option<String>,
    pub via: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RelayType {
    Digipeated,
    #[serde(rename = "third-party")]
    ThirdParty,
    Direct,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DigipeaterSourceConfig {
    pub source: String,
    pub relay_type: Option<RelayType>,
    pub viscous_delay: Option<u32>,
    pub ratelimit: Option<Vec<u32>>,
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DigipeaterConfig {
    pub transmitter: String,
    #[serde(default)]
    pub ratelimit: Vec<u32>,
    #[serde(rename = "source", default)]
    pub sources: Vec<DigipeaterSourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryConfig {
    pub source: String,
    pub destination: Option<String>,
}

fn default_web_listen() -> String {
    "127.0.0.1".to_string()
}

fn default_web_port() -> u16 {
    14501
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebConfig {
    #[serde(default = "default_web_listen")]
    pub listen: String,
    #[serde(default = "default_web_port")]
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub mycall: String,
    pub location: Option<LocationConfig>,
    pub aprsis: Option<AprsIsConfig>,
    pub logging: Option<LoggingConfig>,
    pub web: Option<WebConfig>,

    #[serde(rename = "interface", default)]
    pub interfaces: Vec<InterfaceConfig>,

    #[serde(rename = "beacon", default)]
    pub beacons: Vec<BeaconConfig>,

    #[serde(rename = "digipeater", default)]
    pub digipeaters: Vec<DigipeaterConfig>,

    #[serde(rename = "telemetry", default)]
    pub telemetry: Vec<TelemetryConfig>,
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let toml_str = r#"
            mycall = "N0CALL-1"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mycall, "N0CALL-1");
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
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
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mycall, "OH2MQK-1");
        assert_eq!(config.aprsis.as_ref().unwrap().passcode, 12345);
        assert_eq!(config.interfaces.len(), 1);
        assert_eq!(config.interfaces[0].device.as_deref(), Some("/dev/ttyUSB0"));
    }

    #[test]
    fn test_parse_interface_types() {
        let toml_str = r#"
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
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.interfaces.len(), 2);
        assert_eq!(config.interfaces[0].iface_type, InterfaceType::Serial);
        assert_eq!(config.interfaces[1].iface_type, InterfaceType::Tcp);
    }

    #[test]
    fn test_default_values() {
        let toml_str = r#"
            mycall = "N0CALL-1"

            [[interface]]
            type = "serial"
            device = "/dev/ttyUSB0"
            speed = 9600
            protocol = "KISS"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let iface = &config.interfaces[0];
        assert!(!iface.tx_ok);
        assert!(iface.telem_to_is);
        assert_eq!(iface.igate_group, 1);
    }

    #[test]
    fn test_parse_digipeater_config() {
        let toml_str = r#"
            mycall = "N0CALL-1"

            [[digipeater]]
            transmitter = "N0CALL-1"
            ratelimit = [60, 120]

            [[digipeater.source]]
            source = "N0CALL-1"
            relay_type = "digipeated"
            viscous_delay = 0
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.digipeaters.len(), 1);
        assert_eq!(config.digipeaters[0].transmitter, "N0CALL-1");
        assert_eq!(config.digipeaters[0].sources.len(), 1);
    }

    #[test]
    fn test_parse_beacon_config() {
        let toml_str = r#"
            mycall = "N0CALL-1"

            [[beacon]]
            symbol = "R&"
            comment = "Rx-only iGate"
            cycle_size = "20m"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.beacons.len(), 1);
        assert_eq!(config.beacons[0].symbol.as_deref(), Some("R&"));
    }

    #[test]
    fn test_location_aprs_format_passthrough() {
        let toml_str = r#"
            mycall = "N0CALL-1"
            [location]
            lat = "4903.50N"
            lon = "07201.75W"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let loc = config.location.unwrap();
        assert_eq!(loc.lat, "4903.50N");
        assert_eq!(loc.lon, "07201.75W");
    }

    #[test]
    fn test_location_decimal_degrees_float() {
        let toml_str = r#"
            mycall = "N0CALL-1"
            [location]
            lat = 33.45
            lon = -96.78
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let loc = config.location.unwrap();
        assert_eq!(loc.lat, "3327.00N");
        assert_eq!(loc.lon, "09646.80W");
    }

    #[test]
    fn test_location_decimal_degrees_integer() {
        let toml_str = r#"
            mycall = "N0CALL-1"
            [location]
            lat = 33
            lon = -97
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let loc = config.location.unwrap();
        assert_eq!(loc.lat, "3300.00N");
        assert_eq!(loc.lon, "09700.00W");
    }

    #[test]
    fn test_location_decimal_degrees_string() {
        let toml_str = r#"
            mycall = "N0CALL-1"
            [location]
            lat = "33.45"
            lon = "-96.78"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let loc = config.location.unwrap();
        assert_eq!(loc.lat, "3327.00N");
        assert_eq!(loc.lon, "09646.80W");
    }

    #[test]
    fn test_location_southern_hemisphere() {
        let toml_str = r#"
            mycall = "N0CALL-1"
            [location]
            lat = -33.86
            lon = 151.21
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let loc = config.location.unwrap();
        assert!(loc.lat.ends_with('S'));
        assert!(loc.lon.ends_with('E'));
    }

    #[test]
    fn test_location_out_of_range() {
        let toml_str = r#"
            mycall = "N0CALL-1"
            [location]
            lat = 91.0
            lon = 0.0
        "#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_decimal_lat_to_aprs() {
        assert_eq!(decimal_lat_to_aprs(0.0).unwrap(), "0000.00N");
        assert_eq!(decimal_lat_to_aprs(49.0583333).unwrap(), "4903.50N");
        assert_eq!(decimal_lat_to_aprs(-33.86).unwrap(), "3351.60S");
    }

    #[test]
    fn test_decimal_lon_to_aprs() {
        assert_eq!(decimal_lon_to_aprs(0.0).unwrap(), "00000.00E");
        assert_eq!(decimal_lon_to_aprs(-72.029167).unwrap(), "07201.75W");
        assert_eq!(decimal_lon_to_aprs(151.21).unwrap(), "15112.60E");
    }

    #[test]
    fn test_web_config_parsing() {
        let toml_str = r#"
            mycall = "N0CALL-1"
            [web]
            listen = "0.0.0.0"
            port = 8080
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let web = config.web.unwrap();
        assert_eq!(web.listen, "0.0.0.0");
        assert_eq!(web.port, 8080);
    }

    #[test]
    fn test_web_config_defaults() {
        let toml_str = r#"
            mycall = "N0CALL-1"
            [web]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let web = config.web.unwrap();
        assert_eq!(web.listen, "127.0.0.1");
        assert_eq!(web.port, 14501);
    }

    #[test]
    fn test_web_config_absent() {
        let toml_str = r#"
            mycall = "N0CALL-1"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.web.is_none());
    }
}
