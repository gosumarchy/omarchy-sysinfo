//! The program's error type.

use std::error::Error as StdError;
use std::fmt;
use std::io;

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Everything that can stop the program, as a closed set rather than a
/// `Box<dyn Error>`.
///
/// Collection never fails (a missing file is a row that says so), so what is
/// left is the terminal: it can refuse raw mode, or not be a terminal at all.
#[derive(Debug)]
pub(crate) enum Error {
    Io(io::Error),
    /// The TUI needs a keyboard, and stdin is a pipe or a file.
    NotATerminal,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::NotATerminal => {
                f.write_str("stdin is not a terminal; use --plain for a text report")
            }
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::NotATerminal => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_as_sentences_not_debug_dumps() {
        let io = Error::from(io::Error::other("disk on fire"));

        assert_eq!(io.to_string(), "disk on fire");
        assert!(io.source().is_some());
        assert!(Error::NotATerminal.to_string().contains("--plain"));
        assert!(Error::NotATerminal.source().is_none());
    }
}
