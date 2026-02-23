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
pub struct LocationConfig {
    pub lat: String,
    pub lon: String,
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

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub mycall: String,
    pub location: Option<LocationConfig>,
    pub aprsis: Option<AprsIsConfig>,
    pub logging: Option<LoggingConfig>,

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
}
