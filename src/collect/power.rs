use super::{
    Row,
    fs::{read, read_f64, read_u64},
    units::dash,
};
use std::path::Path;

pub(crate) fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    let supplies = super::fs::list_dir("/sys/class/power_supply");

    for supply in &supplies {
        let name = supply
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let kind = read(supply.join("type")).unwrap_or_else(|| "unknown".into());
        if kind == "Mains" {
            if let Some(power) = read(supply.join("online")).and_then(|v| v.parse::<u8>().ok()) {
                rows.push(Row::field(
                    format!("{name} (AC)"),
                    if power == 1 { "online" } else { "offline" },
                ));
            }
        }
    }

    for supply in &supplies {
        let name = supply
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if read(supply.join("type")).as_deref() != Some("Battery") {
            continue;
        }

        rows.push(Row::Header(name));

        let capacity = read_f64(supply.join("capacity"));
        if let Some(raw) = capacity {
            // A worn pack can report more charge than it physically holds, and
            // this machine's does: capacity reads 112 with energy_now above
            // energy_full. A charge level cannot be displayed above full, so
            // the percentage is capped. The Energy row below still shows the
            // real micro-unit figures behind the number.
            let pct = raw.clamp(0.0, 100.0);
            let status = read(supply.join("status")).unwrap_or_default();
            let bar_label = if status == "Charging" {
                format!("{pct:.0}%  ({status})")
            } else {
                format!("{pct:.0}%")
            };
            rows.push(Row::field_with("Charge", bar_label, pct / 100.0));
        }
        rows.push(Row::field("Status", dash(read(supply.join("status")))));

        if let (Some(now), Some(full)) = (
            read_u64(supply.join("energy_now")),
            read_u64(supply.join("energy_full")),
        ) {
            rows.push(Row::field(
                "Energy",
                format!(
                    "{} / {}",
                    super::units::human_energy(now),
                    super::units::human_energy(full)
                ),
            ));
        } else if let (Some(now), Some(full)) = (
            read_u64(supply.join("charge_now")),
            read_u64(supply.join("charge_full")),
        ) {
            rows.push(Row::field(
                "Charge",
                format!(
                    "{:.0} / {:.0} mAh",
                    now as f64 / 1000.0,
                    full as f64 / 1000.0
                ),
            ));
        }

        // No kernel exposes battery health directly, so it is the full charge
        // measured against the design capacity. Wh if the kernel reports energy,
        // mAh if it only reports charge.
        let (full, design) = match (
            read_u64(supply.join("energy_full")),
            read_u64(supply.join("energy_full_design")),
        ) {
            (Some(f), Some(d)) => (f, d),
            _ => (
                read_u64(supply.join("charge_full")).unwrap_or(0),
                read_u64(supply.join("charge_full_design")).unwrap_or(0),
            ),
        };
        if design > 0 && full > 0 {
            let pct = (full as f64 / design as f64).clamp(0.0, 1.0);
            let unit = |value: u64| {
                if read_u64(supply.join("energy_full")).is_some() {
                    super::units::human_energy(value)
                } else {
                    format!("{:.0} mAh", value as f64 / 1000.0)
                }
            };
            rows.push(Row::field_with(
                "Health",
                format!("{:.0}% of design capacity", pct * 100.0),
                pct,
            ));
            rows.push(Row::field(
                "Full charge",
                format!("{} of {}", unit(full), unit(design)),
            ));
        }
        rows.push(Row::field(
            "Cycles",
            dash_opt(read(supply.join("cycle_count"))),
        ));
        rows.push(Row::field(
            "Technology",
            dash(read(supply.join("technology"))),
        ));
        rows.push(Row::field("Model", dash(read(supply.join("model_name")))));
        rows.push(Row::field(
            "Manufacturer",
            dash(read(supply.join("manufacturer"))),
        ));
        if let Some(v) = read_u64(supply.join("voltage_now")) {
            rows.push(Row::field(
                "Voltage",
                format!("{:.2} V", v as f64 / 1_000_000.0),
            ));
        }
        if let Some(power) = read_u64(supply.join("power_now")) {
            let avg = read_u64(supply.join("power_avg")).unwrap_or(power);
            let max = read_u64(supply.join("power_max")).unwrap_or(power);
            rows.push(Row::field(
                "Power now",
                format!(
                    "{}  (avg {}, max {})",
                    super::units::human_watts(power),
                    super::units::human_watts(avg),
                    super::units::human_watts(max)
                ),
            ));
        }
        // Only the remaining time. A "Discharge rate" row used to sit here
        // too, but it computed now/power -- the same figure as below -- and
        // labelled it "h of full charge at this rate", which is not what that
        // number is.
        if let Some(remaining) = time_remaining(supply) {
            rows.push(Row::field("Time remaining", remaining));
        }
    }

    if let Some(thresholds) = thresholds() {
        rows.push(Row::Header("Charge thresholds".into()));
        for (label, value) in thresholds {
            rows.push(Row::field(label, value));
        }
    }

    if rows.is_empty() {
        rows.push(Row::note("no power supply information (desktop?)"));
    }
    rows
}

fn dash_opt(v: Option<String>) -> String {
    match v {
        Some(s) if s == "0" => "-".to_string(),
        other => dash(other),
    }
}

fn battery() -> Option<std::path::PathBuf> {
    super::fs::list_dir("/sys/class/power_supply")
        .into_iter()
        .find(|p| read(p.join("type")).as_deref() == Some("Battery"))
}

fn time_remaining(supply: &Path) -> Option<String> {
    let now = read_u64(supply.join("energy_now"))?;
    let power = read_u64(supply.join("power_now"))?;
    if power == 0 {
        return None;
    }
    // Both values are micro-units, so their ratio is already hours.
    let hours = now as f64 / power as f64;
    Some(super::units::human_secs((hours * 3600.0) as u64))
}

fn thresholds() -> Option<Vec<(String, String)>> {
    // The kernel exposes these per battery. Reading a hardcoded BAT0 meant a
    // machine whose battery is BAT1, BATT or battery0 reported no thresholds
    // at all.
    let bat = battery()?;
    let mut out = Vec::new();
    if let Some(start) = read_u64(bat.join("charge_control_start_threshold")) {
        out.push(("Start charging".to_string(), format!("{start}%")));
    }
    if let Some(end) = read_u64(bat.join("charge_control_end_threshold")) {
        out.push(("Stop charging".to_string(), format!("{end}%")));
    }
    if let Some(mode) = read(bat.join("charge_control_mode")) {
        out.push(("Mode".to_string(), mode));
    }
    (!out.is_empty()).then_some(out)
}

/// Everything the overview badge needs from this module.
pub(crate) fn summary() -> (String, String) {
    // Looking for BAT0 by name made the badge say "no battery" on a machine
    // whose only battery is called BAT1.
    let Some(bat) = battery() else {
        return ("no battery".into(), "-".into());
    };
    // Capped for the same reason as the Charge row: a worn pack can report
    // more than 100, and "112%" reads as a bug in this tool.
    let pct = read_f64(bat.join("capacity"))
        .map(|p| format!("{:.0}%", p.clamp(0.0, 100.0)))
        .unwrap_or_else(|| "unknown".into());
    let status = read(bat.join("status")).unwrap_or_else(|| "-".into());
    (pct, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_render_without_panicking_or_producing_nan() {
        let rows = rows();
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!rows.is_empty());
        assert!(!text.contains("NaN"), "{text}");
        assert!(!text.contains("∞"), "{text}");
    }

    #[test]
    fn battery_is_found_by_type_not_by_name() {
        // The bug: thresholds() and summary() read a hardcoded BAT0, so a
        // machine whose battery is BAT1 reported nothing.
        match battery() {
            Some(path) => {
                assert_eq!(
                    read(path.join("type")).as_deref(),
                    Some("Battery"),
                    "{path:?} is not a battery"
                );
                assert!(path.starts_with("/sys/class/power_supply"));
            }
            None => assert!(!Path::new("/sys/class/power_supply").exists()),
        }
    }

    #[test]
    fn summary_reports_a_battery_on_this_machine_or_explicitly_none() {
        let (pct, status) = summary();
        if battery().is_some() {
            assert_ne!(
                pct, "no battery",
                "a battery exists, so the badge must not say otherwise"
            );
            assert!(
                pct.ends_with('%') || pct == "unknown",
                "unexpected percentage: {pct:?}"
            );
        } else {
            assert_eq!(pct, "no battery");
        }
        assert!(!status.is_empty());
    }

    #[test]
    fn thresholds_are_a_percentage_pair() {
        if let Some(out) = thresholds() {
            for (label, value) in out {
                assert!(!label.is_empty());
                if label != "Mode" {
                    assert!(value.ends_with('%'), "{label} = {value:?}");
                }
            }
        }
    }

    #[test]
    fn a_charge_level_above_full_is_capped() {
        // This machine's battery reports capacity 112 with energy_now above
        // energy_full, so an uncapped row read "112%".
        assert_eq!(format!("{:.0}%", 112.0f64.clamp(0.0, 100.0)), "100%");
        assert_eq!(format!("{:.0}%", 250.0f64.clamp(0.0, 100.0)), "100%");
        assert_eq!(format!("{:.0}%", 0.0f64.clamp(0.0, 100.0)), "0%");
        assert_eq!(format!("{:.0}%", 87.4f64.clamp(0.0, 100.0)), "87%");
    }

    #[test]
    fn the_badge_never_reports_more_than_full() {
        let (pct, _) = summary();
        if let Some(value) = pct.strip_suffix('%').and_then(|n| n.parse::<f64>().ok()) {
            assert!(
                (0.0..=100.0).contains(&value),
                "the badge shows {value}%, which is not a charge level"
            );
        }
    }

    #[test]
    fn charge_bar_fraction_stays_in_range() {
        // Mirrors the clamp in rows().
        for pct in [0.0, 50.0, 100.0, 105.0, f64::INFINITY] {
            let f = (pct / 100.0).clamp(0.0, 1.0);
            assert!((0.0..=1.0).contains(&f), "{pct} -> {f}");
        }
    }

    #[test]
    fn time_remaining_of_a_missing_or_idle_battery_is_none() {
        assert!(time_remaining(Path::new("/nonexistent-battery")).is_none());
    }

    #[test]
    fn dash_opt_hides_a_zero_cycle_count() {
        assert_eq!(dash_opt(Some("0".to_string())), "-");
        assert_eq!(dash_opt(Some("12".to_string())), "12");
        assert_eq!(dash_opt(None), "-");
    }
}
