//! The plain-text report: every section, one line per row, no escape codes.

use std::io::{self, Write};
use std::time::Duration;

use crate::collect::{Collector, Host, Row, Section, Sensitivity};
use crate::error::Result;

/// CPU usage is a delta between two readings; this is the gap between them.
const SAMPLE_GAP: Duration = Duration::from_millis(150);

/// Whether the report includes values that identify this machine or its
/// user. The report is made for pasting into bug reports, so they are
/// hidden unless asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Identifiers {
    Hide,
    Show,
}

const HIDDEN: &str = "<hidden>";

/// Describe the machine on stdout.
///
/// A reader that goes away (`--plain | head`) ends the report quietly; any
/// other write failure (a full disk behind `> report.txt`) is an error, so
/// the exit status does not claim a report was written when it was not.
pub(crate) fn print(identifiers: Identifiers) -> Result<()> {
    let mut collector = Collector::new(Host::live());
    std::thread::sleep(SAMPLE_GAP);
    let snapshot = collector.collect();

    let mut out = io::BufWriter::new(io::stdout().lock());
    let written =
        write_report(&mut out, &snapshot.sections, identifiers).and_then(|()| out.flush());

    match written {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

fn write_report(
    out: &mut impl Write,
    sections: &[Section],
    identifiers: Identifiers,
) -> io::Result<()> {
    for line in format_report(sections, identifiers) {
        writeln!(out, "{line}")?;
    }

    Ok(())
}

/// The whole report, one string per line.
///
/// This output is meant to be piped into grep, a file or a bug report, so it
/// carries no escape sequences and nothing else writes a heading: the heading
/// and rule used to be emitted both here and in `format_section`, which
/// printed every section title twice.
fn format_report(sections: &[Section], identifiers: Identifiers) -> Vec<String> {
    let mut lines: Vec<String> = sections
        .iter()
        .flat_map(|s| format_section(s, identifiers))
        .collect();

    let hid_something = identifiers == Identifiers::Hide
        && sections.iter().flat_map(|s| &s.rows).any(|r| {
            matches!(
                r,
                Row::Field {
                    sensitivity: Sensitivity::Identifier,
                    ..
                }
            )
        });
    if hid_something {
        lines.push(String::new());
        lines.push(format!(
            "Values shown as {HIDDEN} identify this machine; run with --plain --identifiers to include them."
        ));
    }

    lines
}

/// The plain-text rendering of one section, one string per line.
fn format_section(section: &Section, identifiers: Identifiers) -> Vec<String> {
    let mut lines = vec![section.title.to_uppercase(), "\u{2500}".repeat(60)];

    for row in &section.rows {
        match row {
            Row::Header(text) => {
                // A blank line before each group, as its own line: embedding
                // the newline in the heading broke "one string per line".
                lines.push(String::new());
                lines.push(format!("  \u{258c} {text}"));
            }
            Row::Field {
                label,
                value,
                bar,
                sensitivity,
            } => {
                let value = match (sensitivity, identifiers) {
                    (Sensitivity::Identifier, Identifiers::Hide) => HIDDEN,
                    (Sensitivity::Identifier, Identifiers::Show) | (Sensitivity::Public, _) => {
                        value.as_str()
                    }
                };
                lines.push(match bar {
                    // The value already carrying a percentage is left alone, so
                    // it is never printed twice.
                    Some(b) if !value.contains('%') => {
                        format!("  {label:<20}  {value}  {}%", b.percent())
                    }
                    Some(_) | None => format!("  {label:<20}  {value}"),
                });
            }
            Row::Note(text) => lines.push(format!("  \u{2013} {text}")),
            Row::Blank => lines.push(String::new()),
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    fn section(rows: Vec<Row>) -> Section {
        Section::new("Test Section", rows)
    }

    fn lines(rows: Vec<Row>) -> Vec<String> {
        format_section(&section(rows), Identifiers::Hide)
    }

    // ---- plain formatting ------------------------------------------------

    #[test]
    fn a_section_starts_with_its_uppercased_title_and_a_rule() {
        let lines = lines(vec![]);
        assert_eq!(lines[0], "TEST SECTION");
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
        let report = format_report(&sections, Identifiers::Hide);
        for title in ["ALPHA", "BETA"] {
            let count = report.iter().filter(|l| l.contains(title)).count();
            assert_eq!(count, 1, "{title} appeared {count} times: {report:?}");
        }
        assert_eq!(
            report
                .iter()
                .filter(|l| !l.is_empty() && l.chars().all(|c| c == '\u{2500}'))
                .count(),
            2,
            "one rule per section: {report:?}"
        );
    }

    #[test]
    fn every_line_is_one_line() {
        let report = format_report(
            &[section(vec![Row::header("Inner"), Row::field("B", "2")])],
            Identifiers::Hide,
        );
        for line in &report {
            assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
        }
        assert_eq!(report[2], "", "a group is preceded by a blank line");
        assert_eq!(report[3], "  \u{258c} Inner");
    }

    #[test]
    fn a_report_of_no_sections_is_empty() {
        assert!(format_report(&[], Identifiers::Hide).is_empty());
    }

    #[test]
    fn labels_are_padded_into_a_column() {
        let lines = lines(vec![Row::field("CPU", "12%")]);
        // Two leading spaces, the label padded to 20 columns, then two more.
        assert_eq!(lines[2], "  CPU                   12%");
    }

    #[test]
    fn a_long_label_overflows_its_column_rather_than_truncating() {
        let long = "a-really-quite-long-label-indeed";
        let lines = lines(vec![Row::field(long, "v")]);
        assert!(lines[2].starts_with(&format!("  {long}")));
        assert!(lines[2].ends_with('v'));
    }

    #[test]
    fn a_bar_adds_its_percentage_unless_the_value_already_has_one() {
        let lines = lines(vec![Row::field_with("Memory", "1.0 GiB / 2.0 GiB", 0.5)]);
        assert!(
            lines[2].ends_with("50%"),
            "expected a gauge value: {:?}",
            lines[2]
        );
        assert_eq!(lines[2].matches('%').count(), 1, "must not print it twice");

        let lines = self::lines(vec![Row::field_with("Memory", "50%", 0.5)]);
        assert_eq!(lines[2].matches('%').count(), 1, "must not print it twice");
        assert!(lines[2].ends_with("50%"));
    }

    #[test]
    fn gauge_percentages_round_to_whole_numbers() {
        let pct = |frac: f64| {
            let lines = lines(vec![Row::field_with("x", "v", frac)]);
            lines[2]
                .rsplit(' ')
                .next()
                .map(str::to_string)
                .unwrap_or_default()
        };
        assert_eq!(pct(0.0), "0%");
        assert_eq!(pct(0.999), "100%");
        assert_eq!(pct(0.5), "50%");
        assert_eq!(pct(0.004), "0%");
        assert_eq!(pct(0.006), "1%");
    }

    #[test]
    fn headers_notes_and_blanks_have_their_own_markers() {
        let lines = lines(vec![
            Row::header("Group"),
            Row::note("nothing here"),
            Row::Blank,
        ]);
        assert!(lines[3].contains('\u{258c}') && lines[3].contains("Group"));
        assert!(lines[4].starts_with("  \u{2013} "), "{:?}", lines[4]);
        assert_eq!(lines[5], "", "a blank row is an empty line");
    }

    #[test]
    fn formatting_never_panics_on_multibyte_or_empty_values() {
        let _ = lines(vec![
            Row::field("", ""),
            Row::field("🎉", "日本語"),
            Row::header("🎉"),
            Row::note(""),
        ]);
    }

    // ---- identifiers ---------------------------------------------------------

    #[test]
    fn identifiers_are_hidden_by_default_and_the_report_says_why() {
        let sections = [section(vec![
            Row::identifier("Serial number", "PF3ABCDE"),
            Row::field("Model", "ThinkPad"),
        ])];
        let report = format_report(&sections, Identifiers::Hide).join("\n");

        assert!(!report.contains("PF3ABCDE"), "{report}");
        assert!(report.contains("<hidden>"), "{report}");
        assert!(report.contains("ThinkPad"), "{report}");
        assert!(report.contains("--identifiers"), "{report}");
    }

    #[test]
    fn identifiers_are_shown_when_asked_for() {
        let sections = [section(vec![Row::identifier("Serial number", "PF3ABCDE")])];
        let report = format_report(&sections, Identifiers::Show).join("\n");

        assert!(report.contains("PF3ABCDE"), "{report}");
        assert!(!report.contains("<hidden>"), "{report}");
    }

    #[test]
    fn a_report_with_nothing_to_hide_has_no_footnote() {
        let sections = [section(vec![Row::field("Model", "ThinkPad")])];

        assert!(
            !format_report(&sections, Identifiers::Hide)
                .join("\n")
                .contains("--identifiers")
        );
    }

    // ---- writing -------------------------------------------------------------

    /// A writer that fails like a full disk.
    struct Full;

    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::StorageFull))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_write_failure_is_reported_not_swallowed() {
        // `--plain > /dev/full` used to exit 0 with nothing written.
        let sections = [section(vec![Row::field("a", "b")])];
        let err = write_report(&mut Full, &sections, Identifiers::Hide).expect_err("full");

        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    }

    #[test]
    fn a_report_of_a_whole_machine_carries_no_control_characters() {
        // One NUL anywhere makes grep call the whole report a binary file, and
        // an ESC from a device name would be obeyed by the reader's terminal.
        let fx = Fixture::new();
        fx.write("sys/bus/usb/devices/1-1/product", "evil\x1b]0;pwned\x07\n");
        fx.write("sys/class/hwmon/hwmon0/name", "chip\x00\n");
        fx.write("sys/class/hwmon/hwmon0/temp1_input", "40000\n");
        let snapshot = Collector::new(fx.host()).collect();

        let mut out = Vec::new();
        write_report(&mut out, &snapshot.sections, Identifiers::Show).expect("write");
        let text = String::from_utf8(out).expect("utf-8");

        assert!(text.contains("evil"), "the device is still listed");
        assert!(
            !text.chars().any(|c| c.is_control() && c != '\n'),
            "control character in the report"
        );
    }
}
