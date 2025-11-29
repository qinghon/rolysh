use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidHostName(String),
    InvalidArgs(String),
    BufferTooLarge,
    ConnectionError(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "IO error: {e}"),
            Error::InvalidHostName(h) => write!(f, "Invalid hostname: {h}"),
            Error::InvalidArgs(msg) => write!(f, "Invalid arguments: {msg}"),
            Error::BufferTooLarge => write!(f, "Buffer too large"),
            Error::ConnectionError(msg) => write!(f, "Connection error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
