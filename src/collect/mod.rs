//! Read-only probes that turn this machine's `/proc` and `/sys` state into
//! plain rows the UI can print. Nothing here ever mutates the system.
//!
//! Each collector is a module that owns one subsystem and returns a `Vec<Row>`.
//! Two support modules sit underneath them: [`fs`] for reading files and
//! [`units`] for formatting the numbers inside them. The types they all speak —
//! [`Bar`], [`Row`] and [`Section`] — live here.

pub(crate) mod cpu;
pub(crate) mod display;
pub(crate) mod dmi;
pub(crate) mod fs;
pub(crate) mod gpu;
pub(crate) mod omarchy;
pub(crate) mod pci;
pub(crate) mod power;
pub(crate) mod sensors;
pub(crate) mod stats;
pub(crate) mod storage;
pub(crate) mod system;
pub(crate) mod units;
pub(crate) mod usb;

pub(crate) use stats::Stats;

/// A bar rendered next to a value, e.g. CPU usage or a temperature.
///
/// The fraction is private, so `Bar::new` is the only way to build one and every
/// `Bar` in existence is already a sane ratio. An `f64` in `0.0..=1.0` cannot
/// say that on its own: it admits `NaN`, and `clamp` passes `NaN` straight
/// through because it is neither above the max nor below the min.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Bar {
    frac: f64,
}

impl Bar {
    /// Build a bar from a raw ratio, clamping it into `0.0..=1.0`.
    ///
    /// `NaN` is pinned to `0.0` and infinities clamp to the ends, so a collector
    /// that divided by a zero total cannot produce a literal "NaN%" bar.
    pub(crate) fn new(frac: f64) -> Bar {
        let frac = if frac.is_nan() {
            0.0
        } else {
            frac.clamp(0.0, 1.0)
        };

        Bar { frac }
    }

    /// The filled fraction, always within `0.0..=1.0`.
    pub(crate) fn frac(&self) -> f64 {
        self.frac
    }
}

/// One line of a section's body.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    /// Small sub-heading that groups the fields below it.
    Header(String),
    /// A `label  value` pair, optionally with a usage bar.
    Field {
        label: String,
        value: String,
        bar: Option<Bar>,
    },
    /// A dimmed hint, used for absent or skipped devices.
    Note(String),
    Blank,
}

impl Row {
    /// A plain `label  value` row with no bar.
    pub(crate) fn field(label: impl Into<String>, value: impl Into<String>) -> Row {
        Row::Field {
            label: label.into(),
            value: value.into(),
            bar: None,
        }
    }

    /// A row carrying a usage bar, built from a raw ratio that gets clamped.
    pub(crate) fn field_with(label: impl Into<String>, value: impl Into<String>, frac: f64) -> Row {
        Row::Field {
            label: label.into(),
            value: value.into(),
            bar: Some(Bar::new(frac)),
        }
    }

    /// A dimmed hint for something absent or deliberately skipped.
    pub(crate) fn note(text: impl Into<String>) -> Row {
        Row::Note(text.into())
    }
}

/// A named page of the report.
#[derive(Debug, PartialEq)]
pub(crate) struct Section {
    pub(crate) title: String,
    pub(crate) rows: Vec<Row>,
}

impl Section {
    /// Group rows under a heading the UI can page through.
    pub(crate) fn new(title: impl Into<String>, rows: Vec<Row>) -> Section {
        Section {
            title: title.into(),
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Bar -------------------------------------------------------------

    #[test]
    fn bar_new_clamps_and_its_accessor_reports_the_stored_value() {
        // The invariant lives in `Bar::new` now that the field is private, so it
        // is worth pinning here rather than only through `Row::field_with`.
        for (raw, stored) in [
            (-5.0, 0.0),
            (0.0, 0.0),
            (0.5, 0.5),
            (1.0, 1.0),
            (5.0, 1.0),
            (f64::NAN, 0.0),
            (f64::INFINITY, 1.0),
            (f64::NEG_INFINITY, 0.0),
        ] {
            let bar = Bar::new(raw);

            assert_eq!(bar.frac(), stored, "Bar::new({raw}) stored the wrong value");
            assert!(
                (0.0..=1.0).contains(&bar.frac()),
                "Bar::new({raw}) escaped the 0.0..=1.0 range"
            );
        }
    }

    #[test]
    fn two_bars_with_the_same_fraction_are_equal() {
        // Lets tests assert on a whole `Row` without matching its internals.
        assert_eq!(Bar::new(0.5), Bar::new(0.5));
        assert_ne!(Bar::new(0.5), Bar::new(0.25));
        assert_eq!(Bar::new(9.0), Bar::new(1.0), "clamped ends compare equal");
    }

    // ---- Row constructors ------------------------------------------------

    #[test]
    fn field_with_clamps_the_bar_fraction() {
        let frac = |f: f64| match Row::field_with("l", "v", f) {
            Row::Field {
                bar: Some(Bar { frac }),
                ..
            } => frac,
            other => panic!("expected a bar, got {other:?}"),
        };

        assert_eq!(frac(-5.0), 0.0);
        assert_eq!(frac(0.0), 0.0);
        assert_eq!(frac(0.5), 0.5);
        assert_eq!(frac(1.0), 1.0);
        assert_eq!(frac(5.0), 1.0);
    }

    #[test]
    fn field_with_pins_a_nan_fraction_to_zero() {
        // NaN survives `f64::clamp`, so a collector that divided by a zero total
        // used to render a literal "NaN%" bar. It must be flattened to 0 instead.
        let frac = match Row::field_with("l", "v", f64::NAN) {
            Row::Field {
                bar: Some(Bar { frac }),
                ..
            } => frac,
            other => panic!("expected a bar, got {other:?}"),
        };

        assert_eq!(frac, 0.0);
    }

    #[test]
    fn field_with_pins_infinities_to_the_clamped_ends() {
        let frac = |f: f64| match Row::field_with("l", "v", f) {
            Row::Field {
                bar: Some(Bar { frac }),
                ..
            } => frac,
            other => panic!("expected a bar, got {other:?}"),
        };

        assert_eq!(frac(f64::INFINITY), 1.0);
        assert_eq!(frac(f64::NEG_INFINITY), 0.0);
    }

    #[test]
    fn field_has_no_bar_by_default() {
        assert!(matches!(Row::field("l", "v"), Row::Field { bar: None, .. }));
    }

    #[test]
    fn field_keeps_unicode_labels_and_values_intact() {
        let row = Row::field("Café", "日本語");

        assert!(matches!(&row, Row::Field { label, value, .. }
            if label == "Café" && value == "日本語"));
    }

    // ---- Section ---------------------------------------------------------

    #[test]
    fn a_section_keeps_its_title_and_rows() {
        let rows = vec![Row::field("a", "1")];
        let section = Section::new("Alpha", rows.clone());

        assert_eq!(section.title, "Alpha");
        assert_eq!(section.rows, rows);
    }
}
