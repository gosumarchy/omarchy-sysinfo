//! Read-only probes that turn this machine's `/proc` and `/sys` state into
//! plain rows the UI can print. Nothing here ever mutates the system.
//!
//! Each collector is a module that owns one subsystem and returns a `Vec<Row>`.
//! Support modules sit underneath them: [`fs`] for reading files through a
//! [`Host`], [`command`] and [`hypr`] for asking other programs, and [`units`]
//! for formatting the numbers. The types they all speak — [`Bar`], [`Row`] and
//! [`Section`] — live here, along with the [`Collector`] that runs them.

pub(crate) mod command;
pub(crate) mod cpu;
pub(crate) mod display;
pub(crate) mod dmi;
#[cfg(test)]
pub(crate) mod fixture;
pub(crate) mod fs;
pub(crate) mod gpu;
pub(crate) mod hypr;
pub(crate) mod omarchy;
mod overview;
pub(crate) mod pci;
pub(crate) mod power;
pub(crate) mod sensors;
pub(crate) mod stats;
pub(crate) mod storage;
pub(crate) mod system;
pub(crate) mod units;
pub(crate) mod usb;

pub(crate) use fs::Host;
pub(crate) use stats::Stats;

use crate::text::Text;

/// A bar rendered next to a value, e.g. CPU usage or a temperature.
///
/// The fraction is private, so `Bar::new` is the only way to build one and every
/// `Bar` in existence is already a sane ratio. An `f64` in `0.0..=1.0` cannot
/// say that on its own: it admits `NaN`, and `clamp` passes `NaN` straight
/// through because it is neither above the max nor below the min.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    pub(crate) fn frac(self) -> f64 {
        self.frac
    }

    /// The fraction as a whole percentage, `0..=100`.
    pub(crate) fn percent(self) -> u64 {
        units::round_u64(self.frac * 100.0)
    }
}

/// Whether a value identifies this particular machine or its owner.
///
/// The TUI shows everything; the plain report is made for pasting into bug
/// reports, so it hides identifiers unless asked not to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sensitivity {
    Public,
    /// A serial, UUID, MAC address, or something the user was looking at.
    Identifier,
    /// Useful as a whole but carrying identifiers inside it, like a kernel
    /// command line with `root=UUID=...`. The plain report masks the
    /// identifiers in place instead of hiding the whole value.
    Embedded,
}

/// One line of a section's body.
///
/// Every string is a [`Text`], so nothing a device or another program reports
/// can reach the terminal with an escape sequence still in it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    /// Small sub-heading that groups the fields below it.
    Header(Text),
    /// A `label  value` pair, optionally with a usage bar.
    Field {
        label: Text,
        value: Text,
        bar: Option<Bar>,
        sensitivity: Sensitivity,
    },
    /// A dimmed hint, used for absent or skipped devices.
    Note(Text),
    Blank,
}

impl Row {
    pub(crate) fn header(text: impl Into<Text>) -> Row {
        Row::Header(text.into())
    }

    /// A plain `label  value` row with no bar.
    pub(crate) fn field(label: impl Into<Text>, value: impl Into<Text>) -> Row {
        Row::Field {
            label: label.into(),
            value: value.into(),
            bar: None,
            sensitivity: Sensitivity::Public,
        }
    }

    /// A row carrying a usage bar, built from a raw ratio that gets clamped.
    pub(crate) fn field_with(label: impl Into<Text>, value: impl Into<Text>, frac: f64) -> Row {
        Row::Field {
            label: label.into(),
            value: value.into(),
            bar: Some(Bar::new(frac)),
            sensitivity: Sensitivity::Public,
        }
    }

    /// A row whose value identifies this machine or its user.
    pub(crate) fn identifier(label: impl Into<Text>, value: impl Into<Text>) -> Row {
        Row::Field {
            label: label.into(),
            value: value.into(),
            bar: None,
            sensitivity: Sensitivity::Identifier,
        }
    }

    /// A row whose value may contain identifiers (UUIDs) among other text.
    pub(crate) fn with_embedded_identifiers(label: impl Into<Text>, value: impl Into<Text>) -> Row {
        Row::Field {
            label: label.into(),
            value: value.into(),
            bar: None,
            sensitivity: Sensitivity::Embedded,
        }
    }

    /// A dimmed hint for something absent or deliberately skipped.
    pub(crate) fn note(text: impl Into<Text>) -> Row {
        Row::Note(text.into())
    }
}

/// A named page of the report.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Section {
    pub(crate) title: Text,
    pub(crate) rows: Vec<Row>,
}

impl Section {
    /// Group rows under a heading the UI can page through.
    pub(crate) fn new(title: impl Into<Text>, rows: Vec<Row>) -> Section {
        Section {
            title: title.into(),
            rows,
        }
    }
}

/// Everything one pass over the machine found.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) sections: Vec<Section>,
    pub(crate) hostname: Text,
    pub(crate) kernel: Text,
    pub(crate) uptime: u64,
}

/// Owns everything that outlives a single pass: the host, the CPU counters a
/// usage figure is a delta against, and the facts that cannot change while
/// the program runs.
#[derive(Debug)]
pub(crate) struct Collector {
    host: Host,
    stats: Stats,
    fixed: Fixed,
}

/// Read once per process. None of this changes without a reboot or a package
/// upgrade, and some of it (the PCI name database, the keybinding list) is
/// expensive enough that re-reading it every refresh would be felt.
#[derive(Debug)]
struct Fixed {
    cpu: cpu::CpuInfo,
    dmi: Vec<Row>,
    pci: pci::Ids,
    omarchy: omarchy::Fixed,
}

impl Collector {
    pub(crate) fn new(host: Host) -> Collector {
        let fixed = Fixed {
            cpu: cpu::CpuInfo::read(&host),
            dmi: dmi::rows(&host),
            pci: pci::Ids::load(&host),
            omarchy: omarchy::Fixed::read(&host),
        };
        let stats = Stats::new(&host);

        Collector { host, stats, fixed }
    }

    /// Sample the counters and describe the whole machine.
    pub(crate) fn collect(&mut self) -> Snapshot {
        self.stats.sample(&self.host);

        let host = &self.host;
        let stats = &self.stats;
        let fixed = &self.fixed;
        let hostname = system::hostname(host);
        let kernel = system::kernel(host);

        let mut sections = vec![
            Section::new(
                "Overview",
                overview::rows(host, stats, &fixed.cpu, &fixed.omarchy),
            ),
            Section::new("OS & kernel", system::rows(host, stats)),
            Section::new("CPU", cpu::rows(host, stats, &fixed.cpu)),
            Section::new("Memory", stats.memory().rows(host)),
            Section::new("Board & firmware", fixed.dmi.clone()),
            Section::new("Graphics", gpu::rows(host, &fixed.pci)),
            Section::new("Displays", display::rows(host)),
            Section::new("Disks", storage::rows(host)),
            Section::new("PCI devices", pci::rows(host, &fixed.pci)),
            Section::new("USB", usb::rows(host)),
        ];
        if let Some(rows) = usb::wireless(host) {
            sections.push(Section::new("Wireless", rows));
        }
        sections.push(Section::new("Sensors", sensors::rows(host)));
        sections.push(Section::new("Power & battery", power::rows(host)));
        sections.push(Section::new("Omarchy", omarchy::rows(host, &fixed.omarchy)));

        Snapshot {
            sections,
            hostname: Text::new(hostname),
            kernel: Text::new(kernel),
            uptime: stats.uptime(),
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "these tests pin exact, exactly representable results"
)]
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
            Row::Field { bar: Some(bar), .. } => bar.frac(),
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
            Row::Field { bar: Some(bar), .. } => bar.frac(),
            other => panic!("expected a bar, got {other:?}"),
        };

        assert_eq!(frac, 0.0);
    }

    #[test]
    fn field_with_pins_infinities_to_the_clamped_ends() {
        let frac = |f: f64| match Row::field_with("l", "v", f) {
            Row::Field { bar: Some(bar), .. } => bar.frac(),
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

    #[test]
    fn row_constructors_sanitise_their_text() {
        let row = Row::field("label\x1b[31m", "value\x07");

        let Row::Field { label, value, .. } = row else {
            panic!("expected a field");
        };
        assert!(!label.chars().any(char::is_control));
        assert!(!value.chars().any(char::is_control));
    }

    #[test]
    fn identifier_rows_are_marked_as_such() {
        assert!(matches!(
            Row::identifier("MAC", "aa:bb"),
            Row::Field {
                sensitivity: Sensitivity::Identifier,
                ..
            }
        ));
        assert!(matches!(
            Row::field("MAC", "aa:bb"),
            Row::Field {
                sensitivity: Sensitivity::Public,
                ..
            }
        ));
    }

    #[test]
    fn bar_percent_rounds_to_a_whole_number() {
        assert_eq!(Bar::new(0.0).percent(), 0);
        assert_eq!(Bar::new(0.499).percent(), 50);
        assert_eq!(Bar::new(1.0).percent(), 100);
    }

    // ---- Collector against a fixture ---------------------------------------

    #[test]
    fn a_collector_over_an_empty_machine_still_produces_every_section() {
        let fx = fixture::Fixture::new();
        let mut collector = Collector::new(fx.host());
        let snapshot = collector.collect();

        let titles: Vec<&str> = snapshot.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Overview",
                "OS & kernel",
                "CPU",
                "Memory",
                "Board & firmware",
                "Graphics",
                "Displays",
                "Disks",
                "PCI devices",
                "USB",
                "Sensors",
                "Power & battery",
                "Omarchy",
            ]
        );
        assert!(
            snapshot.sections.iter().all(|s| !s.rows.is_empty()),
            "an empty section should say why it is empty"
        );
    }

    #[test]
    fn wireless_gets_its_own_section_only_when_there_is_a_radio() {
        let fx = fixture::Fixture::new();
        fx.mkdir("sys/class/net/wlan0/wireless");
        fx.write("sys/class/net/wlan0/address", "aa:bb:cc:dd:ee:ff\n");

        let snapshot = Collector::new(fx.host()).collect();
        let titles: Vec<&str> = snapshot.sections.iter().map(|s| s.title.as_str()).collect();

        let usb = titles.iter().position(|t| *t == "USB").expect("usb");
        assert_eq!(titles.get(usb + 1), Some(&"Wireless"));
    }
}
