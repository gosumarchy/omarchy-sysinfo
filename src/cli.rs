//! The command line: what was asked for.

use std::ffi::OsString;
use std::fmt;

use crate::report::Identifiers;

pub(crate) const HELP: &str = "\
omarchy-sysinfo — describe this machine

usage: omarchy-sysinfo [--plain [--identifiers]]

(no args)       interactive TUI
--plain         print every section and exit
--identifiers   with --plain, include serials, UUIDs, MAC addresses,
                network names and the focused window title
--version       print the version
--help          print this help

Run from a pipe, the TUI prints the plain report instead.";

/// What the command line asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Tui,
    Plain(Identifiers),
    Help,
    Version,
}

/// A command line that does not mean anything; the message says why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageError(String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\ntry --help", self.0)
    }
}

impl std::error::Error for UsageError {}

/// Parse the arguments after the program name.
///
/// Every argument must mean something. `--plain --nonsense` used to print
/// the report and ignore the typo, which is how a misspelt flag goes
/// unnoticed in a keybinding for months.
pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| {
            // `env::args` panics on an argument that is not UTF-8; none of
            // ours are, so it is a usage error like any other.
            arg.into_string().map_err(|raw| {
                UsageError(format!(
                    "argument is not valid UTF-8: {}",
                    raw.to_string_lossy()
                ))
            })
        })
        .collect::<Result<_, _>>()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    match args.as_slice() {
        [] => Ok(Command::Tui),
        ["--help" | "-h"] => Ok(Command::Help),
        ["--version" | "-V"] => Ok(Command::Version),
        ["--plain"] => Ok(Command::Plain(Identifiers::Hide)),
        ["--plain", "--identifiers"] | ["--identifiers", "--plain"] => {
            Ok(Command::Plain(Identifiers::Show))
        }
        ["--identifiers"] => Err(UsageError("--identifiers only applies to --plain".into())),
        [first, ..] if !is_known(first) => Err(UsageError(format!("unknown argument: {first}"))),
        [_, extra, ..] => Err(UsageError(format!("unexpected argument: {extra}"))),
        // A known flag alone that is not one of the forms above.
        [only] => Err(UsageError(format!("unexpected argument: {only}"))),
    }
}

fn is_known(arg: &str) -> bool {
    matches!(
        arg,
        "--help" | "-h" | "--version" | "-V" | "--plain" | "--identifiers"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(list: &[&str]) -> Result<Command, UsageError> {
        parse(list.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_starts_the_tui() {
        assert_eq!(parse_strs(&[]), Ok(Command::Tui));
    }

    #[test]
    fn the_documented_flags_are_recognised() {
        assert_eq!(
            parse_strs(&["--plain"]),
            Ok(Command::Plain(Identifiers::Hide))
        );
        assert_eq!(parse_strs(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["-h"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["--version"]), Ok(Command::Version));
        assert_eq!(parse_strs(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn identifiers_are_opt_in_and_only_for_the_plain_report() {
        let show = Ok(Command::Plain(Identifiers::Show));
        assert_eq!(parse_strs(&["--plain", "--identifiers"]), show);
        assert_eq!(parse_strs(&["--identifiers", "--plain"]), show);
        assert!(parse_strs(&["--identifiers"]).is_err());
    }

    #[test]
    fn an_unknown_flag_is_reported_rather_than_ignored() {
        let err = parse_strs(&["--nope"]).expect_err("unknown");
        assert!(err.to_string().contains("--nope"), "{err}");
        // A bare word is not a shortcut for the TUI either.
        assert!(parse_strs(&["plain"]).is_err());
        assert!(parse_strs(&["-x"]).is_err());
    }

    #[test]
    fn a_trailing_typo_is_an_error_not_silently_dropped() {
        let err = parse_strs(&["--plain", "--nonsense"]).expect_err("typo");
        assert!(err.to_string().contains("--nonsense"), "{err}");
        assert!(parse_strs(&["--help", "--version"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_argument_is_a_usage_error_not_a_panic() {
        use std::os::unix::ffi::OsStringExt;

        let raw = OsString::from_vec(vec![b'-', b'-', 0xff]);
        let err = parse([raw]).expect_err("not UTF-8");
        assert!(err.to_string().contains("UTF-8"), "{err}");
    }

    #[test]
    fn help_text_documents_every_flag() {
        for flag in ["--plain", "--identifiers", "--version", "--help"] {
            assert!(HELP.contains(flag), "help omits {flag}");
        }
        assert!(HELP.contains("usage:"), "help needs a usage line");
        assert!(
            HELP.contains("interactive TUI"),
            "help must say how to start the TUI"
        );
    }
}
