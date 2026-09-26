use super::{
    fs::{read, read_u64},
    units::human_bytes,
    Row,
};
use std::path::Path;

pub fn rows() -> Vec<Row> {
    let mounts = mount_table();
    let mut rows = Vec::new();
    let mut disks = 0usize;

    for block in super::fs::list_dir("/sys/block") {
        let name = block
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("sr") {
            continue;
        }
        let Some(size_sectors) = read_u64(block.join("size")) else {
            continue;
        };
        let size = size_sectors.sectors_to_bytes();
        if size == 0 {
            continue;
        }
        disks += 1;

        let rotational = read(block.join("queue/rotational")).as_deref() == Some("1");
        // zram is compressed memory, not a disk: never call it an SSD.
        let ram_disk = name.starts_with("zram")
            || std::fs::read_link(&block)
                .map(|t| t.to_string_lossy().contains("zram"))
                .unwrap_or(false);
        let model = read(block.join("device/model"))
            .or_else(|| read(block.join("device/name")))
            .unwrap_or_else(|| {
                if rotational {
                    "unknown rotational".to_string()
                } else {
                    "unknown".to_string()
                }
            });
        let scheduler = read(block.join("queue/scheduler")).unwrap_or_default();
        let rotational_label = if ram_disk {
            "RAM disk"
        } else if rotational {
            "HDD"
        } else {
            "SSD / NVMe"
        };
        let mm = read(block.join("queue/logical_block_size"))
            .map(|v| format!("{v} B"))
            .unwrap_or_default();

        let mut mounted: Vec<String> = mounts
            .iter()
            .filter(|(dev, _, _)| device_matches(dev, &name))
            .map(|(_, fstype, point)| format!("{point} ({fstype})"))
            .collect();
        mounted.sort();
        let usage = mounts
            .iter()
            .find(|(dev, _, _)| device_matches(dev, &name))
            .and_then(|(dev, _, _)| disk_usage(dev));

        rows.push(Row::Header(format!("{name}  ·  {rotational_label}")));
        rows.push(Row::field("Capacity", human_bytes(size)));
        rows.push(Row::field("Model", model));
        if let Some(d) = read(block.join("device/wwid")) {
            rows.push(Row::field("World-wide id", d));
        }
        if !mm.is_empty() {
            rows.push(Row::field("Sector size", mm));
        }
        if !scheduler.is_empty() {
            rows.push(Row::field("Scheduler", scheduler));
        }
        rows.push(Row::field(
            "Removable",
            dash_opt(read(block.join("removable"))),
        ));
        rows.push(Row::field(
            "Mounted",
            if mounted.is_empty() {
                "-".to_string()
            } else {
                mounted.join(", ")
            },
        ));
        if let Some((used, avail, pct)) = usage {
            rows.push(Row::field_with(
                "Usage",
                format!("{used} used, {avail} free"),
                pct / 100.0,
            ));
        }
        if let Some(smart) = smart_summary(&block) {
            rows.push(Row::field("SMART", smart));
        }

        let partitions: Vec<String> = super::fs::list_dir(&block)
            .iter()
            .filter(|p| read(p.join("partition")).is_some())
            .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
            .collect();
        for part in partitions {
            let part_path = block.join(&part);
            let part_size = read_u64(part_path.join("size"))
                .map(|s| s.sectors_to_bytes())
                .unwrap_or(0);
            let fstype = mounts
                .iter()
                .find(|(dev, _, _)| device_matches(dev, &part))
                .map(|(_, fs, _)| fs.clone())
                .unwrap_or_else(|| "-".into());
            let point = mounts
                .iter()
                .find(|(dev, _, _)| device_matches(dev, &part))
                .map(|(_, _, p)| p.clone())
                .unwrap_or_else(|| "-".into());
            rows.push(Row::field(
                format!("  {part}"),
                format!("{}  {point}  {fstype}", human_bytes(part_size)),
            ));
        }
    }

    if disks == 0 {
        return vec![Row::note("no block devices")];
    }

    rows.push(Row::Header("Storage totals".into()));
    rows.push(Row::field("Block devices", disks.to_string()));
    if let Some(root) = root_disk() {
        rows.push(Row::field("Root filesystem on", root));
    }
    if let Some(bcache) = btrfs_summary() {
        rows.push(Row::field("Btrfs", bcache));
    }
    if let Some(trim) = read("/sys/module/fstrim/parameters/enabled") {
        rows.push(Row::field("fstrim", trim));
    }
    rows
}

trait Sectors {
    fn sectors_to_bytes(self) -> u64;
}

impl Sectors for u64 {
    fn sectors_to_bytes(self) -> u64 {
        // Saturating: a nonsense `size` must not wrap a huge disk into a tiny
        // one, or panic in a debug build.
        self.saturating_mul(512)
    }
}

fn dash_opt(v: Option<String>) -> String {
    match v {
        Some(s) if s == "0" => "no".to_string(),
        Some(s) if s == "1" => "yes".to_string(),
        Some(s) => s,
        None => "-".to_string(),
    }
}

type Mounts = Vec<(String, String, String)>;

fn mount_table() -> Mounts {
    match read("/proc/mounts") {
        Some(content) => parse_mounts(&content),
        None => Vec::new(),
    }
}

fn parse_mounts(content: &str) -> Mounts {
    content
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let dev = parts.next()?.to_string();
            let point = parts.next()?.to_string();
            let fstype = parts.next()?.to_string();
            Some((unescape(&dev), fstype, unescape(&point)))
        })
        .filter(|(dev, fs, _)| {
            dev.starts_with("/dev/") || matches!(fs.as_str(), "zfs" | "btrfs" | "overlay")
        })
        .collect()
}

/// `/proc/mounts` escapes whitespace and backslashes in paths as three-digit
/// octal. Decoding only `\040` left paths containing a literal backslash or
/// newline showing their escape sequence in the UI.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '\\' && i + 3 < bytes.len() {
            let digits: String = bytes[i + 1..i + 4].iter().collect();
            if digits.len() == 3 && digits.chars().all(|c| ('0'..='7').contains(&c)) {
                if let Ok(byte) = u8::from_str_radix(&digits, 8) {
                    // Multi-byte UTF-8 is escaped byte by byte, so rebuild it.
                    out.push(byte as char);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// `/proc/mounts` names the device the user sees, which for LVM or btrfs is a
/// symlink like `/dev/mapper/root`. Resolve it so it matches `/sys/block/dm-0`.
fn device_matches(device: &str, name: &str) -> bool {
    let base = |s: &str| {
        Path::new(s)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| s.to_string())
    };
    if name_matches(&base(device), name) {
        return true;
    }
    if let Ok(resolved) = std::fs::canonicalize(device) {
        if name_matches(&base(&resolved.to_string_lossy()), name) {
            return true;
        }
    }
    false
}

/// Whether a mount source refers to `name` or to one of its partitions.
///
/// Partition naming is not uniform: SATA and USB disks append digits directly
/// (`sda1`), while NVMe and mmc insert a `p` (`nvme0n1p1`, `mmcblk0p1`). Only
/// recognising the `p` form meant a mounted `/dev/sda1` was reported as an
/// unmounted `sda`.
fn name_matches(base: &str, name: &str) -> bool {
    if base == name {
        return true;
    }
    let Some(rest) = base.strip_prefix(name) else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    digits(rest) || rest.strip_prefix('p').is_some_and(digits)
}

fn disk_usage(device: &str) -> Option<(String, String, f64)> {
    let out = std::process::Command::new("df")
        .args(["-B1", "--output=size,used,avail,pcent", device])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    parse_df(&text)
}

/// `df` puts a device whose name wraps onto a line of its own, so the figures
/// are the last non-empty row rather than the second.
fn parse_df(text: &str) -> Option<(String, String, f64)> {
    let last = text
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .last()?;
    let mut parts = last.split_whitespace();
    let _size = parts.next()?;
    let used = parts.next()?;
    let avail = parts.next()?;
    let pcent = parts.next()?.trim_end_matches('%');
    Some((
        human_bytes(used.parse().ok()?),
        human_bytes(avail.parse().ok()?),
        pcent.parse().ok()?,
    ))
}

fn root_disk() -> Option<String> {
    let dev = std::fs::read_link("/dev/root").ok()?;
    Some(dev.to_string_lossy().to_string())
}

fn btrfs_summary() -> Option<String> {
    let out = std::process::Command::new("btrfs")
        .args(["filesystem", "show", "/"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    parse_btrfs(&text)
}

fn parse_btrfs(text: &str) -> Option<String> {
    let label = text.lines().find(|l| l.contains("Label:")).map(|l| {
        l.split("Label:")
            .nth(1)
            .unwrap_or("")
            .split("uuid:")
            .next()
            .unwrap_or("")
            .trim()
            // btrfs prints the label in single quotes; keeping them made
            // the summary read `label "'root'"`.
            .trim_matches('\'')
            .to_string()
    })?;
    let devices = text
        .lines()
        .filter(|l| l.trim_start().starts_with("devid") || l.contains(" path /dev/"))
        .count();
    Some(format!(
        "label {:?}, {devices} device(s)",
        if label == "none" { "<none>" } else { &label }
    ))
}

fn smart_summary(block: &Path) -> Option<String> {
    if !block.join("device").exists() {
        return None;
    }
    if let Some(health) = read(block.join("device/health")) {
        return Some(format!("health {}", health.trim_end_matches('\n')));
    }
    let power = read_u64(block.join("device/power_state"))?;
    Some(format!("power state {power}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- dash_opt --------------------------------------------------------

    #[test]
    fn dash_opt_spells_out_the_boolean() {
        assert_eq!(dash_opt(Some("0".into())), "no");
        assert_eq!(dash_opt(Some("1".into())), "yes");
        assert_eq!(dash_opt(Some("other".into())), "other");
        assert_eq!(dash_opt(None), "-");
    }

    // ---- sectors_to_bytes ------------------------------------------------

    #[test]
    fn sectors_convert_at_512_bytes() {
        assert_eq!(0u64.sectors_to_bytes(), 0);
        assert_eq!(1u64.sectors_to_bytes(), 512);
        assert_eq!(2048u64.sectors_to_bytes(), 1_048_576);
    }

    #[test]
    fn sector_conversion_saturates_instead_of_wrapping() {
        // A wrapped multiplication would report a huge disk as a tiny one.
        assert_eq!(u64::MAX.sectors_to_bytes(), u64::MAX);
    }

    // ---- unescape -------------------------------------------------------

    #[test]
    fn unescape_decodes_the_octal_escapes_proc_mounts_uses() {
        assert_eq!(unescape("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape("/mnt/tab\\011here"), "/mnt/tab\there");
        // A backslash is escaped as a backslash.
        assert_eq!(unescape("/mnt/back\\134slash"), "/mnt/back\\slash");
    }

    #[test]
    fn unescape_leaves_ordinary_paths_alone() {
        assert_eq!(unescape("/"), "/");
        assert_eq!(unescape("/dev/nvme0n1p2"), "/dev/nvme0n1p2");
        assert_eq!(unescape(""), "");
    }

    #[test]
    fn unescape_keeps_a_stray_backslash_rather_than_eating_it() {
        assert_eq!(unescape("/a\\b"), "/a\\b");
        assert_eq!(unescape("/a\\09"), "/a\\09", "09 is not octal");
        assert_eq!(unescape("/a\\999"), "/a\\999", "9 is not an octal digit");
        assert_eq!(unescape("/a\\"), "/a\\", "a trailing backslash");
        assert_eq!(unescape("/a\\04"), "/a\\04", "too few digits");
    }

    // ---- name_matches ----------------------------------------------------

    #[test]
    fn a_disk_matches_its_own_name() {
        assert!(name_matches("sda", "sda"));
        assert!(name_matches("nvme0n1", "nvme0n1"));
    }

    #[test]
    fn a_disk_matches_its_partitions_in_both_naming_styles() {
        // The bug: only the "p" form was recognised, so a mounted /dev/sda1
        // left the sda disk looking unmounted.
        assert!(name_matches("sda1", "sda"), "SATA/USB digit suffix");
        assert!(name_matches("sda12", "sda"));
        assert!(name_matches("nvme0n1p1", "nvme0n1"), "NVMe p suffix");
        assert!(name_matches("nvme0n1p12", "nvme0n1"));
        assert!(name_matches("mmcblk0p1", "mmcblk0"), "mmc p suffix");
    }

    #[test]
    fn a_disk_does_not_match_a_different_disk() {
        assert!(!name_matches("sdb", "sda"));
        assert!(!name_matches("nvme0n2", "nvme0n1"));
        // A shared prefix that is not a partition number.
        assert!(!name_matches("sdaa", "sda"));
        assert!(!name_matches("sdaX1", "sda"));
        assert!(!name_matches("nvme0n1xp1", "nvme0n1"));
    }

    #[test]
    fn a_bare_name_is_not_treated_as_a_partition() {
        // "p" with no digits after it is a different device.
        assert!(!name_matches("sda p", "sda"));
        assert!(!name_matches("nvme0n1p", "nvme0n1"));
    }

    // ---- parse_mounts ----------------------------------------------------

    const MOUNTS: &str = "\
proc /proc proc rw,nosuid 0 0
/dev/nvme0n1p2 /home ext4 rw,relatime 0 0
/dev/mapper/root / btrfs rw 0 0
tmpfs /run tmpfs rw 0 0
/dev/sda1 /mnt/my\\040disk vfat rw 0 0
overlay /var/lib/docker overlay rw 0 0
";

    #[test]
    fn parse_mounts_keeps_only_real_filesystems() {
        let mounts = parse_mounts(MOUNTS);
        let devices: Vec<&str> = mounts.iter().map(|(d, _, _)| d.as_str()).collect();
        assert!(devices.contains(&"/dev/nvme0n1p2"));
        assert!(devices.contains(&"/dev/mapper/root"), "btrfs root kept");
        assert!(devices.contains(&"overlay"), "overlay kept by fstype");
        assert!(!devices.contains(&"/run"), "tmpfs dropped");
        assert!(
            !devices.iter().any(|d| d.starts_with("proc")),
            "proc dropped"
        );
    }

    #[test]
    fn parse_mounts_captures_the_type_and_unescapes_the_point() {
        let mounts = parse_mounts(MOUNTS);
        let disk = mounts
            .iter()
            .find(|(d, _, _)| d == "/dev/sda1")
            .expect("sda1 present");
        assert_eq!(disk.1, "vfat");
        assert_eq!(disk.2, "/mnt/my disk", "the escaped space is decoded");
    }

    #[test]
    fn parse_mounts_skips_lines_with_too_few_columns() {
        let mounts = parse_mounts("/dev/sda1\n/dev/sdb1 /mnt ext4\n");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].2, "/mnt");
    }

    #[test]
    fn parse_mounts_of_nothing_is_empty() {
        assert!(parse_mounts("").is_empty());
    }

    // ---- parse_df --------------------------------------------------------

    #[test]
    fn parse_df_reads_used_available_and_percentage() {
        let out = "  Size   Used  Avail Use%\n1073741824 536870912 536870912  50%\n";
        let (used, avail, pct) = parse_df(out).expect("parsed");
        assert_eq!(used, "512.0 MiB");
        assert_eq!(avail, "512.0 MiB");
        assert_eq!(pct, 50.0);
    }

    #[test]
    fn parse_df_takes_the_last_row_when_df_wraps_a_long_device_name() {
        // df prints a device whose name is too long onto its own line, leaving
        // the figures on the following row.
        let out = "  Size   Used  Avail Use%\n\
                   /dev/mapper/a-really-long-volume-group-name\n\
                   1073741824 268435456 805306368  25%\n";
        let (_, _, pct) = parse_df(out).expect("parsed");
        assert_eq!(pct, 25.0);
    }

    #[test]
    fn parse_df_rejects_incomplete_output() {
        assert!(parse_df("").is_none());
        assert!(parse_df("  Size   Used  Avail Use%\n").is_none());
        assert!(
            parse_df("header\n1 2 3\n").is_none(),
            "missing the percentage"
        );
        assert!(
            parse_df("header\n1 2 3 x%\n").is_none(),
            "percentage not a number"
        );
    }

    // ---- parse_btrfs -----------------------------------------------------

    #[test]
    fn parse_btrfs_reads_the_label_and_device_count() {
        let out = "\
Label: 'myroot'  uuid: 1234abcd
Data profile: single
Devices:
   ID    gen    top level  path
   1     20     5           path /dev/sda2
devid    1 size 1.0 GiB used 0.00 B
";
        let s = parse_btrfs(out).expect("parsed");
        assert!(s.contains("myroot"), "{s}");
        assert!(!s.contains('\''), "quotes must not be doubled: {s}");
        assert!(s.contains("2 device(s)"), "{s}");
    }

    #[test]
    fn parse_btrfs_labels_an_unnamed_filesystem() {
        let out = "Label: 'none'  uuid: 1234\ndevid    1 size 1 GiB\n";
        let s = parse_btrfs(out).expect("parsed");
        assert!(s.contains("<none>"), "{s}");
    }

    #[test]
    fn parse_btrfs_needs_a_label_line() {
        assert!(parse_btrfs("").is_none());
        assert!(parse_btrfs("devid 1 size 1 GiB\n").is_none());
    }
}
