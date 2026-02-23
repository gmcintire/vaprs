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
