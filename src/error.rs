use std::fmt;

#[derive(Debug)]
pub enum VaprsError {
    Config(String),
    Io(std::io::Error),
    AprsIs(String),
    Ax25(String),
    Kiss(String),
    Callsign(String),
    Interface { interface: String, message: String },
}

impl fmt::Display for VaprsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "configuration error: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::AprsIs(msg) => write!(f, "APRS-IS connection error: {msg}"),
            Self::Ax25(msg) => write!(f, "AX.25 frame error: {msg}"),
            Self::Kiss(msg) => write!(f, "KISS framing error: {msg}"),
            Self::Callsign(msg) => write!(f, "invalid callsign: {msg}"),
            Self::Interface { interface, message } => {
                write!(f, "interface error on {interface}: {message}")
            }
        }
    }
}

impl std::error::Error for VaprsError {}

impl From<std::io::Error> for VaprsError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

pub type Result<T> = std::result::Result<T, VaprsError>;
