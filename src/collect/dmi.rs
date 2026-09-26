use super::{
    fs::{read, try_read},
    units::dash,
    Row,
};
use std::path::Path;

/// Firmware identity straight out of `/sys/class/dmi/id`. Laptop vendors rename
/// these boards, so the product code is usually more honest than the name.
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

pub fn rows() -> Vec<Row> {
    let base = Path::new("/sys/class/dmi/id");
    let mut rows = Vec::new();
    let mut exposed = false;
    // `/sys/class/dmi/id/*` is mode 0400 on most distros, so an unprivileged run
    // cannot read a single field even though the hardware is right there. That is
    // a different answer from "this is a VM", and it is worth telling the user
    // which one they are looking at.
    let mut denied = false;

    for (file, label) in FIELDS {
        match try_read(base.join(file)) {
            Ok(Some(value)) => {
                exposed = true;
                // `chassis_type` is a number whose meaning lives in the spec, so
                // show the name and keep the raw code beside it.
                if *file == "chassis_type" {
                    let code = value.trim();
                    rows.push(Row::field(*label, format!("{} ({code})", chassis())));
                } else {
                    rows.push(Row::field(*label, value));
                }
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

    if let Some(uuid) = read(base.join("product_uuid")) {
        if uuid != "None" && !uuid.starts_with("FFFFFFFF") {
            rows.push(Row::field("Product UUID", uuid));
        }
    }
    if let Some(sku) = read(base.join("product_sku")) {
        rows.push(Row::field("SKU", sku));
    }
    if let Some(family) = read(base.join("product_family")) {
        rows.push(Row::field("Family", family));
    }

    rows.push(Row::Header("Firmware features".into()));
    if let Some(state) = secure_boot_state() {
        rows.push(Row::field("Secure Boot", state));
    }
    // The loader description was read and then thrown away by a
    // `label == "Secure Boot"` guard, so this row never appeared.
    if let Some(loader) = efi_text(SECURE_BOOT_LOADER) {
        rows.push(Row::field("Boot loader", loader));
    }
    rows.push(Row::field("EFI", efi_state()));
    rows.push(Row::field(
        "Kernel lockdown",
        dash(read("/sys/kernel/security/lockdown")),
    ));

    rows
}

fn efi_state() -> String {
    let efi = Path::new("/sys/firmware/efi");
    if !efi.exists() {
        return "legacy bios".into();
    }
    let vars = super::fs::list_dir(efi.join("efivars")).len();
    format!("UEFI with {vars} firmware variables")
}

const SECURE_BOOT_VAR: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";
const SECURE_BOOT_LOADER: &str =
    "/sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

/// How many bytes of an efivar file are header rather than data.
///
/// A variable read through `/sys/firmware/efi/efivars` starts with four
/// attribute bytes, and the data follows. Getting this wrong is why Secure Boot
/// used to report `disabled` on a machine with it enabled: the check looked at
/// the first attribute byte instead of the byte after the header.
const EFI_HEADER: usize = 4;

/// Whether the firmware has Secure Boot turned on.
fn secure_boot_state() -> Option<String> {
    secure_boot_from(&read(Path::new(SECURE_BOOT_VAR))?)
}

fn secure_boot_from(raw: &str) -> Option<String> {
    match raw.as_bytes().get(EFI_HEADER)? {
        0x01 => Some("enabled".into()),
        _ => Some("disabled".into()),
    }
}

/// A text efivar, decoded.
///
/// The payload is UTF-16LE. Reading it as bytes put a NUL between every
/// character, which is what made `omarchy-sysinfo --plain | grep` report
/// "binary file matches" and paste a loader description into a bug report as a
/// wall of NULs.
fn efi_text(path: &str) -> Option<String> {
    efi_text_from(&read(Path::new(path))?)
}

fn efi_text_from(raw: &str) -> Option<String> {
    let payload = raw.as_bytes().get(EFI_HEADER..)?;

    // An odd trailing byte cannot be a UTF-16 unit; `chunks_exact` drops it
    // rather than let from_utf16_lossy put a replacement character in the
    // report.
    let units: Vec<u16> = payload
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let text = String::from_utf16_lossy(&units);
    let text = text.trim_end_matches('\u{0}').trim();

    (!text.is_empty()).then(|| text.to_string())
}

/// Human name of the chassis type, e.g. `10` means Notebook.
pub fn chassis() -> String {
    let Some(code) =
        read(Path::new("/sys/class/dmi/id/chassis_type")).and_then(|v| v.parse::<u8>().ok())
    else {
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
        _ => return format!("code {code}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_render_without_panicking() {
        let rows = rows();
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "{text}");
        assert!(!rows.is_empty());
    }

    #[test]
    fn rows_either_expose_dmi_or_say_so() {
        // Either the identity fields are present, or the note explains why not.
        let has_identity = rows()
            .iter()
            .any(|r| matches!(r, Row::Field { label, .. } if label == "Vendor"));
        let has_note = rows().iter().any(|r| matches!(r, Row::Note(_)));
        assert!(has_identity || has_note, "{:?}", rows());
    }

    #[test]
    fn chassis_always_names_something() {
        assert!(!chassis().is_empty());
    }

    #[test]
    fn efi_state_always_says_something() {
        assert!(!efi_state().is_empty());
    }

    // ---- efivars ---------------------------------------------------------

    /// The bytes a real efivar file holds: four attribute bytes, then the data.
    fn efivar(data: &[u8]) -> String {
        let mut bytes = vec![0x06, 0x00, 0x00, 0x00];
        bytes.extend_from_slice(data);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// UTF-16LE, which is how the firmware stores a text variable.
    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn a_text_efivar_is_decoded_rather_than_shown_as_nuls() {
        // This is the exact shape of systemd-boot's LoaderInfo on this machine.
        // Read as bytes it was "L\0i\0m\0i\0n\0e\0", and every NUL made grep
        // treat the whole report as a binary file.
        let raw = efivar(&utf16("Limine 12.8.0\0"));
        assert_eq!(efi_text_from(&raw), Some("Limine 12.8.0".into()));
    }

    #[test]
    fn a_text_efivar_with_no_header_or_no_text_is_absent() {
        assert_eq!(efi_text_from(""), None, "too short to hold a header");
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
        let text = efi_text_from(&efivar(&data)).unwrap();
        assert_eq!(text, "ab", "half a UTF-16 unit is dropped, not mangled");
    }

    #[test]
    fn secure_boot_reads_the_byte_after_the_header() {
        // The bug: the check looked at the first attribute byte, so an enabled
        // machine still reported "disabled".
        assert_eq!(secure_boot_from(&efivar(&[0x01])), Some("enabled".into()));
        assert_eq!(secure_boot_from(&efivar(&[0x00])), Some("disabled".into()));
        assert_eq!(secure_boot_from(&efivar(&[0x02])), Some("disabled".into()));
    }

    #[test]
    fn secure_boot_is_absent_without_the_variable() {
        assert_eq!(secure_boot_from(""), None);
        assert_eq!(secure_boot_from(&efivar(&[])), None, "no data byte");
    }
}
