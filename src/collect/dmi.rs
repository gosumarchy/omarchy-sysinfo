//! Firmware identity and boot features, straight out of `/sys/class/dmi/id`
//! and the EFI variables.

use std::path::Path;

use super::fs::{list_dir, read_bytes, try_read};
use super::units::dash;
use super::{Host, Row};

/// Laptop vendors rename these boards, so the product code is usually more
/// honest than the name.
const FIELDS: &[(&str, &str)] = &[
    ("sys_vendor", "Vendor"),
    ("product_name", "Product"),
    ("product_version", "Product version"),
    ("board_vendor", "Board vendor"),
    ("board_name", "Board"),
    ("board_version", "Board version"),
    ("bios_vendor", "BIOS vendor"),
    ("bios_version", "BIOS version"),
    ("bios_date", "BIOS date"),
    ("chassis_vendor", "Chassis vendor"),
    ("chassis_type", "Chassis type"),
    ("firmware_version", "Firmware version"),
];

const DMI: &str = "/sys/class/dmi/id";

const SECURE_BOOT_VAR: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";
const LOADER_INFO_VAR: &str =
    "/sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

/// How many bytes of an efivar file are header rather than data.
///
/// A variable read through `/sys/firmware/efi/efivars` starts with four
/// attribute bytes, and the data follows. Getting this wrong is why Secure Boot
/// used to report `disabled` on a machine with it enabled: the check looked at
/// the first attribute byte instead of the byte after the header.
const EFI_HEADER: usize = 4;

/// The board and firmware section. Read once per process by the collector:
/// none of it changes without a reboot.
pub(crate) fn rows(host: &Host) -> Vec<Row> {
    let base = host.path(DMI);
    let mut rows = Vec::new();
    let mut exposed = false;
    // The identity files are world-readable, but the serials and UUID are
    // mode 0400, and some hardened kernels restrict the whole directory. An
    // unreadable file is a different answer from "this is a VM", and it is
    // worth telling the user which one they are looking at.
    let mut denied = false;

    for (file, label) in FIELDS {
        match try_read(base.join(file)) {
            Ok(Some(value)) => {
                exposed = true;
                // `chassis_type` is a number whose meaning lives in the spec, so
                // show the name and keep the raw code beside it.
                let value = if *file == "chassis_type" {
                    format!("{} ({value})", chassis_name(&value))
                } else {
                    value
                };
                rows.push(Row::field(*label, value));
            }
            Ok(None) => {}
            Err(_) => denied = true,
        }
    }

    // Checked before the firmware rows are added. Appending them first made
    // this unreachable, so a VM or container with no DMI at all was shown an
    // empty "Firmware features" heading instead of the explanation.
    if !exposed {
        return vec![Row::note(if denied {
            "dmi present but unreadable (needs root; try: sudo omarchy-sysinfo)"
        } else {
            "dmi not exposed (vm or container?)"
        })];
    }

    rows.extend(identifier_rows(&base));
    rows.extend(firmware_rows(host));

    rows
}

/// Values that name this one machine rather than its model. Only root can
/// read most of them, and the plain report hides them unless asked.
fn identifier_rows(base: &Path) -> Vec<Row> {
    let mut rows = Vec::new();

    if let Some(uuid) = super::fs::read(base.join("product_uuid"))
        && uuid != "None"
        && !uuid.starts_with("FFFFFFFF")
    {
        rows.push(Row::identifier("Product UUID", uuid));
    }
    for (file, label) in [
        ("product_serial", "Serial number"),
        ("board_serial", "Board serial"),
    ] {
        if let Some(serial) = super::fs::read(base.join(file)) {
            rows.push(Row::identifier(label, serial));
        }
    }
    if let Some(sku) = super::fs::read(base.join("product_sku")) {
        rows.push(Row::field("SKU", sku));
    }
    if let Some(family) = super::fs::read(base.join("product_family")) {
        rows.push(Row::field("Family", family));
    }

    rows
}

fn firmware_rows(host: &Host) -> Vec<Row> {
    let mut rows = vec![Row::header("Firmware features")];

    if let Some(state) = read_bytes(host.path(SECURE_BOOT_VAR)).and_then(|b| secure_boot_from(&b)) {
        rows.push(Row::field("Secure Boot", state));
    }
    // The loader description was read and then thrown away by a
    // `label == "Secure Boot"` guard, so this row never appeared.
    if let Some(loader) = read_bytes(host.path(LOADER_INFO_VAR)).and_then(|b| efi_text_from(&b)) {
        rows.push(Row::field("Boot loader", loader));
    }
    rows.push(Row::field("EFI", efi_state(host)));
    rows.push(Row::field(
        "Kernel lockdown",
        dash(host.read("/sys/kernel/security/lockdown")),
    ));

    rows
}

fn efi_state(host: &Host) -> String {
    if !host.exists("/sys/firmware/efi") {
        return "legacy bios".into();
    }

    let vars = list_dir(host.path("/sys/firmware/efi/efivars")).len();

    format!("UEFI with {vars} firmware variables")
}

/// Whether the firmware has Secure Boot turned on.
fn secure_boot_from(raw: &[u8]) -> Option<&'static str> {
    Some(match raw.get(EFI_HEADER)? {
        0x01 => "enabled",
        _ => "disabled",
    })
}

/// A text efivar, decoded.
///
/// The payload is UTF-16LE. Reading it as bytes put a NUL between every
/// character, which is what made `omarchy-sysinfo --plain | grep` report
/// "binary file matches" and paste a loader description into a bug report as a
/// wall of NULs.
fn efi_text_from(raw: &[u8]) -> Option<String> {
    let payload = raw.get(EFI_HEADER..)?;

    // An odd trailing byte cannot be a UTF-16 unit; `as_chunks` leaves it in
    // the remainder rather than let from_utf16_lossy put a replacement
    // character in the report.
    let (pairs, _odd_byte) = payload.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    let text = String::from_utf16_lossy(&units);
    let text = text.trim_end_matches('\u{0}').trim();

    (!text.is_empty()).then(|| text.to_string())
}

/// Human name of the chassis type, e.g. `10` means Notebook.
pub(crate) fn chassis(host: &Host) -> String {
    match host.read("/sys/class/dmi/id/chassis_type") {
        Some(code) => chassis_name(&code),
        None => "unknown".into(),
    }
}

/// The SMBIOS chassis type table (DSP0134, 7.4.1).
fn chassis_name(code: &str) -> String {
    let Ok(code) = code.trim().parse::<u8>() else {
        return "unknown".into();
    };

    let name = match code {
        3 => "Desktop",
        4 => "Low Profile Desktop",
        6 => "Mini Tower",
        7 => "Tower",
        8 => "Portable",
        9 => "Laptop",
        10 => "Notebook",
        11 => "Hand Held",
        12 => "Docking Station",
        13 => "All In One",
        14 => "Sub Notebook",
        15 => "Space-saving Server",
        16 => "Lunch Box",
        17 => "Main Server Chassis",
        18 => "Expansion Chassis",
        19 => "Sub Chassis",
        20 => "Bus Expansion Chassis",
        21 => "Peripheral Chassis",
        22 => "RAID Chassis",
        23 => "Rack Mount Chassis",
        24 => "Sealed-case PC",
        25 => "Multi-system",
        26 => "Compact PCI",
        27 => "Advanced TCA",
        28 => "Blade",
        29 => "Blade Enclosure",
        30 => "Tablet",
        31 => "Convertible",
        32 => "Detachable",
        33 => "IoT Gateway",
        34 => "Embedded PC",
        35 => "Mini PC",
        36 => "Stick PC",
        // 1 is "Other", 2 "Unknown", and anything past 36 is newer than this
        // table: show the code rather than guess.
        other => return format!("code {other}"),
    };

    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::Sensitivity;
    use crate::collect::fixture::Fixture;

    fn text(rows: &[Row]) -> String {
        format!("{rows:?}")
    }

    /// The bytes a real efivar file holds: four attribute bytes, then the data.
    fn efivar(data: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x06, 0x00, 0x00, 0x00];
        bytes.extend_from_slice(data);
        bytes
    }

    /// UTF-16LE, which is how the firmware stores a text variable.
    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    // ---- rows ------------------------------------------------------------

    #[test]
    fn a_machine_without_dmi_says_so() {
        let fx = Fixture::new();
        let rows = rows(&fx.host());

        assert_eq!(rows.len(), 1);
        assert!(text(&rows).contains("dmi not exposed"), "{}", text(&rows));
    }

    #[test]
    fn identity_fields_are_shown_with_a_named_chassis() {
        let fx = Fixture::new();
        fx.write("sys/class/dmi/id/sys_vendor", "LENOVO\n");
        fx.write("sys/class/dmi/id/chassis_type", "10\n");
        let rows = rows(&fx.host());

        assert!(text(&rows).contains("LENOVO"));
        assert!(text(&rows).contains("Notebook (10)"), "{}", text(&rows));
        assert!(text(&rows).contains("Firmware features"));
    }

    #[test]
    fn serials_and_the_uuid_are_identifiers() {
        let fx = Fixture::new();
        fx.write("sys/class/dmi/id/sys_vendor", "LENOVO\n");
        fx.write(
            "sys/class/dmi/id/product_uuid",
            "4c4c4544-0042-3510-8052-b4c04f4e3432\n",
        );
        fx.write("sys/class/dmi/id/product_serial", "PF3ABCDE\n");
        let rows = rows(&fx.host());

        let identifiers = rows
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Row::Field {
                        sensitivity: Sensitivity::Identifier,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(identifiers, 2, "{}", text(&rows));
    }

    #[test]
    fn a_placeholder_uuid_is_not_shown() {
        let fx = Fixture::new();
        fx.write("sys/class/dmi/id/sys_vendor", "QEMU\n");
        fx.write(
            "sys/class/dmi/id/product_uuid",
            "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF\n",
        );

        assert!(!text(&rows(&fx.host())).contains("Product UUID"));
    }

    #[test]
    fn efi_and_secure_boot_come_from_the_efivars() {
        let fx = Fixture::new();
        fx.write("sys/class/dmi/id/sys_vendor", "LENOVO\n");
        fx.write_bytes(SECURE_BOOT_VAR.trim_start_matches('/'), &efivar(&[0x01]));
        fx.write_bytes(
            LOADER_INFO_VAR.trim_start_matches('/'),
            &efivar(&utf16("Limine 12.8.0\0")),
        );
        let rows = rows(&fx.host());

        assert!(text(&rows).contains("\"enabled\""), "{}", text(&rows));
        assert!(text(&rows).contains("Limine 12.8.0"), "{}", text(&rows));
        assert!(text(&rows).contains("UEFI with 2 firmware variables"));
    }

    #[test]
    fn chassis_names_known_codes_and_shows_unknown_ones() {
        assert_eq!(chassis_name("10"), "Notebook");
        assert_eq!(chassis_name("35\n"), "Mini PC");
        assert_eq!(chassis_name("99"), "code 99");
        assert_eq!(chassis_name("x"), "unknown");
        assert_eq!(chassis(&Fixture::new().host()), "unknown");
    }

    // ---- efivars ---------------------------------------------------------

    #[test]
    fn a_text_efivar_is_decoded_rather_than_shown_as_nuls() {
        // This is the exact shape of systemd-boot's LoaderInfo. Read as bytes
        // it was "L\0i\0m\0i\0n\0e\0", and every NUL made grep treat the whole
        // report as a binary file.
        let raw = efivar(&utf16("Limine 12.8.0\0"));
        assert_eq!(efi_text_from(&raw), Some("Limine 12.8.0".into()));
    }

    #[test]
    fn a_text_efivar_with_no_header_or_no_text_is_absent() {
        assert_eq!(efi_text_from(b""), None, "too short to hold a header");
        assert_eq!(efi_text_from(&efivar(&[])), None, "header but no data");
        assert_eq!(
            efi_text_from(&efivar(&utf16("\0\0"))),
            None,
            "all NUL is no value"
        );
    }

    #[test]
    fn an_odd_trailing_byte_does_not_become_a_replacement_character() {
        let mut data = utf16("ab");
        data.push(0x41);
        let text = efi_text_from(&efivar(&data)).expect("text");
        assert_eq!(text, "ab", "half a UTF-16 unit is dropped, not mangled");
    }

    #[test]
    fn secure_boot_reads_the_byte_after_the_header() {
        // The bug: the check looked at the first attribute byte, so an enabled
        // machine still reported "disabled".
        assert_eq!(secure_boot_from(&efivar(&[0x01])), Some("enabled"));
        assert_eq!(secure_boot_from(&efivar(&[0x00])), Some("disabled"));
        assert_eq!(secure_boot_from(&efivar(&[0x02])), Some("disabled"));
    }

    #[test]
    fn secure_boot_is_absent_without_the_variable() {
        assert_eq!(secure_boot_from(b""), None);
        assert_eq!(secure_boot_from(&efivar(&[])), None, "no data byte");
    }
}
