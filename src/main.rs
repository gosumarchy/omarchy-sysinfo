//! omarchy-sysinfo — describe this machine, section by section.
//!
//! No dependencies: the terminal, the keyboard, and every `/proc` and `/sys`
//! parser here are written against the standard library only.

mod app;
mod collect;
mod input;
mod term;
mod ui;

use std::error::Error as StdError;
use std::fmt;
use std::io::{self, Write};
use std::time::Duration;

type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong, as a closed set rather than a
/// `Box<dyn Error>`.
///
/// Only the terminal can fail, and it can only fail with an I/O error, so a
/// single variant says all of that: no dynamic dispatch, and `main` can match
/// on the cause when it decides how to report.
enum Error {
    Io(io::Error),
}

/// `main` prints a failed `Result` with `Debug`, so delegate to `Display` and
/// save the reader an `Io(Os { code: 5, kind: ... })` dump.
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

const HELP: &str = "\
omarchy-sysinfo — describe this machine

usage: omarchy-sysinfo [--plain]

(no args)   interactive TUI
--plain     print every section and exit
--version   print the version";

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Tui,
    Plain,
    Help,
    Version,
    Unknown(String),
}

fn main() -> Result<()> {
    if let Some(result) = handle_args() {
        return result;
    }
    run_tui()
}

/// Exit code for a usage mistake, matching the usual CLI convention.
const USAGE_EXIT: i32 = 2;

fn handle_args() -> Option<Result<()>> {
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
        Action::Plain => Some(print_plain()),
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

/// Print the whole report, for piping into a file or a bug report.
fn print_plain() -> Result<()> {
    let mut app = app::App::new();
    // CPU usage is a delta between two reads, so give it a moment to happen.
    if !app.stats.primed() {
        std::thread::sleep(Duration::from_millis(150));
        app.collect();
    }

    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    // `println!` panics when the reader goes away, which is exactly what
    // `omarchy-sysinfo --plain | head` does. Write through a buffer and stop
    // quietly instead.
    for line in format_report(&app.sections) {
        if writeln!(out, "{line}").is_err() {
            return Ok(());
        }
    }
    let _ = out.flush();
    Ok(())
}

/// The whole report, one string per line.
///
/// This output is meant to be piped into grep, a file or a bug report, so it
/// carries no escape sequences and nothing else writes a heading: the heading
/// and rule used to be emitted both here and in `format_section`, which
/// printed every section title twice.
fn format_report(sections: &[collect::Section]) -> Vec<String> {
    sections.iter().flat_map(format_section).collect()
}

/// The plain-text rendering of one section, one string per line.
fn format_section(section: &collect::Section) -> Vec<String> {
    let mut lines = vec![section.title.to_uppercase(), "\u{2500}".repeat(60)];
    for row in &section.rows {
        lines.push(match row {
            collect::Row::Header(text) => format!("\n  \u{258c} {text}"),
            collect::Row::Field { label, value, bar } => match bar {
                // The value already carrying a percentage is left alone, so it
                // is never printed twice.
                Some(b) if !value.contains('%') => {
                    format!("  {label:<20}  {value}  {:.0}%", b.frac() * 100.0)
                }
                _ => format!("  {label:<20}  {value}"),
            },
            collect::Row::Note(text) => format!("  \u{2013} {text}"),
            collect::Row::Blank => String::new(),
        });
    }
    lines
}

fn run_tui() -> Result<()> {
    let mut terminal = term::Terminal::enter()?;
    let keys = input::spawn();
    let mut app = app::App::new();
    let palette = ui::theme::Palette::from_omarchy();

    let mut redraw = true;
    loop {
        if redraw {
            terminal.refresh_size();
            let (width, height) = terminal.size();
            let mut buffer = term::Buffer::new(
                width,
                height,
                term::Style::new(term::Color::Default).on(palette.background),
            );
            ui::draw(&mut buffer, &app, &palette);
            terminal.draw(&buffer)?;
            redraw = false;
        }

        if let Some(key) = input::next_key(&keys, Duration::from_millis(250)) {
            redraw = true;
            app.on_key(key);
        }

        if app.tick() {
            redraw = true;
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collect::{Bar, Row, Section};

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

    // ---- plain formatting ------------------------------------------------

    fn section(rows: Vec<Row>) -> Section {
        Section::new("Test Section", rows)
    }

    #[test]
    fn a_section_starts_with_its_uppercased_title_and_a_rule() {
        let lines = format_section(&section(vec![]));
        assert!(lines[0].contains("TEST SECTION"), "{:?}", lines[0]);
        assert!(lines[1].chars().all(|c| c == '\u{2500}'));
        assert_eq!(lines.len(), 2, "an empty section is just a heading");
    }

    #[test]
    fn a_report_prints_every_heading_exactly_once() {
        // The bug: the heading and rule were emitted by the caller as well as
        // by format_section, so every section appeared twice.
        let sections = vec![
            Section::new("Alpha", vec![Row::field("A", "1")]),
            Section::new("Beta", vec![Row::field("B", "2")]),
        ];
        let report = format_report(&sections);
        for title in ["ALPHA", "BETA"] {
            let count = report.iter().filter(|l| l.contains(title)).count();
            assert_eq!(count, 1, "{title} appeared {count} times: {report:?}");
        }
        assert_eq!(
            report
                .iter()
                .filter(|l| l.chars().all(|c| c == '\u{2500}'))
                .count(),
            2,
            "one rule per section: {report:?}"
        );
    }

    #[test]
    fn a_report_carries_no_escape_sequences() {
        // The bug: bold escapes made grep treat the output as a binary file.
        let sections = vec![
            Section::new("Alpha", vec![Row::field("A", "1")]),
            Section::new(
                "Beta",
                vec![Row::Header("Inner".into()), Row::field("B", "2")],
            ),
        ];
        for line in format_report(&sections) {
            assert!(!line.contains('\u{1b}'), "escape sequence in {line:?}");
            assert!(line.is_char_boundary(0) && !line.contains('\r'), "{line:?}");
        }
    }

    #[test]
    fn a_report_of_no_sections_is_empty() {
        assert!(format_report(&[]).is_empty());
    }

    #[test]
    fn labels_are_padded_into_a_column() {
        let lines = format_section(&section(vec![Row::field("CPU", "12%")]));
        // Two leading spaces, the label padded to 20 columns, then two more.
        assert_eq!(lines[2], "  CPU                   12%");
    }

    #[test]
    fn a_long_label_overflows_its_column_rather_than_truncating() {
        let long = "a-really-quite-long-label-indeed";
        let lines = format_section(&section(vec![Row::field(long, "v")]));
        assert!(lines[2].starts_with(&format!("  {long}")));
        assert!(lines[2].contains("v"));
    }

    #[test]
    fn a_bar_adds_its_percentage_unless_the_value_already_has_one() {
        let lines = format_section(&section(vec![Row::Field {
            label: "Memory".into(),
            value: "1.0 GiB / 2.0 GiB".into(),
            bar: Some(Bar::new(0.5)),
        }]));
        assert!(
            lines[2].ends_with("50%"),
            "expected a gauge value: {:?}",
            lines[2]
        );
        assert_eq!(lines[2].matches('%').count(), 1, "must not print it twice");

        let lines = format_section(&section(vec![Row::Field {
            label: "Memory".into(),
            value: "50%".into(),
            bar: Some(Bar::new(0.5)),
        }]));
        assert_eq!(lines[2].matches('%').count(), 1, "must not print it twice");
        assert!(lines[2].ends_with("50%"));
    }

    #[test]
    fn gauge_percentages_round_to_whole_numbers() {
        let pct = |frac: f64| {
            let lines = format_section(&section(vec![Row::Field {
                label: "x".into(),
                value: "v".into(),
                bar: Some(Bar::new(frac)),
            }]));
            lines[2].rsplit(' ').next().unwrap().to_string()
        };
        assert_eq!(pct(0.0), "0%");
        assert_eq!(pct(0.999), "100%");
        assert_eq!(pct(0.5), "50%");
        assert_eq!(pct(0.004), "0%");
        // Rust's `{:.0}` rounds halves to even, so exactly 0.5% prints as 0%.
        assert_eq!(pct(0.005), "0%");
        assert_eq!(pct(0.006), "1%");
        assert_eq!(pct(0.015), "2%", "1.5% also rounds to even");
    }

    #[test]
    fn headers_notes_and_blanks_have_their_own_markers() {
        let lines = format_section(&section(vec![
            Row::Header("Group".into()),
            Row::note("nothing here"),
            Row::Blank,
        ]));
        assert!(lines[2].contains('\u{258c}') && lines[2].contains("Group"));
        assert!(lines[3].starts_with("  \u{2013} "), "{:?}", lines[3]);
        assert_eq!(lines[4], "", "a blank row is an empty line");
    }

    #[test]
    fn formatting_never_panics_on_multibyte_or_empty_values() {
        let rows = vec![
            Row::field("", ""),
            Row::field("🎉", "日本語"),
            Row::Header("🎉".into()),
            Row::note(""),
        ];
        let _ = format_section(&section(rows));
    }
}
