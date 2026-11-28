// What stops a step of the converter: an error in what it reads or makes, one that says
// the map can't be converted, or a conversion stopped by its caller.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// What went wrong reading or writing something, as the player is told.
    Plain(String),
    /// The map can't be converted (what it needs is missing, or too large).
    Port(String),
    /// The conversion was stopped by its log function (the window's CANCEL). No handler
    /// of a failing step takes it for an error: `is_error` is false.
    Cancelled,
}

impl Error {
    pub fn new(text: impl Into<String>) -> Error {
        Error::Plain(text.into())
    }

    pub fn port(text: impl Into<String>) -> Error {
        Error::Port(text.into())
    }

    /// An error a step may catch and carry on from: anything but a stop.
    pub fn is_error(&self) -> bool {
        !matches!(self, Error::Cancelled)
    }

    pub fn is_port(&self) -> bool {
        matches!(self, Error::Port(_))
    }

    pub fn text(&self) -> &str {
        match self {
            Error::Plain(text) | Error::Port(text) => text,
            Error::Cancelled => "the conversion was stopped",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for Error {}

impl From<mw2_zone::Error> for Error {
    fn from(e: mw2_zone::Error) -> Error {
        Error::Plain(e.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Returns an Error::Plain of the text formatted.
#[macro_export]
macro_rules! fail {
    ($($arg:tt)*) => {
        return Err($crate::error::Error::Plain(format!($($arg)*)))
    };
}
