//! Graphics devices and the display connectors they drive, from
//! `/sys/class/drm`.

use std::path::{Path, PathBuf};

use super::fs::{driver_name, file_name, list_dir, read, read_u64};
use super::pci::{Ids, pci_slot_of};
use super::units::{dash, fraction, human_bytes};
use super::{Host, Row};

/// How many modes a connector row lists before summarising the rest. A 4K
/// panel advertises dozens, which pushed the row far past any terminal.
const MODES_SHOWN: usize = 3;

/// `IORESOURCE_MEM` in a `resource` line's flags: the BAR maps memory, not
/// I/O ports.
const IORESOURCE_MEM: u64 = 0x200;

pub(crate) fn rows(host: &Host, ids: &Ids) -> Vec<Row> {
    let drm = host.path("/sys/class/drm");
    let entries = list_dir(&drm);
    let mut rows = Vec::new();

    for card in entries.iter().filter(|p| is_card(&file_name(p))) {
        rows.extend(card_rows(card, ids));
    }

    let connectors: Vec<&Path> = entries
        .iter()
        .filter(|p| is_connector(&file_name(p)))
        .map(PathBuf::as_path)
        .collect();
    if !connectors.is_empty() {
        rows.push(Row::header("Connectors"));
        rows.extend(connectors.into_iter().map(connector_row));
    }

    if rows.is_empty() {
        rows.push(Row::note("no drm devices"));
    }

    rows
}

/// `card0`, not `card0-eDP-1` and not `renderD128`.
fn is_card(name: &str) -> bool {
    name.strip_prefix("card")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// `card0-eDP-1`.
fn is_connector(name: &str) -> bool {
    name.strip_prefix("card")
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn card_rows(card: &Path, ids: &Ids) -> Vec<Row> {
    let device = card.join("device");
    let slot = pci_slot_of(&device);
    let vendor = read(device.join("vendor"));
    let device_id = read(device.join("device"));
    let name = match (&vendor, &device_id) {
        (Some(v), Some(d)) => ids.describe(v, d, "unknown graphics device"),
        (Some(v), None) => ids
            .vendor_name(v)
            .unwrap_or_else(|| "unknown graphics device".into()),
        (None, _) => "unknown graphics device".into(),
    };

    let mut rows = vec![
        Row::header(format!(
            "{} · {}",
            file_name(card),
            slot.as_deref().unwrap_or("no pci slot")
        )),
        Row::field("Device", name),
        Row::field("Driver", dash(driver_name(device.join("driver")))),
    ];
    if let Some(vendor) = vendor {
        rows.push(Row::field("Vendor ID", vendor));
    }
    if let Some(device_id) = device_id {
        rows.push(Row::field("Device ID", device_id));
    }
    if let Some(boot) = read(device.join("boot_vga")) {
        rows.push(Row::field(
            "Primary GPU",
            if boot == "1" { "yes" } else { "no" },
        ));
    }
    rows.extend(vram_rows(&device));
    if let Some(link) = pcie_link(&device) {
        rows.push(Row::field("PCIe link", link));
    }

    rows
}

fn connector_row(path: &Path) -> Row {
    let status = read(path.join("status")).unwrap_or_else(|| "unknown".into());
    let enabled = read(path.join("enabled")).unwrap_or_else(|| "-".into());
    let modes: Vec<String> = read(path.join("modes"))
        .map(|m| m.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();

    Row::field(
        format!("{}  {status}", file_name(path)),
        format!("modes: {}   {enabled}", summarise_modes(&modes)),
    )
}

/// `2880x1800, 1920x1200, 1920x1080 (+14 more)`.
fn summarise_modes(modes: &[String]) -> String {
    if modes.is_empty() {
        return "-".into();
    }

    // The kernel repeats a resolution once per refresh rate.
    let mut unique: Vec<&str> = Vec::new();
    for mode in modes {
        if !unique.contains(&mode.as_str()) {
            unique.push(mode);
        }
    }

    let shown = unique
        .iter()
        .take(MODES_SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    match unique.len().checked_sub(MODES_SHOWN) {
        Some(rest) if rest > 0 => format!("{shown} (+{rest} more)"),
        _ => shown,
    }
}

fn vram_rows(device: &Path) -> Vec<Row> {
    let mut rows = Vec::new();

    // amdgpu reports real VRAM usage; nothing else does.
    if let Some(total) = read_u64(device.join("mem_info_vram_total")) {
        let used = read_u64(device.join("mem_info_vram_used")).unwrap_or(0);
        rows.push(Row::field_with(
            "VRAM",
            format!("{} / {}", human_bytes(used), human_bytes(total)),
            fraction(used, total),
        ));
        if let Some(visible) = read_u64(device.join("mem_info_vis_vram_total")) {
            rows.push(Row::field("CPU-visible VRAM", human_bytes(visible)));
        }
    } else if let Some(bar) = read(device.join("resource")).and_then(|r| largest_memory_bar(&r)) {
        // Every other driver: the largest memory BAR is the window the CPU
        // gets onto the card. With Resizable BAR it is all of VRAM; without,
        // typically 256 MiB. It is not the VRAM size, so it is not called
        // that. (The old code read `resource0`, a binary mmap-only file that
        // cannot be read as text, so this row never appeared.)
        rows.push(Row::field("Largest PCI BAR", human_bytes(bar)));
    }

    rows
}

/// The size of the largest memory BAR in a sysfs `resource` file, whose lines
/// are `start end flags` in hex, one per BAR.
fn largest_memory_bar(resource: &str) -> Option<u64> {
    resource
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(parse_hex);
            let (Some(Some(start)), Some(Some(end)), Some(Some(flags))) =
                (fields.next(), fields.next(), fields.next())
            else {
                return None;
            };
            let is_memory = flags & IORESOURCE_MEM != 0;

            (is_memory && start != 0 && end > start).then(|| end - start + 1)
        })
        .max()
}

fn parse_hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}

/// `Gen 4 x16 (max Gen 4 x16)`, or `None` before a link is negotiated.
fn pcie_link(device: &Path) -> Option<String> {
    let speed = read(device.join("current_link_speed"))?;
    let width = read(device.join("current_link_width"))?;
    let current = describe_link(&speed, &width)?;

    let max = read(device.join("max_link_speed"))
        .zip(read(device.join("max_link_width")))
        .and_then(|(speed, width)| describe_link(&speed, &width));

    Some(match max {
        Some(max) if max != current => format!("{current}  (max {max})"),
        Some(_) | None => current,
    })
}

/// A link from its sysfs speed and width, `8.0 GT/s PCIe` and `4`.
///
/// The speed file is a transfer rate with a unit, not a number, which is why
/// the old integer check rejected every real link and the row never showed.
/// Integrated GPUs report `Unknown` until a link is negotiated.
fn describe_link(speed: &str, width: &str) -> Option<String> {
    let width: u32 = width.trim().parse().ok().filter(|w| *w > 0)?;
    let rate = speed.split_whitespace().next()?;
    let generation = match rate {
        "2.5" => 1,
        "5.0" | "5" => 2,
        "8.0" | "8" => 3,
        "16.0" | "16" => 4,
        "32.0" | "32" => 5,
        "64.0" | "64" => 6,
        // An unknown rate (or "Unknown") is shown as the kernel printed it.
        _ => {
            return rate
                .parse::<f64>()
                .ok()
                .map(|_| format!("{rate} GT/s x{width}"));
        }
    };

    Some(format!("Gen {generation} x{width}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    fn text(rows: &[Row]) -> String {
        format!("{rows:?}")
    }

    // ---- names -----------------------------------------------------------

    #[test]
    fn cards_and_connectors_are_told_apart() {
        assert!(is_card("card0"));
        assert!(is_card("card12"));
        assert!(!is_card("card0-eDP-1"));
        assert!(!is_card("renderD128"));
        assert!(!is_card("card"));

        assert!(is_connector("card0-eDP-1"));
        assert!(is_connector("card1-DP-3"));
        assert!(!is_connector("card0"));
        assert!(!is_connector("cardx-DP-1"));
    }

    // ---- PCIe link ---------------------------------------------------------

    #[test]
    fn a_negotiated_link_is_named_by_generation() {
        // The real sysfs format. The old parser wanted a bare integer and
        // so never showed this row on any machine.
        assert_eq!(
            describe_link("16.0 GT/s PCIe", "16").as_deref(),
            Some("Gen 4 x16")
        );
        assert_eq!(
            describe_link("8.0 GT/s PCIe", "4").as_deref(),
            Some("Gen 3 x4")
        );
        assert_eq!(
            describe_link("2.5 GT/s PCIe", "1").as_deref(),
            Some("Gen 1 x1")
        );
    }

    #[test]
    fn an_unnegotiated_link_is_absent() {
        assert_eq!(describe_link("Unknown", "4"), None);
        assert_eq!(describe_link("8.0 GT/s PCIe", "0"), None);
        assert_eq!(describe_link("8.0 GT/s PCIe", "x4"), None);
        assert_eq!(describe_link("", ""), None);
    }

    #[test]
    fn an_unfamiliar_rate_is_shown_as_is() {
        assert_eq!(
            describe_link("128.0 GT/s PCIe", "16").as_deref(),
            Some("128.0 GT/s x16")
        );
    }

    #[test]
    fn a_downtrained_link_shows_its_maximum() {
        let fx = Fixture::new();
        fx.write("dev/current_link_speed", "8.0 GT/s PCIe\n");
        fx.write("dev/current_link_width", "8\n");
        fx.write("dev/max_link_speed", "16.0 GT/s PCIe\n");
        fx.write("dev/max_link_width", "16\n");

        assert_eq!(
            pcie_link(&fx.dir().join("dev")).as_deref(),
            Some("Gen 3 x8  (max Gen 4 x16)")
        );
    }

    #[test]
    fn a_link_at_its_maximum_does_not_repeat_itself() {
        let fx = Fixture::new();
        for file in ["current_link_speed", "max_link_speed"] {
            fx.write(&format!("dev/{file}"), "16.0 GT/s PCIe\n");
        }
        for file in ["current_link_width", "max_link_width"] {
            fx.write(&format!("dev/{file}"), "16\n");
        }

        assert_eq!(
            pcie_link(&fx.dir().join("dev")).as_deref(),
            Some("Gen 4 x16")
        );
    }

    // ---- BARs --------------------------------------------------------------

    #[test]
    fn the_largest_memory_bar_is_found_and_io_bars_are_ignored() {
        let resource = "\
0x00000000fb000000 0x00000000fbffffff 0x0000000000040200
0x0000006000000000 0x00000063ffffffff 0x000000000014220c
0x0000000000000000 0x0000000000000000 0x0000000000000000
0x000000000000e000 0x000000000000e07f 0x0000000000040101
";
        // BAR 2 is 16 GiB (Resizable BAR on a 16 GiB card).
        assert_eq!(largest_memory_bar(resource), Some(16 << 30));
    }

    #[test]
    fn a_resource_file_with_no_memory_bars_is_none() {
        assert_eq!(largest_memory_bar(""), None);
        assert_eq!(
            largest_memory_bar("0x000000000000e000 0x000000000000e07f 0x0000000000040101\n"),
            None
        );
    }

    // ---- modes -------------------------------------------------------------

    #[test]
    fn modes_are_deduplicated_and_summarised() {
        let modes: Vec<String> = [
            "2880x1800",
            "2880x1800",
            "1920x1200",
            "1920x1080",
            "1280x800",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();

        assert_eq!(
            summarise_modes(&modes),
            "2880x1800, 1920x1200, 1920x1080 (+1 more)"
        );
        assert_eq!(summarise_modes(&[]), "-");
        assert_eq!(summarise_modes(&modes[..1]), "2880x1800");
    }

    // ---- rows against a fixture --------------------------------------------

    fn discrete_gpu() -> Fixture {
        let fx = Fixture::new();
        let dev = "sys/devices/pci0000:00/0000:00:01.0/0000:01:00.0";
        fx.write(&format!("{dev}/vendor"), "0x1002\n");
        fx.write(&format!("{dev}/device"), "0x744c\n");
        fx.write(&format!("{dev}/boot_vga"), "1\n");
        fx.write(&format!("{dev}/mem_info_vram_total"), "17163091968\n");
        fx.write(&format!("{dev}/mem_info_vram_used"), "4290772992\n");
        fx.write(&format!("{dev}/current_link_speed"), "16.0 GT/s PCIe\n");
        fx.write(&format!("{dev}/current_link_width"), "16\n");
        fx.symlink(
            &format!("{dev}/driver"),
            "../../../../bus/pci/drivers/amdgpu",
        );
        fx.symlink(
            "sys/class/drm/card1/device",
            "../../../devices/pci0000:00/0000:00:01.0/0000:01:00.0",
        );
        fx.write("sys/class/drm/card1-DP-1/status", "connected\n");
        fx.write("sys/class/drm/card1-DP-1/enabled", "enabled\n");
        fx.write("sys/class/drm/card1-DP-1/modes", "3840x2160\n2560x1440\n");
        fx
    }

    #[test]
    fn a_discrete_gpu_is_described_from_sysfs() {
        let fx = discrete_gpu();
        let rows = rows(&fx.host(), &Ids::default());
        let text = text(&rows);

        assert!(text.contains("card1 · 0000:01:00.0"), "{text}");
        assert!(text.contains("amdgpu"), "{text}");
        assert!(text.contains("Gen 4 x16"), "{text}");
        assert!(text.contains("4.0 GiB / 16.0 GiB"), "{text}");
        assert!(text.contains("\"yes\""), "{text}");
        assert!(text.contains("3840x2160, 2560x1440"), "{text}");
    }

    #[test]
    fn vram_used_carries_a_gauge() {
        let fx = discrete_gpu();
        let rows = rows(&fx.host(), &Ids::default());

        assert!(rows.iter().any(|r| matches!(
            r,
            Row::Field { label, bar: Some(_), .. } if label == "VRAM"
        )));
    }

    #[test]
    fn a_machine_without_drm_says_so() {
        assert_eq!(
            rows(&Fixture::new().host(), &Ids::default()),
            [Row::note("no drm devices")]
        );
    }
}
