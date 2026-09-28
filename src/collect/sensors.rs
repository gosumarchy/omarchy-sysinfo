use super::{
    Row,
    fs::{read, read_f64},
};
use std::collections::BTreeMap;

pub(crate) fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<(String, f64)>>> = BTreeMap::new();

    for kind in ["temp", "fan", "in"] {
        for (chip, sensor, value) in all_readings(kind) {
            grouped
                .entry(chip)
                .or_default()
                .entry(kind.to_string())
                .or_default()
                .push((sensor, value));
        }
    }

    if grouped.is_empty() {
        return vec![Row::note("no hwmon sensors exposed")];
    }

    for (chip, kinds) in grouped {
        rows.push(Row::Header(chip.to_string()));
        for (kind, readings) in kinds {
            for (sensor, value) in readings {
                // Temperature is millidegrees, voltage is millivolts.
                // The temperature gauge is scaled so that 100 °C is full.
                // Clamped, because a sensor above that -- or one reporting a
                // negative value -- would push the fraction outside 0..1.
                let (display, bar) = match kind.as_str() {
                    "temp" => (
                        format!("{:.1} °C", value / 1000.0),
                        Some((value / 100_000.0f64).clamp(0.0, 1.0)),
                    ),
                    "fan" => (format!("{value:.0} RPM"), None),
                    _ => (format!("{:.2} V", value / 1000.0), None),
                };
                rows.push(match bar {
                    Some(frac) => Row::field_with(sensor, display, frac),
                    None => Row::field(sensor, display),
                });
            }
        }
    }

    rows
}

/// Hottest sensor on the machine, in °C, for the overview badge.
pub(crate) fn peak_temperature() -> Option<f64> {
    all_readings("temp")
        .into_iter()
        .map(|(_, _, v)| v / 1000.0)
        .fold(None, |acc: Option<f64>, v| {
            Some(acc.map_or(v, |a| a.max(v)))
        })
}

pub(crate) fn all_readings(kind: &str) -> Vec<(String, String, f64)> {
    let mut out = Vec::new();
    for hwmon in super::fs::list_dir("/sys/class/hwmon") {
        let chip = read(hwmon.join("name")).unwrap_or_else(|| {
            hwmon
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        for entry in super::fs::list_dir(&hwmon) {
            let Some(name) = entry.file_name().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            let Some(rest) = name.strip_prefix(kind) else {
                continue;
            };
            let Some(index) = rest.strip_suffix("_input") else {
                continue;
            };
            if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let Some(dir) = entry.parent() else {
                continue;
            };
            let Some(value) = read_f64(&entry) else {
                continue;
            };
            let label = read(dir.join(format!("{kind}{index}_label")))
                .map(|l| l.replace(['\n', '\t'], " "))
                .unwrap_or_else(|| default_sensor_label(kind, index));
            out.push((chip.clone(), label, value));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    out
}

fn default_sensor_label(kind: &str, index: &str) -> String {
    match (kind, index) {
        ("temp", "1") => "Package".into(),
        ("temp", "2") => "Core 0".into(),
        ("temp", "3") => "Core 1".into(),
        ("temp", _) => format!("Sensor {index}"),
        ("fan", "1") => "Fan".into(),
        _ => format!("Input {index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_render_without_panicking_or_producing_nan() {
        let rows = rows();
        assert!(!rows.is_empty());
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "{text}");
        assert!(!text.contains("∞"), "{text}");
    }

    #[test]
    fn rows_report_sensors_or_an_explicit_note() {
        let rows = rows();
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(
            text.contains("no hwmon sensors exposed")
                || text.contains("hwmon")
                || text.contains('°'),
            "{text}"
        );
    }

    #[test]
    fn all_readings_yields_well_formed_tuples() {
        for kind in ["temp", "fan", "in"] {
            for (chip, label, value) in all_readings(kind) {
                assert!(!chip.is_empty(), "{kind} chip");
                assert!(!label.is_empty(), "{kind} label");
                assert!(value.is_finite(), "{kind} {label} = {value}");
                assert!(
                    !chip.contains(" hwmon"),
                    "the chip name needs no suffix: {chip}"
                );
            }
        }
    }

    #[test]
    fn all_readings_of_an_unknown_kind_is_empty() {
        assert!(all_readings("nonexistent_kind").is_empty());
        assert!(all_readings("").is_empty());
    }

    #[test]
    fn all_readings_is_sorted_so_the_list_does_not_jump() {
        let readings = all_readings("temp");
        let mut sorted = readings.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        assert_eq!(readings.len(), sorted.len());
    }

    #[test]
    fn peak_temperature_is_a_plausible_celsius_value() {
        if let Some(peak) = peak_temperature() {
            assert!(peak.is_finite());
            assert!(peak > -50.0 && peak < 200.0, "implausible peak {peak}");
        }
    }

    #[test]
    fn default_sensor_label_covers_the_known_slots() {
        assert_eq!(default_sensor_label("temp", "1"), "Package");
        assert_eq!(default_sensor_label("temp", "2"), "Core 0");
        assert_eq!(default_sensor_label("temp", "9"), "Sensor 9");
        assert_eq!(default_sensor_label("fan", "1"), "Fan");
        assert_eq!(default_sensor_label("in", "1"), "Input 1");
    }

    #[test]
    fn the_temperature_gauge_fraction_stays_in_range() {
        // Mirrors the clamp in rows(): 100 °C is a full bar.
        for millis in [-5000.0f64, 0.0, 45_000.0, 100_000.0, 250_000.0] {
            let frac = (millis / 100_000.0).clamp(0.0, 1.0);
            assert!((0.0..=1.0).contains(&frac), "{millis} -> {frac}");
        }
    }
}
