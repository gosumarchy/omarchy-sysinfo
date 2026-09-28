//! AC adapters and batteries from `/sys/class/power_supply`.

use std::path::{Path, PathBuf};

use super::fs::{file_name, read, read_f64, read_u64};
use super::units::{approx_f64, dash, human_energy, human_secs, human_watts, round_u64};
use super::{Host, Row};

/// What a power supply is, and whether it powers this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Supply {
    Mains,
    /// A battery that runs the computer.
    System,
    /// A battery inside a wireless mouse, keyboard or headset. The kernel
    /// reports these as `type=Battery` too, told apart only by
    /// `scope=Device`; treating one as the laptop battery put a mouse's
    /// charge on a desktop's overview.
    Peripheral,
    Other,
}

fn supply_kind(path: &Path) -> Supply {
    match read(path.join("type")).as_deref() {
        Some("Mains") => Supply::Mains,
        Some("Battery") if read(path.join("scope")).as_deref() == Some("Device") => {
            Supply::Peripheral
        }
        Some("Battery") => Supply::System,
        _ => Supply::Other,
    }
}

pub(crate) fn rows(host: &Host) -> Vec<Row> {
    let supplies: Vec<(PathBuf, Supply)> = host
        .list_dir("/sys/class/power_supply")
        .into_iter()
        .map(|p| {
            let kind = supply_kind(&p);
            (p, kind)
        })
        .collect();
    let of_kind = |kind: Supply| {
        supplies
            .iter()
            .filter(move |(_, k)| *k == kind)
            .map(|(p, _)| p.as_path())
    };

    let mut rows = Vec::new();

    for adapter in of_kind(Supply::Mains) {
        if let Some(online) = read(adapter.join("online")) {
            rows.push(Row::field(
                format!("{} (AC)", file_name(adapter)),
                if online == "1" { "online" } else { "offline" },
            ));
        }
    }

    for battery in of_kind(Supply::System) {
        rows.extend(battery_rows(battery));
    }

    if let Some(battery) = of_kind(Supply::System).next() {
        rows.extend(threshold_rows(battery));
    }

    let peripherals: Vec<Row> = of_kind(Supply::Peripheral).map(peripheral_row).collect();
    if !peripherals.is_empty() {
        rows.push(Row::header("Peripherals"));
        rows.extend(peripherals);
    }

    if rows.is_empty() {
        rows.push(Row::note("no power supply information (desktop?)"));
    }

    rows
}

/// A charge level as a percentage, capped at 100.
///
/// A worn pack can report more charge than it physically holds (capacity 112
/// with `energy_now` above `energy_full`). A charge level cannot be shown
/// above full, and "112%" reads as a bug in this tool; the Energy row still
/// shows the real figures behind the number.
fn capacity(battery: &Path) -> Option<f64> {
    read_f64(battery.join("capacity")).map(|pct| pct.clamp(0.0, 100.0))
}

fn battery_rows(battery: &Path) -> Vec<Row> {
    let status = read(battery.join("status"));
    let charging = status.as_deref() == Some("Charging");
    let mut rows = vec![Row::header(file_name(battery))];

    if let Some(pct) = capacity(battery) {
        let label = if charging {
            format!("{pct:.0}%  (Charging)")
        } else {
            format!("{pct:.0}%")
        };
        rows.push(Row::field_with("Charge", label, pct / 100.0));
    }
    rows.push(Row::field("Status", dash(status)));

    let reading = Reading::read(battery);
    if let Some(reading) = &reading {
        rows.push(Row::field(
            "Energy",
            format!(
                "{} / {}",
                reading.show(reading.now),
                reading.show(reading.full)
            ),
        ));
        if let Some(design) = reading.design
            && design > 0
            && reading.full > 0
        {
            let health = super::units::fraction(reading.full, design).clamp(0.0, 1.0);
            rows.push(Row::field_with(
                "Health",
                format!("{:.0}% of design capacity", health * 100.0),
                health,
            ));
            rows.push(Row::field(
                "Full charge",
                format!("{} of {}", reading.show(reading.full), reading.show(design)),
            ));
        }
    }

    rows.push(Row::field(
        "Cycles",
        nonzero_or_dash(read(battery.join("cycle_count"))),
    ));
    rows.push(Row::field(
        "Technology",
        dash(read(battery.join("technology"))),
    ));
    rows.push(Row::field("Model", dash(read(battery.join("model_name")))));
    rows.push(Row::field(
        "Manufacturer",
        dash(read(battery.join("manufacturer"))),
    ));
    if let Some(v) = read_u64(battery.join("voltage_now")) {
        rows.push(Row::field(
            "Voltage",
            format!("{:.2} V", approx_f64(v) / 1_000_000.0),
        ));
    }
    if let Some(power) = read_u64(battery.join("power_now")) {
        let avg = read_u64(battery.join("power_avg")).unwrap_or(power);
        let max = read_u64(battery.join("power_max")).unwrap_or(power);
        rows.push(Row::field(
            "Power now",
            format!(
                "{}  (avg {}, max {})",
                human_watts(power),
                human_watts(avg),
                human_watts(max)
            ),
        ));
    }
    if let Some(reading) = &reading
        && let Some((label, secs)) = reading.time_left(charging)
    {
        rows.push(Row::field(label, human_secs(secs)));
    }

    rows
}

/// A battery's level in whichever unit its driver reports.
///
/// Most report energy (µWh) and power (µW); some only charge (µAh) and
/// current (µA). The ratios work out the same either way.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Reading {
    unit: Unit,
    now: u64,
    full: u64,
    design: Option<u64>,
    /// Draw in the matching unit: µW for energy, µA for charge.
    rate: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Energy,
    Charge,
}

impl Reading {
    fn read(battery: &Path) -> Option<Reading> {
        let get = |file: &str| read_u64(battery.join(file));

        if let (Some(now), Some(full)) = (get("energy_now"), get("energy_full")) {
            return Some(Reading {
                unit: Unit::Energy,
                now,
                full,
                design: get("energy_full_design"),
                rate: get("power_now"),
            });
        }

        // Some drivers report current as signed, negative while discharging.
        let current =
            read(battery.join("current_now")).and_then(|c| c.trim_start_matches('-').parse().ok());

        Some(Reading {
            unit: Unit::Charge,
            now: get("charge_now")?,
            full: get("charge_full")?,
            design: get("charge_full_design"),
            rate: current,
        })
    }

    fn show(&self, value: u64) -> String {
        match self.unit {
            Unit::Energy => human_energy(value),
            Unit::Charge => format!("{:.0} mAh", approx_f64(value) / 1000.0),
        }
    }

    /// How long until empty while discharging, or until full while
    /// charging. The old row always divided the charge left by the draw,
    /// which while charging is a time to empty that is not happening.
    fn time_left(&self, charging: bool) -> Option<(&'static str, u64)> {
        let rate = self.rate.filter(|r| *r > 0)?;
        let (label, remaining) = if charging {
            ("Time to full", self.full.saturating_sub(self.now))
        } else {
            ("Time remaining", self.now)
        };

        // Both values share a unit, so their ratio is already hours.
        let hours = approx_f64(remaining) / approx_f64(rate);

        Some((label, round_u64(hours * 3600.0)))
    }
}

fn threshold_rows(battery: &Path) -> Vec<Row> {
    let mut rows = Vec::new();

    // The kernel exposes these per battery. Reading a hardcoded BAT0 meant a
    // machine whose battery is BAT1, BATT or battery0 reported no thresholds.
    if let Some(start) = read_u64(battery.join("charge_control_start_threshold")) {
        rows.push(Row::field("Start charging", format!("{start}%")));
    }
    if let Some(end) = read_u64(battery.join("charge_control_end_threshold")) {
        rows.push(Row::field("Stop charging", format!("{end}%")));
    }
    if let Some(mode) = read(battery.join("charge_control_mode")) {
        rows.push(Row::field("Mode", mode));
    }

    if !rows.is_empty() {
        rows.insert(0, Row::header("Charge thresholds"));
    }

    rows
}

fn peripheral_row(battery: &Path) -> Row {
    let name = read(battery.join("model_name")).unwrap_or_else(|| file_name(battery));

    match capacity(battery) {
        Some(pct) => Row::field_with(name, format!("{pct:.0}%"), pct / 100.0),
        None => Row::field(
            name,
            read(battery.join("capacity_level")).unwrap_or_else(|| "unknown".into()),
        ),
    }
}

/// A cycle count of zero means the firmware does not track it.
fn nonzero_or_dash(value: Option<String>) -> String {
    match value {
        Some(v) if v == "0" => "-".to_string(),
        other => dash(other),
    }
}

/// The first battery that powers the machine, not a peripheral's.
fn system_battery(host: &Host) -> Option<PathBuf> {
    host.list_dir("/sys/class/power_supply")
        .into_iter()
        .find(|p| supply_kind(p) == Supply::System)
}

/// The overview's battery: charge and status.
pub(crate) fn summary(host: &Host) -> (String, String) {
    let Some(battery) = system_battery(host) else {
        return ("no battery".into(), "-".into());
    };

    let pct = capacity(&battery).map_or_else(|| "unknown".into(), |p| format!("{p:.0}%"));
    let status = read(battery.join("status")).unwrap_or_else(|| "-".into());

    (pct, status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    fn text(rows: &[Row]) -> String {
        format!("{rows:?}")
    }

    fn laptop(status: &str) -> Fixture {
        let fx = Fixture::new();
        let bat = "sys/class/power_supply/BAT1";
        fx.write(&format!("{bat}/type"), "Battery\n");
        fx.write(&format!("{bat}/scope"), "System\n");
        fx.write(&format!("{bat}/status"), &format!("{status}\n"));
        fx.write(&format!("{bat}/capacity"), "112\n");
        fx.write(&format!("{bat}/energy_now"), "30000000\n");
        fx.write(&format!("{bat}/energy_full"), "60000000\n");
        fx.write(&format!("{bat}/energy_full_design"), "80000000\n");
        fx.write(&format!("{bat}/power_now"), "15000000\n");
        fx.write(&format!("{bat}/cycle_count"), "0\n");
        fx.write(&format!("{bat}/charge_control_end_threshold"), "80\n");
        fx.write("sys/class/power_supply/AC/type", "Mains\n");
        fx.write("sys/class/power_supply/AC/online", "1\n");
        fx
    }

    fn mouse(fx: &Fixture) {
        let hid = "sys/class/power_supply/hidpp_battery_0";
        fx.write(&format!("{hid}/type"), "Battery\n");
        fx.write(&format!("{hid}/scope"), "Device\n");
        fx.write(&format!("{hid}/model_name"), "MX Master 3\n");
        fx.write(&format!("{hid}/capacity"), "55\n");
        fx.write(&format!("{hid}/status"), "Discharging\n");
    }

    #[test]
    fn a_discharging_laptop_reports_time_remaining() {
        let fx = laptop("Discharging");
        let text = text(&rows(&fx.host()));

        // 30 Wh left at 15 W is two hours.
        assert!(text.contains("Time remaining"), "{text}");
        assert!(text.contains("2h 0m 0s"), "{text}");
        assert!(!text.contains("Time to full"), "{text}");
    }

    #[test]
    fn a_charging_laptop_reports_time_to_full_not_time_to_empty() {
        let fx = laptop("Charging");
        let text = text(&rows(&fx.host()));

        // 30 Wh still to go at 15 W is also two hours, but labelled as such.
        assert!(text.contains("Time to full"), "{text}");
        assert!(!text.contains("Time remaining"), "{text}");
        assert!(text.contains("(Charging)"), "{text}");
    }

    #[test]
    fn a_charge_level_above_full_is_capped() {
        let fx = laptop("Discharging");
        let text = text(&rows(&fx.host()));

        assert!(text.contains("\"100%\""), "{text}");
        assert!(!text.contains("112%"), "{text}");
        assert_eq!(summary(&fx.host()).0, "100%");
    }

    #[test]
    fn health_thresholds_and_the_adapter_are_shown() {
        let fx = laptop("Discharging");
        let text = text(&rows(&fx.host()));

        assert!(text.contains("75% of design capacity"), "{text}");
        assert!(text.contains("Stop charging"), "{text}");
        assert!(text.contains("AC (AC)"), "{text}");
        assert!(text.contains("\"online\""), "{text}");
    }

    #[test]
    fn a_mouse_is_a_peripheral_not_the_system_battery() {
        // On a desktop the only "Battery" is the mouse, and the overview used
        // to report its charge as the machine's.
        let fx = Fixture::new();
        mouse(&fx);

        assert_eq!(summary(&fx.host()).0, "no battery");
        let text = text(&rows(&fx.host()));
        assert!(text.contains("Peripherals"), "{text}");
        assert!(text.contains("MX Master 3"), "{text}");
        assert!(!text.contains("Charge thresholds"), "{text}");
    }

    #[test]
    fn a_laptop_with_a_mouse_still_summarises_the_laptop() {
        // hidpp sorts after BAT1 here, but the scope decides, not the order.
        let fx = laptop("Discharging");
        mouse(&fx);
        fx.write("sys/class/power_supply/BAT1/capacity", "42\n");

        assert_eq!(summary(&fx.host()).0, "42%");
    }

    #[test]
    fn a_charge_only_battery_uses_milliamp_hours() {
        let fx = Fixture::new();
        let bat = "sys/class/power_supply/BAT0";
        fx.write(&format!("{bat}/type"), "Battery\n");
        fx.write(&format!("{bat}/status"), "Discharging\n");
        fx.write(&format!("{bat}/charge_now"), "2000000\n");
        fx.write(&format!("{bat}/charge_full"), "4000000\n");
        fx.write(&format!("{bat}/current_now"), "-1000000\n");
        let text = text(&rows(&fx.host()));

        assert!(text.contains("2000 mAh / 4000 mAh"), "{text}");
        assert!(text.contains("2h 0m 0s"), "{text}");
    }

    #[test]
    fn an_idle_battery_has_no_time_estimate() {
        let reading = Reading {
            unit: Unit::Energy,
            now: 10,
            full: 20,
            design: None,
            rate: Some(0),
        };

        assert_eq!(reading.time_left(false), None);
        assert_eq!(
            Reading {
                rate: None,
                ..reading
            }
            .time_left(true),
            None
        );
    }

    #[test]
    fn a_desktop_says_it_has_no_power_supply() {
        assert_eq!(
            rows(&Fixture::new().host()),
            [Row::note("no power supply information (desktop?)")]
        );
        assert_eq!(
            summary(&Fixture::new().host()),
            ("no battery".to_string(), "-".to_string())
        );
    }

    #[test]
    fn a_zero_cycle_count_is_hidden() {
        assert_eq!(nonzero_or_dash(Some("0".to_string())), "-");
        assert_eq!(nonzero_or_dash(Some("12".to_string())), "12");
        assert_eq!(nonzero_or_dash(None), "-");
    }
}
