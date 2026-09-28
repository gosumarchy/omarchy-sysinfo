//! The PCI bus, and the `pci.ids` database that names what is on it.

use std::collections::HashMap;
use std::path::Path;

use super::fs::{driver_name, file_name, read};
use super::units::dash;
use super::{Host, Row};

/// Vendor and device names from the system `pci.ids`.
///
/// Loaded once per process: the file is over a megabyte and never changes
/// while we run.
#[derive(Debug, Default)]
pub(crate) struct Ids {
    vendors: HashMap<u16, String>,
    devices: HashMap<(u16, u16), String>,
}

impl Ids {
    pub(crate) fn load(host: &Host) -> Ids {
        ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"]
            .into_iter()
            .filter_map(|path| std::fs::read_to_string(host.path(path)).ok())
            .map(|text| parse_ids(&text))
            .find(|ids| !ids.vendors.is_empty())
            .unwrap_or_default()
    }

    /// Resolve `0x8086:0x9a49` into `Intel Corporation Meteor Lake-P [Intel Arc
    /// Graphics]`.
    pub(crate) fn device_name(&self, vendor: &str, device: &str) -> Option<String> {
        let vendor_id = parse_hex_id(vendor)?;
        let device_id = parse_hex_id(device)?;
        let device_name = self.devices.get(&(vendor_id, device_id))?;
        let vendor_name = self
            .vendors
            .get(&vendor_id)
            .map_or("Unknown vendor", String::as_str);

        Some(format!("{vendor_name} {device_name}"))
    }

    pub(crate) fn vendor_name(&self, vendor: &str) -> Option<String> {
        self.vendors.get(&parse_hex_id(vendor)?).cloned()
    }

    /// The best name available: the device, else its vendor, else `fallback`.
    pub(crate) fn describe(&self, vendor: &str, device: &str, fallback: &str) -> String {
        self.device_name(vendor, device)
            .or_else(|| self.vendor_name(vendor))
            .unwrap_or_else(|| fallback.to_string())
    }
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

/// Parse a `pci.ids` database into vendors and devices.
///
/// The file has four kinds of line:
///
/// - `8086  Intel Corporation`: a vendor, at the left margin.
/// - `\t9a49  Meteor Lake-P`: one of that vendor's devices, one tab in.
/// - `\t\t1028 0b1d  Latitude`: a subsystem of that device, two tabs in. Its
///   first token is the *subvendor* id, and reading it as a device id filed
///   Dell's and Lenovo's subsystem names as devices of whatever vendor was
///   current; the first-wins map then shadowed the real device of that id.
/// - `C 0c  Serial bus controller`: a class heading, whose first token `C` is
///   a valid hex digit. Only a four-hex-digit token can start a vendor line.
fn parse_ids(text: &str) -> Ids {
    let mut ids = Ids::default();
    let mut current_vendor = None;

    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() || line.starts_with("\t\t") {
            continue;
        }

        let (id, name) = split_id_line(line.trim());

        if line.starts_with('\t') {
            let (Some(vendor), Some(device)) = (current_vendor, id) else {
                continue;
            };
            if !name.is_empty() {
                ids.devices.entry((vendor, device)).or_insert(name);
            }
        } else {
            // A `C xx` class heading, or anything else that is not a vendor,
            // must not become the current vendor either, or the indented class
            // names below it would be filed as its devices.
            current_vendor = id;
            if let Some(vendor) = id
                && !name.is_empty()
            {
                // First line wins, matching the device map.
                ids.vendors.entry(vendor).or_insert(name);
            }
        }
    }

    ids
}

/// `9a49  Meteor Lake-P [Intel Arc Graphics]` into its id and its name. The
/// name can contain double spaces, so everything after the id is kept.
fn split_id_line(line: &str) -> (Option<u16>, String) {
    let (id, name) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let id = (id.len() == 4)
        .then(|| u16::from_str_radix(id, 16).ok())
        .flatten();

    (id, name.trim().to_string())
}

/// One function on the bus, as sysfs describes it.
#[derive(Clone, Debug, PartialEq)]
struct PciDevice {
    slot: String,
    class: String,
    vendor: String,
    device: String,
    driver: Option<String>,
}

impl PciDevice {
    fn read(path: &Path) -> PciDevice {
        PciDevice {
            slot: file_name(path),
            class: dash(read(path.join("class"))),
            vendor: dash(read(path.join("vendor"))),
            device: dash(read(path.join("device"))),
            driver: driver_name(path.join("driver")),
        }
    }
}

pub(crate) fn rows(host: &Host, ids: &Ids) -> Vec<Row> {
    // `list_dir` sorts, so the devices arrive in slot order.
    let devices: Vec<PciDevice> = host
        .list_dir("/sys/bus/pci/devices")
        .iter()
        .map(|path| PciDevice::read(path))
        .collect();
    if devices.is_empty() {
        return vec![Row::note("no pci bus")];
    }

    devices
        .into_iter()
        .map(|d| {
            let name = ids.describe(&d.vendor, &d.device, "unknown device");

            Row::field(
                format!("{}  {}", d.slot, d.driver.as_deref().unwrap_or("unbound")),
                format!("{name}  [{} {}:{}]", d.class, d.vendor, d.device),
            )
        })
        .collect()
}

/// Map a `/sys/class/drm/cardN/device` link to its PCI slot, e.g. `0000:03:00.0`.
///
/// The canonical path walks down from the root complex through every bridge,
/// `/sys/devices/pci0000:00/0000:00:01.0/0000:01:00.0/0000:02:00.0/0000:03:00.0`,
/// so the device's own slot is the *last* slot-shaped component. Taking the
/// first named the root port for every discrete GPU.
pub(crate) fn pci_slot_of(path: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(path).ok()?;

    resolved
        .components()
        .filter_map(|c| {
            let s = c.as_os_str().to_string_lossy();
            looks_like_slot(&s).then(|| s.into_owned())
        })
        .next_back()
}

/// `0000:00:02.0`: four hex digits, then bus and device, then a function.
fn looks_like_slot(s: &str) -> bool {
    let mut parts = s.split(':');
    let (Some(domain), Some(bus), Some(rest), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
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
    use crate::collect::fixture::Fixture;

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
\t1000  82542 Gigabit Ethernet Controller (Fiber)
\t\t1028 0002  PowerEdge Subsystem
\t1028  RealDevice 1028
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
        // 2 AMD + 1 VMware + 4 Intel, plus the duplicate line's device.
        assert_eq!(ids.devices.len(), 8, "{:?}", ids.devices);
    }

    #[test]
    fn parse_ids_skips_subsystem_lines() {
        // The subsystem line "\t\t1028 0002" came before the real device
        // 8086:1028, and the first-wins map kept the subsystem's name.
        let ids = parse_ids(PCI_IDS);
        assert_eq!(
            ids.devices.get(&(0x8086, 0x1028)).map(String::as_str),
            Some("RealDevice 1028")
        );
        assert!(!ids.devices.values().any(|v| v.contains("PowerEdge")));
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
        let ids = parse_ids(PCI_IDS);

        assert_eq!(
            ids.device_name("0x8086", "0x9a49").as_deref(),
            Some("Intel Corporation Meteor Lake-P [Intel Arc Graphics]")
        );
        assert_eq!(
            ids.describe("0x15ad", "0xffff", "unknown"),
            "VMware, Inc.",
            "an unknown device falls back to its vendor"
        );
        assert_eq!(ids.describe("0xdead", "0xbeef", "unknown"), "unknown");
    }

    #[test]
    fn device_name_is_none_for_an_unknown_or_malformed_id() {
        let ids = parse_ids(PCI_IDS);

        assert_eq!(ids.device_name("nope", "0x0000"), None);
        assert_eq!(ids.device_name("0x8086", "nope"), None);
    }

    #[test]
    fn ids_load_from_the_hwdata_path() {
        let fx = Fixture::new();
        fx.write("usr/share/hwdata/pci.ids", PCI_IDS);

        let ids = Ids::load(&fx.host());
        assert_eq!(
            ids.vendor_name("0x1002").as_deref(),
            Some("Advanced Micro Devices, Inc. [AMD/ATI]")
        );
        assert!(Ids::load(&Fixture::new().host()).vendors.is_empty());
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
        assert!(!looks_like_slot("0000:00:02:1.0"), "too many colons");
        assert!(!looks_like_slot("0000:00:02.x"), "function is not hex");
        assert!(!looks_like_slot(""));
        assert!(!looks_like_slot("drm"));
    }

    // ---- pci_slot_of -------------------------------------------------------

    #[test]
    fn a_gpu_behind_bridges_resolves_to_its_own_slot() {
        // The layout of a discrete GPU: root port, then the card's own
        // upstream and downstream switch ports, then the GPU.
        let fx = Fixture::new();
        fx.mkdir("sys/devices/pci0000:00/0000:00:01.0/0000:01:00.0/0000:02:00.0/0000:03:00.0");
        fx.symlink(
            "sys/class/drm/card1/device",
            "../../../devices/pci0000:00/0000:00:01.0/0000:01:00.0/0000:02:00.0/0000:03:00.0",
        );

        assert_eq!(
            pci_slot_of(&fx.dir().join("sys/class/drm/card1/device")).as_deref(),
            Some("0000:03:00.0")
        );
    }

    #[test]
    fn an_integrated_gpu_on_the_root_complex_resolves_to_its_slot() {
        let fx = Fixture::new();
        fx.mkdir("sys/devices/pci0000:00/0000:00:02.0");
        fx.symlink(
            "sys/class/drm/card0/device",
            "../../../devices/pci0000:00/0000:00:02.0",
        );

        assert_eq!(
            pci_slot_of(&fx.dir().join("sys/class/drm/card0/device")).as_deref(),
            Some("0000:00:02.0")
        );
    }

    // ---- rows --------------------------------------------------------------

    #[test]
    fn rows_list_each_device_with_its_name_driver_and_ids() {
        let fx = Fixture::new();
        let dev = "sys/bus/pci/devices/0000:00:02.0";
        fx.write(&format!("{dev}/class"), "0x030000\n");
        fx.write(&format!("{dev}/vendor"), "0x8086\n");
        fx.write(&format!("{dev}/device"), "0x9a49\n");
        fx.symlink(&format!("{dev}/driver"), "../../../bus/pci/drivers/i915");
        let rows = rows(&fx.host(), &parse_ids(PCI_IDS));

        assert_eq!(
            rows,
            [Row::field(
                "0000:00:02.0  i915",
                "Intel Corporation Meteor Lake-P [Intel Arc Graphics]  [0x030000 0x8086:0x9a49]"
            )]
        );
    }

    #[test]
    fn a_machine_without_pci_says_so() {
        assert_eq!(
            rows(&Fixture::new().host(), &Ids::default()),
            [Row::note("no pci bus")]
        );
    }
}
