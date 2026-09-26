//! The program's error type.

use std::error::Error as StdError;
use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong, as a closed set rather than a
/// `Box<dyn Error>`.
///
/// Only the terminal can fail, and it can only fail with an I/O error, so a
/// single variant says all of that: no dynamic dispatch, and the caller can
/// match on the cause when it decides how to report.
pub enum Error {
    Io(io::Error),
}

/// A failed `main` is reported with `Debug`, so delegate to `Display` and save
/// the reader an `Io(Os { code: 5, kind: ... })` dump.
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Io(e) => Some(e),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}
