use super::{fs::read, units::dash, Row};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// Resolve `0x8086:0x9a49` into `Intel Corporation Meteor Lake-P [Intel Arc Graphics]`
/// using the system `pci.ids` database when one is installed.
pub fn device_name(vendor: &str, device: &str) -> Option<String> {
    let vendor_id = parse_hex_id(vendor)?;
    let device_id = parse_hex_id(device)?;
    let db = ids();
    let device_name = db.devices.get(&(vendor_id, device_id))?;
    let vendor_name = db
        .vendors
        .get(&vendor_id)
        .map(String::as_str)
        .unwrap_or("Unknown vendor");
    Some(format!("{vendor_name} {device_name}"))
}

pub fn vendor_name(vendor: &str) -> Option<String> {
    let id = parse_hex_id(vendor)?;
    ids().vendors.get(&id).cloned()
}

/// A PCI id as sysfs writes it, `0x8086`, with either case of the prefix.
fn parse_hex_id(s: &str) -> Option<u16> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u16::from_str_radix(s, 16).ok()
}

struct Ids {
    vendors: HashMap<u16, String>,
    devices: HashMap<(u16, u16), String>,
}

fn ids() -> &'static Ids {
    static IDS: OnceLock<Ids> = OnceLock::new();
    IDS.get_or_init(|| {
        for path in ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"] {
            if !Path::new(path).exists() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let parsed = parse_ids(&text);
            if !parsed.vendors.is_empty() {
                return parsed;
            }
        }
        Ids {
            vendors: HashMap::new(),
            devices: HashMap::new(),
        }
    })
}

/// Parse a `pci.ids` database into vendors and devices.
///
/// The file has three parts: vendors, their indented devices, and the device
/// classes. A class heading such as `C 0c  Serial bus controller` is *not*
/// indented and its first token, `C`, is a valid hex digit -- and it parses to
/// 0x0c, not to the class number in the second token. The old parser therefore
/// accepted every class heading as vendor 0x0c, letting the last one win, and
/// filed the ~20 indented class names as devices of that bogus vendor. Only a
/// four hex digit token can start a vendor line.
fn parse_ids(text: &str) -> Ids {
    let mut vendors = HashMap::new();
    let mut devices = HashMap::new();
    let mut current_vendor = None;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with('\t');
        if !indented {
            let name = line.trim();
            let Some(id) = name
                .split_whitespace()
                .next()
                .filter(|tok| tok.len() == 4)
                .and_then(|v| u16::from_str_radix(v, 16).ok())
            else {
                // A `C xx` class heading, or anything else that is not a
                // vendor. It must not become the current vendor either, or
                // the indented class names below it would be filed as
                // devices of whatever vendor happened to be last.
                current_vendor = None;
                continue;
            };
            let clean = name
                .split_once(char::is_whitespace)
                .map(|(_, rest)| rest)
                .unwrap_or("")
                .trim()
                .to_string();
            if clean.is_empty() {
                // A bare id carries no information, and storing an empty name
                // would render as " Meteor Lake-P" in the device row.
                continue;
            }
            // First line wins, matching the device map below.
            vendors.entry(id).or_insert(clean);
            current_vendor = Some(id);
        } else if let Some(vendor) = current_vendor {
            // Device lines are `\t<id>  <name>`; the name can contain
            // double spaces, so rejoin everything after the id.
            let mut tokens = line.split_whitespace();
            let Some(dev) = tokens.next() else {
                continue;
            };
            if let Ok(dev_id) = u16::from_str_radix(dev, 16) {
                let rest = line
                    .trim()
                    .split_once(char::is_whitespace)
                    .map(|(_, rest)| rest)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !rest.is_empty() {
                    devices.entry((vendor, dev_id)).or_insert(rest);
                }
            }
        }
    }
    Ids { vendors, devices }
}

pub fn rows() -> Vec<Row> {
    let devices = super::fs::list_dir("/sys/bus/pci/devices");
    if devices.is_empty() {
        return vec![Row::note("no pci bus")];
    }

    let mut rows = Vec::new();
    let mut entries: Vec<(String, String, String, String, String, String)> = Vec::new();

    for dev in devices {
        let slot = dev
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let class_id = dash(read(dev.join("class")));
        let vendor = dash(read(dev.join("vendor")));
        let device = dash(read(dev.join("device")));
        let name = device_name(&vendor, &device)
            .or_else(|| vendor_name(&vendor))
            .unwrap_or_else(|| "unknown device".to_string());
        let driver = super::fs::driver_name(dev.join("driver")).unwrap_or_else(|| "unbound".into());
        entries.push((slot, class_id, vendor, device, name, driver));
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (slot, class_id, vendor, device, name, driver) in entries {
        rows.push(Row::field(
            format!("{slot}  {driver}"),
            format!("{name}  [{class_id} {vendor}:{device}]"),
        ));
    }
    rows
}

/// Map a `/sys/class/drm/cardN` path back to its PCI slot, e.g. `0000:00:02.0`.
///
/// The domain is not assumed to be zero: a system can put a device behind
/// another domain, and a hardcoded `0000:` prefix missed it.
pub fn pci_slot_of(path: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(path).ok()?;
    resolved
        .components()
        .filter_map(|c| {
            let s = c.as_os_str().to_string_lossy().to_string();
            looks_like_slot(&s).then_some(s)
        })
        .next()
}

/// `0000:00:02.0`: four hex digits, then bus and device, then a function.
fn looks_like_slot(s: &str) -> bool {
    let mut parts = s.split(':');
    let (Some(domain), Some(bus), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let Some((dev, func)) = rest.split_once('.') else {
        return false;
    };
    [domain, bus, dev, func]
        .iter()
        .all(|p| p.len() <= 4 && !p.is_empty() && p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PCI_IDS: &str = "\
#
#	List of PCI ID's
#
1002  Advanced Micro Devices, Inc. [AMD/ATI]
\t1000  Ati Raster Operations Raster Accel
\t164e  Raphael GPU
15ad  VMware, Inc.
\t07b0  SVGA II Adapter
8086  Intel Corporation
\t9a49  Meteor Lake-P [Intel Arc Graphics]
\t9a59  Lunar Lake-P [Intel Arc Graphics]

C 00  Unclassified device
\t00  Non-VGA unclassified device
C 15  VGA compatible controller
\t00  VGA controller
C 03  Display controller
\t00  VGA compatible controller

8086  Duplicate Vendor Line
\t0000  Should Not Overwrite
";

    // ---- parse_hex_id ----------------------------------------------------

    #[test]
    fn parse_hex_id_reads_the_sysfs_form() {
        assert_eq!(parse_hex_id("0x8086"), Some(0x8086));
        assert_eq!(parse_hex_id("0X8086"), Some(0x8086), "uppercase prefix");
        assert_eq!(parse_hex_id("8086"), Some(0x8086), "no prefix");
        assert_eq!(parse_hex_id("  0x00  "), Some(0));
    }

    #[test]
    fn parse_hex_id_rejects_junk() {
        assert_eq!(parse_hex_id(""), None);
        assert_eq!(parse_hex_id("0x"), None);
        assert_eq!(parse_hex_id("zzzz"), None);
        assert_eq!(parse_hex_id("0x12345"), None, "too wide for a u16");
    }

    // ---- parse_ids -------------------------------------------------------

    #[test]
    fn parse_ids_reads_vendors_and_their_devices() {
        let ids = parse_ids(PCI_IDS);
        assert_eq!(
            ids.vendors.get(&0x8086).map(String::as_str),
            Some("Intel Corporation")
        );
        assert_eq!(
            ids.devices.get(&(0x8086, 0x9a49)).map(String::as_str),
            Some("Meteor Lake-P [Intel Arc Graphics]")
        );
        assert_eq!(
            ids.devices.get(&(0x1002, 0x164e)).map(String::as_str),
            Some("Raphael GPU")
        );
    }

    #[test]
    fn parse_ids_keeps_double_spaces_inside_a_device_name() {
        let ids = parse_ids(PCI_IDS);
        let name = ids.devices.get(&(0x8086, 0x9a59)).expect("device");
        assert!(name.contains("[Intel Arc Graphics]"), "{name:?}");
        assert!(!name.starts_with(' '), "{name:?}");
    }

    #[test]
    fn parse_ids_does_not_turn_class_headings_into_vendors() {
        // "C" is a hex digit and parses to 0x0c, not to the class number, so
        // the old parser collapsed every class heading onto vendor 0x0c and
        // let the last one ("ff  Unassigned class") win.
        let ids = parse_ids(PCI_IDS);
        assert!(
            !ids.vendors.contains_key(&0x0c),
            "a class heading must not become a vendor: {:?}",
            ids.vendors
        );
        assert_eq!(ids.vendors.len(), 3, "{:?}", ids.vendors);
        assert!(
            !ids.vendors.values().any(|v| v.contains("controller")),
            "{:?}",
            ids.vendors
        );
        // A real vendor must survive, whatever its id.
        assert_eq!(
            ids.vendors.get(&0x15ad).map(String::as_str),
            Some("VMware, Inc.")
        );
    }

    #[test]
    fn parse_ids_does_not_file_class_names_as_devices() {
        // The old parser attributed the ~20 indented class names to vendor
        // 0x0c, so a lookup for a device of that vendor returned a class name.
        let ids = parse_ids(PCI_IDS);
        assert!(
            !ids.devices.values().any(|v| v.contains("controller")),
            "{:?}",
            ids.devices
        );
        // 2 AMD + 1 VMware + 2 Intel, plus the duplicate line's device.
        assert_eq!(ids.devices.len(), 6, "{:?}", ids.devices);
    }

    #[test]
    fn parse_ids_lets_the_first_vendor_line_win() {
        // pci.ids has no duplicates in practice; the first one is kept.
        let ids = parse_ids(PCI_IDS);
        assert_eq!(
            ids.vendors.get(&0x8086).map(String::as_str),
            Some("Intel Corporation")
        );
    }

    #[test]
    fn parse_ids_skips_comments_and_blank_lines() {
        let ids = parse_ids("# only a comment\n\n   \n");
        assert!(ids.vendors.is_empty());
        assert!(ids.devices.is_empty());
    }

    #[test]
    fn parse_ids_ignores_a_vendor_line_with_no_name() {
        let ids = parse_ids("abcd\n");
        assert!(ids.vendors.is_empty(), "{:?}", ids.vendors);
    }

    #[test]
    fn parse_ids_of_nothing_is_empty() {
        let ids = parse_ids("");
        assert!(ids.vendors.is_empty() && ids.devices.is_empty());
    }

    #[test]
    fn parse_ids_ignores_a_device_line_with_no_current_vendor() {
        let ids = parse_ids("\t1234  Orphan device\n");
        assert!(ids.devices.is_empty());
    }

    // ---- device_name / vendor_name ---------------------------------------

    #[test]
    fn device_name_resolves_a_known_pair() {
        // Uses the real system database when one is installed.
        match device_name("0x8086", "0x9a49") {
            Some(name) => assert!(!name.is_empty()),
            None => assert!(ids().vendors.is_empty(), "no pci.ids on this machine"),
        }
    }

    #[test]
    fn device_name_is_none_for_an_unknown_or_malformed_id() {
        assert_eq!(device_name("nope", "0x0000"), None);
        assert_eq!(device_name("0x8086", "nope"), None);
    }

    // ---- looks_like_slot -------------------------------------------------

    #[test]
    fn looks_like_slot_accepts_a_real_slot() {
        assert!(looks_like_slot("0000:00:02.0"));
        assert!(looks_like_slot("0000:4d:00.1"));
        assert!(looks_like_slot("0001:00:1f.7"));
        assert!(
            !looks_like_slot("10000:00:00.0"),
            "domain is at most 4 hex digits"
        );
    }

    #[test]
    fn looks_like_slot_rejects_other_path_components() {
        assert!(!looks_like_slot("pci0000:00"));
        assert!(!looks_like_slot("0000:00:02"), "no function");
        assert!(!looks_like_slot("0000:00:02.0.1"), "too many parts");
        assert!(!looks_like_slot("0000:00:02.x"), "function is not hex");
        assert!(!looks_like_slot(""));
        assert!(!looks_like_slot("drm"));
    }

    // ---- live ------------------------------------------------------------

    #[test]
    fn rows_render_without_panicking() {
        let rows = rows();
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "{text}");
        assert!(!text.contains("null"), "{text}");
    }

    #[test]
    fn pci_slot_of_a_real_drm_card_looks_like_a_slot() {
        let drm = Path::new("/sys/class/drm");
        if !drm.exists() {
            return;
        }
        for entry in super::super::fs::list_dir(drm) {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("card") {
                continue;
            }
            if let Some(slot) = pci_slot_of(&entry) {
                assert!(looks_like_slot(&slot), "{slot:?}");
            }
        }
    }
}
