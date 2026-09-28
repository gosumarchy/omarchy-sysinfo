//! The condensed first page: the handful of numbers you check first.

use super::cpu::CpuInfo;
use super::stats::Stats;
use super::units::{fraction, human_bytes, human_secs};
use super::{Host, Row, dmi, omarchy, power, sensors, system};

pub(super) fn rows(
    host: &Host,
    stats: &Stats,
    cpu: &CpuInfo,
    omarchy_fixed: &omarchy::Fixed,
) -> Vec<Row> {
    let (theme, channel) = omarchy::summary(host, omarchy_fixed);
    let (battery, battery_state) = power::summary(host);
    let memory = stats.memory();

    let mut rows = vec![
        Row::header("Identity"),
        Row::field("Host", system::hostname(host)),
        Row::field("Distro", system::distro(host)),
        Row::field("Kernel", system::kernel(host)),
        Row::field("Chassis", dmi::chassis(host)),
        Row::field("Board", board(host)),
        Row::field(
            "Omarchy",
            format!("{} · {channel}", omarchy_fixed.version()),
        ),
        Row::field("Theme", theme),
        Row::header("Live"),
    ];

    let threads = stats.cores().len();
    if threads > 0 {
        let load = stats.load();
        if stats.primed() {
            rows.push(Row::field_with(
                "CPU",
                format!(
                    "{} · {threads} thread(s) · load {:.2} {:.2} {:.2}",
                    cpu.brand(),
                    load.one,
                    load.five,
                    load.fifteen
                ),
                stats.cpu_usage() / 100.0,
            ));
        } else {
            rows.push(Row::field(
                "CPU",
                format!("{} · {threads} thread(s) · sampling", cpu.brand()),
            ));
        }
    }
    rows.push(Row::field_with(
        "Memory",
        format!(
            "{} / {}",
            human_bytes(memory.used()),
            human_bytes(memory.total)
        ),
        fraction(memory.used(), memory.total),
    ));
    if memory.swap_total > 0 {
        rows.push(Row::field_with(
            "Swap",
            format!(
                "{} / {}",
                human_bytes(memory.swap_used()),
                human_bytes(memory.swap_total)
            ),
            fraction(memory.swap_used(), memory.swap_total),
        ));
    }
    if let Some(peak) = sensors::peak_temperature(host) {
        // No bar here: the fraction would be peak/100, the same number the
        // value already shows, so the plain rendering read "62 °C  62%". The
        // per-sensor gauges in the Sensors section carry that detail.
        rows.push(Row::field("Peak temperature", format!("{peak:.0} °C")));
    }
    rows.push(Row::field("Uptime", human_secs(stats.uptime())));
    rows.push(Row::field(
        "Battery",
        format!("{battery} · {battery_state}"),
    ));

    rows
}

fn board(host: &Host) -> String {
    host.read("/sys/class/dmi/id/board_name")
        .or_else(|| host.read("/sys/class/dmi/id/product_name"))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    #[test]
    fn the_overview_of_a_bare_machine_has_placeholders_not_panics() {
        let fx = Fixture::new();
        let host = fx.host();
        let rows = rows(
            &host,
            &Stats::new(&host),
            &CpuInfo::read(&host),
            &omarchy::Fixed::read(&host),
        );

        assert!(rows.contains(&Row::field("Board", "unknown")));
        assert!(rows.contains(&Row::field("Battery", "no battery · -")));
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, Row::Field { label, .. } if label == "Swap")),
            "no swap, no swap row"
        );
    }

    #[test]
    fn the_board_falls_back_to_the_product_name() {
        let fx = Fixture::new();
        fx.write("sys/class/dmi/id/product_name", "ThinkPad X1\n");

        assert_eq!(board(&fx.host()), "ThinkPad X1");
    }
}
