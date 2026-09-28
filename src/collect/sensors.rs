//! Temperatures, fans and voltages from `/sys/class/hwmon`.

use std::collections::BTreeMap;

use super::fs::{file_name, list_dir, read, read_f64};
use super::{Host, Row};

/// The kinds of hwmon input this report shows, each with its own unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// Millidegrees Celsius.
    Temp,
    /// Revolutions per minute.
    Fan,
    /// Millivolts.
    Voltage,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Temp, Kind::Fan, Kind::Voltage];

    /// The file prefix hwmon uses: `temp1_input`, `fan1_input`, `in0_input`.
    fn prefix(self) -> &'static str {
        match self {
            Kind::Temp => "temp",
            Kind::Fan => "fan",
            Kind::Voltage => "in",
        }
    }
}

/// One `*_input` file.
#[derive(Clone, Debug, PartialEq)]
struct Reading {
    chip: String,
    label: String,
    kind: Kind,
    /// In hwmon's unit for the kind.
    value: f64,
}

impl Reading {
    fn into_row(self) -> Row {
        match self.kind {
            // The gauge is scaled so that 100 °C is full. Clamped (inside
            // `Bar`), because a sensor above that, or one reporting a
            // negative value, would push the fraction outside 0..1.
            Kind::Temp => Row::field_with(
                self.label,
                format!("{:.1} °C", self.value / 1000.0),
                self.value / 100_000.0,
            ),
            Kind::Fan => Row::field(self.label, format!("{:.0} RPM", self.value)),
            Kind::Voltage => Row::field(self.label, format!("{:.2} V", self.value / 1000.0)),
        }
    }
}

pub(crate) fn rows(host: &Host) -> Vec<Row> {
    let mut grouped: BTreeMap<String, Vec<Reading>> = BTreeMap::new();
    for reading in Kind::ALL.into_iter().flat_map(|kind| readings(host, kind)) {
        grouped
            .entry(reading.chip.clone())
            .or_default()
            .push(reading);
    }

    if grouped.is_empty() {
        return vec![Row::note("no hwmon sensors exposed")];
    }

    let mut rows = Vec::new();
    for (chip, readings) in grouped {
        rows.push(Row::header(chip));
        rows.extend(readings.into_iter().map(Reading::into_row));
    }

    rows
}

/// Hottest sensor on the machine, in °C, for the overview.
pub(crate) fn peak_temperature(host: &Host) -> Option<f64> {
    readings(host, Kind::Temp)
        .into_iter()
        .map(|r| r.value / 1000.0)
        .reduce(f64::max)
}

/// Every input of one kind, sorted by chip then label so the list does not
/// jump between refreshes.
fn readings(host: &Host, kind: Kind) -> Vec<Reading> {
    let prefix = kind.prefix();
    let mut out = Vec::new();

    for hwmon in host.list_dir("/sys/class/hwmon") {
        let chip = read(hwmon.join("name")).unwrap_or_else(|| file_name(&hwmon));

        for entry in list_dir(&hwmon) {
            let name = file_name(&entry);
            let Some(index) = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix("_input"))
                .filter(|i| !i.is_empty() && i.chars().all(|c| c.is_ascii_digit()))
            else {
                continue;
            };
            let Some(value) = read_f64(&entry).filter(|v| v.is_finite()) else {
                continue;
            };
            let label = read(hwmon.join(format!("{prefix}{index}_label")))
                .unwrap_or_else(|| default_label(kind, index));

            out.push(Reading {
                chip: chip.clone(),
                label,
                kind,
                value,
            });
        }
    }

    out.sort_by(|a, b| a.chip.cmp(&b.chip).then_with(|| a.label.cmp(&b.label)));

    out
}

fn default_label(kind: Kind, index: &str) -> String {
    match (kind, index) {
        (Kind::Temp, "1") => "Package".into(),
        (Kind::Temp, "2") => "Core 0".into(),
        (Kind::Temp, "3") => "Core 1".into(),
        (Kind::Temp, _) => format!("Sensor {index}"),
        (Kind::Fan, "1") => "Fan".into(),
        (Kind::Fan | Kind::Voltage, _) => format!("Input {index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    fn machine() -> Fixture {
        let fx = Fixture::new();
        let cpu = "sys/class/hwmon/hwmon2";
        fx.write(&format!("{cpu}/name"), "coretemp\n");
        fx.write(&format!("{cpu}/temp1_input"), "62000\n");
        fx.write(&format!("{cpu}/temp1_label"), "Package id 0\n");
        fx.write(&format!("{cpu}/temp2_input"), "71500\n");
        fx.write(&format!("{cpu}/temp2_max"), "100000\n");
        let fan = "sys/class/hwmon/hwmon5";
        fx.write(&format!("{fan}/name"), "thinkpad\n");
        fx.write(&format!("{fan}/fan1_input"), "2400\n");
        fx.write(&format!("{fan}/in0_input"), "12050\n");
        fx
    }

    #[test]
    fn readings_are_grouped_by_chip_with_units() {
        let fx = machine();
        let rows = rows(&fx.host());

        assert_eq!(
            rows,
            [
                Row::header("coretemp"),
                Row::field_with("Core 0", "71.5 °C", 0.715),
                Row::field_with("Package id 0", "62.0 °C", 0.62),
                Row::header("thinkpad"),
                Row::field("Fan", "2400 RPM"),
                Row::field("Input 0", "12.05 V"),
            ]
        );
    }

    #[test]
    fn peak_temperature_is_the_hottest_sensor() {
        let fx = machine();

        assert_eq!(peak_temperature(&fx.host()), Some(71.5));
        assert_eq!(peak_temperature(&Fixture::new().host()), None);
    }

    #[test]
    fn a_label_with_control_characters_cannot_reach_the_terminal() {
        let fx = machine();
        fx.write("sys/class/hwmon/hwmon2/temp1_label", "Pack\x1b[2Jage\n");
        let rows = rows(&fx.host());

        assert!(!format!("{rows:?}").contains('\x1b'), "{rows:?}");
    }

    #[test]
    fn non_input_files_and_bad_values_are_skipped() {
        let fx = Fixture::new();
        let dir = "sys/class/hwmon/hwmon0";
        fx.write(&format!("{dir}/temp1_input"), "not a number\n");
        fx.write(&format!("{dir}/temp_input"), "1000\n");
        fx.write(&format!("{dir}/tempx_input"), "1000\n");

        assert_eq!(rows(&fx.host()), [Row::note("no hwmon sensors exposed")]);
    }

    #[test]
    fn a_chip_without_a_name_is_called_by_its_directory() {
        let fx = Fixture::new();
        fx.write("sys/class/hwmon/hwmon7/temp1_input", "40000\n");

        assert_eq!(rows(&fx.host())[0], Row::header("hwmon7"));
    }

    #[test]
    fn default_labels_cover_the_known_slots() {
        assert_eq!(default_label(Kind::Temp, "1"), "Package");
        assert_eq!(default_label(Kind::Temp, "2"), "Core 0");
        assert_eq!(default_label(Kind::Temp, "9"), "Sensor 9");
        assert_eq!(default_label(Kind::Fan, "1"), "Fan");
        assert_eq!(default_label(Kind::Voltage, "1"), "Input 1");
    }
}
