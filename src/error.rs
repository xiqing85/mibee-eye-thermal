//! Error type shared across the crate.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// Underlying bus transport failed (errno or protocol NAK).
    Bus(String),
    /// Sensor returned data that violates the MLX90640 register-map contract.
    /// (Lands with the EE-calibration / frame-extraction milestone.)
    #[allow(dead_code)]
    Sensor(String),
    /// Bad usage / configuration.
    Usage(String),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Bus(m) => write!(f, "i2c bus: {m}"),
            Error::Sensor(m) => write!(f, "mlx90640: {m}"),
            Error::Usage(m) => write!(f, "usage: {m}"),
            Error::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
