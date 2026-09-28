use super::{
    Row,
    fs::{read, read_u64},
    units::{dash, human_bytes},
};
use std::path::Path;

/// Every DRM connector tells us which display outputs the GPU drives, and the
/// GPU itself comes from the `/sys/class/drm/cardN/device` symlink.
pub(crate) fn rows() -> Vec<Row> {
    let drm = Path::new("/sys/class/drm");
    let mut rows = Vec::new();

    for card in super::fs::list_dir(drm) {
        let name = card
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }

        let device = card.join("device");
        let slot = super::pci::pci_slot_of(&device);
        let vendor = read(device.join("vendor"));
        let dev_id = read(device.join("device"));
        let name_text = vendor
            .as_deref()
            .zip(dev_id.as_deref())
            .and_then(|(v, d)| super::pci::device_name(v, d))
            .or_else(|| vendor.as_deref().and_then(super::pci::vendor_name))
            .unwrap_or_else(|| "unknown graphics device".to_string());
        let driver = super::fs::driver_name(device.join("driver"));

        rows.push(Row::Header(format!(
            "{} · {}",
            name,
            slot.clone().unwrap_or_else(|| "no pci slot".into())
        )));
        rows.push(Row::field("Device", name_text));
        rows.push(Row::field("Driver", dash(driver.clone())));
        if let Some(vendor) = &vendor {
            rows.push(Row::field("Vendor ID", vendor.clone()));
        }
        if let Some(d) = &dev_id {
            rows.push(Row::field("Device ID", d.clone()));
        }
        if let Some(boot) = read(device.join("boot_vga")) {
            rows.push(Row::field(
                "Primary GPU",
                if boot == "1" { "yes" } else { "no" },
            ));
        }
        for (label, value) in vram(&device) {
            rows.push(Row::field(label, value));
        }
        if let Some(link) = pcie_link(&device) {
            rows.push(Row::field("PCIe link", link));
        }
    }

    let connectors: Vec<String> = super::fs::list_dir(drm)
        .iter()
        .filter_map(|p| {
            let n = p.file_name()?.to_string_lossy().to_string();
            (n.starts_with("card") && n.contains('-')).then_some(n)
        })
        .collect();
    if !connectors.is_empty() {
        rows.push(Row::Header("Connectors".into()));
        for c in connectors {
            let path = drm.join(&c);
            let status = read(path.join("status")).unwrap_or_else(|| "unknown".into());
            let enabled = read(path.join("enabled")).unwrap_or_else(|| "-".into());
            let modes = read(path.join("modes"))
                .map(|m| m.split_whitespace().collect::<Vec<_>>().join(", "))
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "-".into());
            rows.push(Row::field(
                format!("{c}  {status}"),
                format!("modes: {modes}   enabled: {enabled}"),
            ));
        }
    }

    if rows.is_empty() {
        rows.push(Row::note("no drm devices"));
    }
    rows
}

fn vram(device: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(total) = read_u64(device.join("mem_info_vram_total")) {
        let used = read_u64(device.join("mem_info_vram_used")).unwrap_or(0);
        out.push((
            "VRAM".to_string(),
            format!("{} / {}", human_bytes(used), human_bytes(total)),
        ));
        out.push((
            "VRAM used".to_string(),
            format!(
                "{:.0}%",
                if total == 0 {
                    0.0
                } else {
                    used as f64 / total as f64 * 100.0
                }
            ),
        ));
    } else if let Some(total) = read_u64(device.join("resource0")) {
        out.push(("VRAM aperture".to_string(), human_bytes(total)));
    }
    if let Some(vis) = read_u64(device.join("mem_info_vis_vram_total")) {
        out.push(("Visible VRAM".to_string(), human_bytes(vis)));
    }
    out
}

fn pcie_link(device: &Path) -> Option<String> {
    let width = read(device.join("current_link_width"))?;
    let speed = read(device.join("current_link_speed"))?;
    // Integrated GPUs report "Unknown" for both until a link is negotiated.
    // Only the width was checked, so a card that knew its width printed
    // "Gen Unknown x4".
    if !is_link_number(&width) || !is_link_number(&speed) {
        return None;
    }
    let max_w = read(device.join("max_link_width")).unwrap_or_default();
    let max_s = read(device.join("max_link_speed")).unwrap_or_default();
    Some(format!("Gen {speed} x{width}  (max Gen {max_s} x{max_w})"))
}

/// A link width or speed is a plain number; anything else is unnegotiated.
fn is_link_number(s: &str) -> bool {
    !s.is_empty() && s != "0" && s.chars().all(|c| c.is_ascii_digit())
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
    fn rows_report_devices_or_an_explicit_note() {
        let rows = rows();
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(
            text.contains("no drm devices") || text.contains("card"),
            "{text}"
        );
    }

    #[test]
    fn is_link_number_accepts_a_negotiated_link() {
        assert!(is_link_number("4"));
        assert!(is_link_number("16"));
    }

    #[test]
    fn is_link_number_rejects_an_unnegotiated_link() {
        // The bug: only the width was checked, so "Gen Unknown x4" was shown.
        assert!(!is_link_number("Unknown"));
        assert!(!is_link_number("0"));
        assert!(!is_link_number(""));
        assert!(!is_link_number("x4"));
        assert!(!is_link_number("4x"));
    }

    #[test]
    fn vram_of_a_device_with_no_vram_files_is_empty() {
        assert!(vram(Path::new("/nonexistent-gpu")).is_empty());
    }

    #[test]
    fn pcie_link_of_a_missing_device_is_none() {
        assert!(pcie_link(Path::new("/nonexistent-gpu")).is_none());
    }
}
