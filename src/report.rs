//! The plain-text report: every section, one line per row, no escape codes.

use std::io::Write;
use std::time::Duration;

use crate::app::App;
use crate::collect::{Row, Section};
use crate::error::Result;

/// Print the whole report, for piping into a file or a bug report.
pub fn print() -> Result<()> {
    let mut app = App::new();
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
fn format_report(sections: &[Section]) -> Vec<String> {
    sections.iter().flat_map(format_section).collect()
}

/// The plain-text rendering of one section, one string per line.
fn format_section(section: &Section) -> Vec<String> {
    let mut lines = vec![section.title.to_uppercase(), "\u{2500}".repeat(60)];
    for row in &section.rows {
        lines.push(match row {
            Row::Header(text) => format!("\n  \u{258c} {text}"),
            Row::Field { label, value, bar } => match bar {
                // The value already carrying a percentage is left alone, so it
                // is never printed twice.
                Some(b) if !value.contains('%') => {
                    format!("  {label:<20}  {value}  {:.0}%", b.frac() * 100.0)
                }
                _ => format!("  {label:<20}  {value}"),
            },
            Row::Note(text) => format!("  \u{2013} {text}"),
            Row::Blank => String::new(),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::Bar;

    fn section(rows: Vec<Row>) -> Section {
        Section::new("Test Section", rows)
    }

    // ---- plain formatting ------------------------------------------------

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

    #[test]
    fn the_live_report_carries_no_control_characters() {
        // One NUL anywhere makes grep call the whole report a binary file, which
        // is what a UTF-16 EFI variable read as raw bytes used to do: the loader
        // description came out as "L\0i\0m\0i\0n\0e\0" and `--plain | grep`
        // answered "binary file matches" instead of printing a section.
        let mut app = App::new();
        app.collect();

        for line in format_report(&app.sections) {
            assert!(
                !line.chars().any(|c| c.is_control() && c != '\n'),
                "control character in {line:?}"
            );
        }
    }
}
