//! The command line: what was asked for, and the two modes that answer it.

use crate::error::Result;
use crate::report;

pub const HELP: &str = "\
omarchy-sysinfo — describe this machine

usage: omarchy-sysinfo [--plain]

(no args)   interactive TUI
--plain     print every section and exit
--version   print the version";

/// Exit code for a usage mistake, matching the usual CLI convention.
const USAGE_EXIT: i32 = 2;

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Tui,
    Plain,
    Help,
    Version,
    Unknown(String),
}

/// Handle the arguments, or report that there are none and the TUI should run.
///
/// `None` means "no arguments, carry on", which is the only case the caller has
/// to think about; everything else is already finished by the time this
/// returns.
pub fn handle_args() -> Option<Result<()>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Action::Tui => None,
        Action::Help => {
            println!("{HELP}");
            Some(Ok(()))
        }
        Action::Version => {
            println!("omarchy-sysinfo {}", env!("CARGO_PKG_VERSION"));
            Some(Ok(()))
        }
        Action::Plain => Some(report::print()),
        Action::Unknown(arg) => {
            eprintln!("unknown argument: {arg}\ntry --help");
            // A bad flag must not look like success: scripts and keybindings
            // check the exit status, and this used to exit 0.
            std::process::exit(USAGE_EXIT);
        }
    }
}

fn parse_args(args: &[String]) -> Action {
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => Action::Help,
        Some("--version") => Action::Version,
        Some("--plain") => Action::Plain,
        Some(other) => Action::Unknown(other.to_string()),
        None => Action::Tui,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // ---- argument parsing -----------------------------------------------

    #[test]
    fn no_arguments_starts_the_tui() {
        assert_eq!(parse_args(&[]), Action::Tui);
    }

    #[test]
    fn the_documented_flags_are_recognised() {
        assert_eq!(parse_args(&args(&["--plain"])), Action::Plain);
        assert_eq!(parse_args(&args(&["--help"])), Action::Help);
        assert_eq!(parse_args(&args(&["-h"])), Action::Help);
        assert_eq!(parse_args(&args(&["--version"])), Action::Version);
    }

    #[test]
    fn an_unknown_flag_is_reported_rather_than_ignored() {
        assert_eq!(
            parse_args(&args(&["--nope"])),
            Action::Unknown("--nope".into())
        );
        // A bare word is not a shortcut for the TUI either.
        assert_eq!(
            parse_args(&args(&["plain"])),
            Action::Unknown("plain".into())
        );
        assert_eq!(parse_args(&args(&["-x"])), Action::Unknown("-x".into()));
    }

    #[test]
    fn only_the_first_argument_decides() {
        // `--plain --nonsense` still prints the report; the extra flag is
        // ignored rather than being treated as an error.
        assert_eq!(parse_args(&args(&["--plain", "--nonsense"])), Action::Plain);
    }

    #[test]
    fn help_text_documents_every_flag_it_has_a_description_for() {
        for flag in ["--plain", "--version"] {
            assert!(HELP.contains(flag), "help omits {flag}");
        }
        assert!(HELP.contains("usage:"), "help needs a usage line");
        assert!(
            HELP.contains("interactive TUI"),
            "help must say how to start the TUI"
        );
    }
}
