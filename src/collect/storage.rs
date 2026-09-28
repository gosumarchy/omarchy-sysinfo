//! Disks, their partitions, and the filesystems that end up on them.
//!
//! The mapping from a disk to what is mounted from it is not one step on a
//! modern install. Omarchy's default is LUKS with btrfs on top, so `/` is
//! mounted from `/dev/mapper/root`, which is `dm-0`, which is held open on
//! `nvme0n1p2`. Matching mount sources to partition names by string missed
//! every one of those, and credited the disk with `/boot`'s usage instead.
//! Here each partition follows its `holders` links up to whatever is
//! mounted, however many device-mapper layers (LUKS, LVM) sit in between.

use std::path::{Path, PathBuf};

use super::fs::{file_name, list_dir, read, read_u64};
use super::units::{human_bytes, round_u64};
use super::{Host, Row};
use crate::sys::{FsUsage, fs_usage};

/// How deep a holder chain is followed. Real stacks are two or three deep
/// (partition, LUKS, LVM); the limit only guards against a cycle.
const MAX_HOLDER_DEPTH: usize = 8;

pub(crate) fn rows(host: &Host) -> Vec<Row> {
    let mounts = mount_table(host);
    let disks: Vec<Disk> = host
        .list_dir("/sys/block")
        .iter()
        .filter_map(|path| Disk::read(path))
        .collect();

    if disks.is_empty() {
        return vec![Row::note("no block devices")];
    }

    let mut rows = Vec::new();
    for disk in &disks {
        rows.extend(disk_rows(host, disk, &mounts));
    }

    rows.push(Row::header("Storage totals"));
    rows.push(Row::field("Block devices", disks.len().to_string()));
    if let Some(root) = mounts.iter().find(|m| m.point == "/") {
        rows.push(Row::field("Root filesystem on", describe_root(host, root)));
        if root.fstype == "btrfs"
            && let Some(summary) = root.device.as_deref().and_then(|d| btrfs_summary(host, d))
        {
            rows.push(Row::field("Btrfs", summary));
        }
    }
    rows.push(Row::field("Periodic TRIM", fstrim_state(host)));

    rows
}

/// What kind of device a disk is, as far as sysfs can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Media {
    Rotational,
    SolidState,
    /// zram: compressed memory, never an SSD.
    Memory,
}

impl Media {
    fn label(self) -> &'static str {
        match self {
            Media::Rotational => "HDD",
            Media::SolidState => "SSD / NVMe",
            Media::Memory => "RAM disk",
        }
    }
}

/// A top-level entry of `/sys/block` that is a real disk.
#[derive(Clone, Debug)]
struct Disk {
    name: String,
    path: PathBuf,
    size: u64,
    media: Media,
    partitions: Vec<Partition>,
}

#[derive(Clone, Debug)]
struct Partition {
    name: String,
    path: PathBuf,
    size: u64,
}

impl Disk {
    fn read(path: &Path) -> Option<Disk> {
        let name = file_name(path);
        // Loop devices and optical drives are not storage anyone asks about,
        // and device-mapper nodes (dm-N) are shown under the partition they
        // sit on rather than as disks of their own.
        if ["loop", "ram", "sr", "dm-"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            return None;
        }

        let size = sectors_to_bytes(read_u64(path.join("size"))?);
        if size == 0 {
            return None;
        }

        let media = if name.starts_with("zram") {
            Media::Memory
        } else if read(path.join("queue/rotational")).as_deref() == Some("1") {
            Media::Rotational
        } else {
            Media::SolidState
        };

        let partitions = list_dir(path)
            .into_iter()
            .filter(|p| p.join("partition").exists())
            .map(|p| Partition {
                name: file_name(&p),
                size: read_u64(p.join("size")).map_or(0, sectors_to_bytes),
                path: p,
            })
            .collect();

        Some(Disk {
            name,
            path: path.to_path_buf(),
            size,
            media,
            partitions,
        })
    }
}

fn disk_rows(host: &Host, disk: &Disk, mounts: &[Mount]) -> Vec<Row> {
    let path = &disk.path;
    let mut rows = vec![
        Row::header(format!("{}  ·  {}", disk.name, disk.media.label())),
        Row::field("Capacity", human_bytes(disk.size)),
        Row::field(
            "Model",
            read(path.join("device/model"))
                .or_else(|| read(path.join("device/name")))
                .unwrap_or_else(|| "unknown".into()),
        ),
    ];
    if let Some(wwid) = read(path.join("device/wwid")) {
        rows.push(Row::identifier("World-wide id", wwid));
    }
    if let Some(sector) = read(path.join("queue/logical_block_size")) {
        rows.push(Row::field("Sector size", format!("{sector} B")));
    }
    if let Some(scheduler) = read(path.join("queue/scheduler")) {
        rows.push(Row::field("Scheduler", scheduler));
    }
    rows.push(Row::field(
        "Removable",
        yes_no(read(path.join("removable")).as_deref()),
    ));

    // An unpartitioned disk can carry a filesystem (or a LUKS container) of
    // its own, and is then its own only "partition".
    let volumes: Vec<Volume<'_>> = if disk.partitions.is_empty() {
        vec![Volume {
            name: &disk.name,
            path: &disk.path,
            size: disk.size,
            indent: false,
        }]
    } else {
        disk.partitions
            .iter()
            .map(|p| Volume {
                name: &p.name,
                path: &p.path,
                size: p.size,
                indent: true,
            })
            .collect()
    };

    for volume in volumes {
        rows.extend(volume_rows(host, &volume, mounts));
    }

    rows
}

/// A partition, or a whole disk used without a partition table.
struct Volume<'a> {
    name: &'a str,
    path: &'a Path,
    size: u64,
    indent: bool,
}

/// One line for the volume, then a usage gauge for each filesystem it
/// carries (directly, or through LUKS/LVM).
fn volume_rows(host: &Host, volume: &Volume<'_>, mounts: &[Mount]) -> Vec<Row> {
    let stack = holder_stack(host, volume.name, volume.path);
    let top = stack
        .last()
        .map_or(volume.name, |layer| layer.name.as_str());
    let mounted: Vec<&Mount> = mounts
        .iter()
        .filter(|m| m.device.as_deref() == Some(top))
        .collect();

    let mut detail = vec![human_bytes(volume.size)];
    for layer in &stack {
        detail.push(format!("→ {}", layer.describe()));
    }
    match mounted.first() {
        Some(first) => {
            let mut points: Vec<&str> = mounted.iter().map(|m| m.point.as_str()).collect();
            points.sort_unstable();
            detail.push(first.fstype.clone());
            detail.push(points.join(", "));
        }
        None => detail.push("not mounted".into()),
    }

    let label = if volume.indent {
        format!("  {}", volume.name)
    } else {
        volume.name.to_string()
    };
    let mut rows = vec![Row::field(label, detail.join("  "))];

    // Every subvolume of one btrfs reports the same usage, so one gauge per
    // filesystem, taken from its first mount point.
    if let Some(first) = mounted.first()
        && let Some(usage) = fs_usage(&host.path(&first.point))
    {
        rows.push(usage_row(usage));
    }

    rows
}

fn usage_row(usage: FsUsage) -> Row {
    Row::field_with(
        "    used",
        format!(
            "{} used, {} free, {}%",
            human_bytes(usage.used),
            human_bytes(usage.avail),
            round_u64(usage.fraction() * 100.0)
        ),
        usage.fraction(),
    )
}

/// A device-mapper node stacked on a partition.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Layer {
    /// Kernel name, `dm-0`.
    name: String,
    /// The mapper name, `root` for `/dev/mapper/root`.
    mapper: Option<String>,
    kind: LayerKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayerKind {
    Luks,
    Lvm,
    Other,
}

impl Layer {
    fn read(host: &Host, name: &str) -> Layer {
        let dm = host.path(format!("/sys/class/block/{name}/dm"));
        // The dm uuid names the target that created the node:
        // CRYPT-LUKS2-..., LVM-..., and so on.
        let uuid = read(dm.join("uuid")).unwrap_or_default();
        let kind = if uuid.starts_with("CRYPT-") {
            LayerKind::Luks
        } else if uuid.starts_with("LVM-") {
            LayerKind::Lvm
        } else {
            LayerKind::Other
        };

        Layer {
            name: name.to_string(),
            mapper: read(dm.join("name")),
            kind,
        }
    }

    fn describe(&self) -> String {
        let kind = match self.kind {
            LayerKind::Luks => Some("LUKS"),
            LayerKind::Lvm => Some("LVM"),
            LayerKind::Other => None,
        };

        match (&self.mapper, kind) {
            (Some(mapper), Some(kind)) => format!("{} ({mapper}, {kind})", self.name),
            (Some(mapper), None) => format!("{} ({mapper})", self.name),
            (None, Some(kind)) => format!("{} ({kind})", self.name),
            (None, None) => self.name.clone(),
        }
    }
}

/// The device-mapper layers stacked on a volume, bottom first, following the
/// first holder at each level. A partition with nothing on top is empty.
fn holder_stack(host: &Host, name: &str, path: &Path) -> Vec<Layer> {
    let mut stack = Vec::new();
    let mut holders = path.join("holders");
    let mut seen = vec![name.to_string()];

    while stack.len() < MAX_HOLDER_DEPTH {
        let Some(next) = list_dir(&holders).first().map(|p| file_name(p)) else {
            break;
        };
        if seen.contains(&next) {
            break;
        }

        holders = host.path(format!("/sys/class/block/{next}/holders"));
        seen.push(next.clone());
        stack.push(Layer::read(host, &next));
    }

    stack
}

/// Walk the other way, from a mounted device down to the partition under it,
/// for the "Root filesystem on" summary: `dm-0 (root, LUKS) on nvme0n1p2`.
fn describe_root(host: &Host, root: &Mount) -> String {
    let Some(device) = root.device.as_deref() else {
        return root.source.clone();
    };

    let mut chain = vec![device.to_string()];
    let mut current = device.to_string();
    for _ in 0..MAX_HOLDER_DEPTH {
        let slaves = host.list_dir(format!("/sys/class/block/{current}/slaves"));
        let Some(below) = slaves.first().map(|p| file_name(p)) else {
            break;
        };
        chain.push(below.clone());
        current = below;
    }

    let top = if device.starts_with("dm-") {
        Layer::read(host, device).describe()
    } else {
        device.to_string()
    };

    match chain.last() {
        Some(bottom) if chain.len() > 1 => format!("{top} on {bottom}"),
        _ => top,
    }
}

fn sectors_to_bytes(sectors: u64) -> u64 {
    // The kernel counts `size` in 512-byte units whatever the real sector
    // size. Saturating: a nonsense value must not wrap a huge disk into a
    // tiny one, or panic in a debug build.
    sectors.saturating_mul(512)
}

fn yes_no(flag: Option<&str>) -> &str {
    match flag {
        Some("0") => "no",
        Some("1") => "yes",
        Some(other) => other,
        None => "-",
    }
}

/// One line of `/proc/mounts`, already filtered to block-backed filesystems.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Mount {
    source: String,
    point: String,
    fstype: String,
    /// The kernel block device behind `source` (`dm-0` for
    /// `/dev/mapper/root`), resolved once here rather than per comparison.
    device: Option<String>,
}

fn mount_table(host: &Host) -> Vec<Mount> {
    let Some(content) = host.read("/proc/mounts") else {
        return Vec::new();
    };

    parse_mounts(&content)
        .into_iter()
        .map(|mut mount| {
            mount.device = std::fs::canonicalize(host.path(&mount.source))
                .ok()
                .map(|p| file_name(&p));
            mount
        })
        .collect()
}

fn parse_mounts(content: &str) -> Vec<Mount> {
    content
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let source = unescape(parts.next()?);
            let point = unescape(parts.next()?);
            let fstype = parts.next()?.to_string();

            Some(Mount {
                source,
                point,
                fstype,
                device: None,
            })
        })
        .filter(|m| m.source.starts_with("/dev/"))
        .collect()
}

/// `/proc/mounts` escapes whitespace and backslashes in paths as three-digit
/// octal. Decoding only `\040` left paths containing a literal backslash or
/// newline showing their escape sequence in the UI.
fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(byte) = bytes.get(i + 1..i + 4).and_then(octal_byte)
        {
            // The kernel escapes each byte of a multi-byte UTF-8 sequence.
            out.push(byte);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// Three octal digits as one byte, or `None` if they are not that.
fn octal_byte(digits: &[u8]) -> Option<u8> {
    let mut value: u16 = 0;
    for digit in digits {
        if !(b'0'..=b'7').contains(digit) {
            return None;
        }
        value = value * 8 + u16::from(digit - b'0');
    }

    u8::try_from(value).ok()
}

/// `label root, 1 device(s)` for the btrfs filesystem on `device`, read from
/// `/sys/fs/btrfs` rather than by running `btrfs filesystem show`, which
/// needs root to scan devices and used to fork on every refresh.
fn btrfs_summary(host: &Host, device: &str) -> Option<String> {
    host.list_dir("/sys/fs/btrfs").into_iter().find_map(|fs| {
        let devices: Vec<String> = list_dir(fs.join("devices"))
            .iter()
            .map(|p| file_name(p))
            .collect();
        if !devices.iter().any(|d| d == device) {
            return None;
        }
        let label = read(fs.join("label")).unwrap_or_else(|| "<none>".into());

        Some(format!("label {label}, {} device(s)", devices.len()))
    })
}

/// Whether the systemd timer that trims SSDs weekly is enabled. There is no
/// kernel switch for this; the old `/sys/module/fstrim` path does not exist.
fn fstrim_state(host: &Host) -> &'static str {
    let wants = "timers.target.wants/fstrim.timer";
    // The enablement is the symlink itself; whether its target resolves from
    // here is beside the point.
    let linked = |dir: &str| {
        host.path(format!("{dir}/{wants}"))
            .symlink_metadata()
            .is_ok()
    };

    if linked("/etc/systemd/system") || linked("/usr/lib/systemd/system") {
        "fstrim.timer enabled"
    } else {
        "fstrim.timer not enabled"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    fn text(rows: &[Row]) -> String {
        format!("{rows:?}")
    }

    // ---- small helpers -----------------------------------------------------

    #[test]
    fn yes_no_spells_out_the_boolean() {
        assert_eq!(yes_no(Some("0")), "no");
        assert_eq!(yes_no(Some("1")), "yes");
        assert_eq!(yes_no(Some("other")), "other");
        assert_eq!(yes_no(None), "-");
    }

    #[test]
    fn sectors_convert_at_512_bytes() {
        assert_eq!(sectors_to_bytes(0), 0);
        assert_eq!(sectors_to_bytes(1), 512);
        assert_eq!(sectors_to_bytes(2048), 1_048_576);
    }

    #[test]
    fn sector_conversion_saturates_instead_of_wrapping() {
        // A wrapped multiplication would report a huge disk as a tiny one.
        assert_eq!(sectors_to_bytes(u64::MAX), u64::MAX);
    }

    // ---- unescape -------------------------------------------------------

    #[test]
    fn unescape_decodes_the_octal_escapes_proc_mounts_uses() {
        assert_eq!(unescape("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape("/mnt/tab\\011here"), "/mnt/tab\there");
        // A backslash is escaped as a backslash.
        assert_eq!(unescape("/mnt/back\\134slash"), "/mnt/back\\slash");
        // é is the two bytes c3 a9. Pushing each as a char produced "Ã©".
        assert_eq!(unescape("/mnt/caf\\303\\251"), "/mnt/café");
        // An escape at the very end is still an escape.
        assert_eq!(unescape("/mnt/end\\040"), "/mnt/end ");
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
        assert_eq!(unescape("/a\\777"), "/a\\777", "does not fit in a byte");
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
    fn parse_mounts_keeps_only_block_backed_filesystems() {
        let mounts = parse_mounts(MOUNTS);
        let sources: Vec<&str> = mounts.iter().map(|m| m.source.as_str()).collect();

        assert_eq!(sources, ["/dev/nvme0n1p2", "/dev/mapper/root", "/dev/sda1"]);
    }

    #[test]
    fn parse_mounts_captures_the_type_and_unescapes_the_point() {
        let mounts = parse_mounts(MOUNTS);
        let disk = mounts
            .iter()
            .find(|m| m.source == "/dev/sda1")
            .expect("sda1 present");

        assert_eq!(disk.fstype, "vfat");
        assert_eq!(disk.point, "/mnt/my disk", "the escaped space is decoded");
    }

    #[test]
    fn parse_mounts_skips_lines_with_too_few_columns() {
        let mounts = parse_mounts("/dev/sda1\n/dev/sdb1 /mnt ext4\n");

        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].point, "/mnt");
    }

    #[test]
    fn parse_mounts_of_nothing_is_empty() {
        assert!(parse_mounts("").is_empty());
    }

    // ---- an Omarchy install: LUKS + btrfs on NVMe ----------------------------

    /// The default Omarchy layout: an EFI partition mounted at /boot, and the
    /// rest of the disk a LUKS container named `root` holding a btrfs with
    /// several subvolumes.
    fn omarchy_install() -> Fixture {
        let fx = Fixture::new();
        let disk = "sys/block/nvme0n1";
        fx.write(&format!("{disk}/size"), "2000409264\n");
        fx.write(&format!("{disk}/queue/rotational"), "0\n");
        fx.write(&format!("{disk}/device/model"), "Samsung SSD 990 PRO 1TB\n");
        fx.write(&format!("{disk}/removable"), "0\n");
        fx.write(&format!("{disk}/nvme0n1p1/partition"), "1\n");
        fx.write(&format!("{disk}/nvme0n1p1/size"), "4194304\n");
        fx.mkdir(&format!("{disk}/nvme0n1p1/holders"));
        fx.write(&format!("{disk}/nvme0n1p2/partition"), "2\n");
        fx.write(&format!("{disk}/nvme0n1p2/size"), "1996212224\n");
        fx.mkdir(&format!("{disk}/nvme0n1p2/holders/dm-0"));

        // dm-0 appears under /sys/block too, and must not become a disk.
        fx.write("sys/block/dm-0/size", "1996179456\n");
        fx.write("sys/class/block/dm-0/dm/name", "root\n");
        fx.write(
            "sys/class/block/dm-0/dm/uuid",
            "CRYPT-LUKS2-0123456789abcdef-root\n",
        );
        fx.mkdir("sys/class/block/dm-0/holders");
        fx.mkdir("sys/class/block/dm-0/slaves/nvme0n1p2");

        fx.write("dev/nvme0n1p1", "");
        fx.write("dev/dm-0", "");
        fx.symlink("dev/mapper/root", "../dm-0");
        fx.write(
            "proc/mounts",
            "/dev/mapper/root / btrfs rw,subvol=/@ 0 0\n\
             /dev/mapper/root /home btrfs rw,subvol=/@home 0 0\n\
             /dev/nvme0n1p1 /boot vfat rw 0 0\n\
             tmpfs /tmp tmpfs rw 0 0\n",
        );

        fx.write("sys/fs/btrfs/1234-abcd/label", "omarchy\n");
        fx.mkdir("sys/fs/btrfs/1234-abcd/devices/dm-0");
        fx.symlink(
            "etc/systemd/system/timers.target.wants/fstrim.timer",
            "/usr/lib/systemd/system/fstrim.timer",
        );
        fx
    }

    #[test]
    fn a_luks_root_is_credited_to_the_partition_under_it() {
        let fx = omarchy_install();
        let rows = rows(&fx.host());
        let text = text(&rows);

        let p2 = rows
            .iter()
            .find_map(|r| match r {
                Row::Field { label, value, .. } if label == "  nvme0n1p2" => {
                    Some(value.to_string())
                }
                _ => None,
            })
            .expect("a row for nvme0n1p2");
        assert!(p2.contains("dm-0 (root, LUKS)"), "{p2}");
        assert!(p2.contains("btrfs"), "{p2}");
        assert!(p2.contains("/, /home"), "{p2}");

        let p1 = rows
            .iter()
            .find_map(|r| match r {
                Row::Field { label, value, .. } if label == "  nvme0n1p1" => {
                    Some(value.to_string())
                }
                _ => None,
            })
            .expect("a row for nvme0n1p1");
        assert!(p1.contains("vfat  /boot"), "{p1}");

        assert!(!text.contains("dm-0  ·"), "dm-0 is not a disk: {text}");
    }

    #[test]
    fn the_totals_describe_the_root_stack_and_btrfs() {
        let fx = omarchy_install();
        let text = text(&rows(&fx.host()));

        assert!(text.contains("\"1\""), "one block device: {text}");
        assert!(text.contains("dm-0 (root, LUKS) on nvme0n1p2"), "{text}");
        assert!(text.contains("label omarchy, 1 device(s)"), "{text}");
        assert!(text.contains("fstrim.timer enabled"), "{text}");
    }

    #[test]
    fn the_disk_header_names_the_media() {
        let fx = omarchy_install();

        assert!(text(&rows(&fx.host())).contains("nvme0n1  ·  SSD / NVMe"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_mounted_filesystem_gets_a_usage_gauge() {
        // statvfs on the fixture's own directory stands in for the real
        // filesystem; all that matters is that the gauge row appears.
        let fx = omarchy_install();
        let rows = rows(&fx.host());

        assert!(rows.iter().any(|r| matches!(
            r,
            Row::Field { label, bar: Some(_), .. } if label == "    used"
        )));
    }

    #[test]
    fn an_unpartitioned_disk_is_its_own_volume() {
        let fx = Fixture::new();
        fx.write("sys/block/sdb/size", "2048\n");
        fx.write("sys/block/sdb/queue/rotational", "1\n");
        fx.write("dev/sdb", "");
        fx.write("proc/mounts", "/dev/sdb /mnt/backup ext4 rw 0 0\n");
        let text = text(&rows(&fx.host()));

        assert!(text.contains("sdb  ·  HDD"), "{text}");
        assert!(text.contains("ext4  /mnt/backup"), "{text}");
    }

    #[test]
    fn loop_and_zero_sized_devices_are_skipped() {
        let fx = Fixture::new();
        fx.write("sys/block/loop0/size", "2048\n");
        fx.write("sys/block/sdc/size", "0\n");

        assert_eq!(rows(&fx.host()), [Row::note("no block devices")]);
    }

    #[test]
    fn zram_is_a_ram_disk_not_an_ssd() {
        let fx = Fixture::new();
        fx.write("sys/block/zram0/size", "8388608\n");

        assert!(text(&rows(&fx.host())).contains("zram0  ·  RAM disk"));
    }

    #[test]
    fn a_holder_cycle_does_not_loop_forever() {
        let fx = Fixture::new();
        fx.mkdir("sys/block/sda/sda1/holders/dm-0");
        fx.mkdir("sys/class/block/dm-0/holders/dm-1");
        fx.mkdir("sys/class/block/dm-1/holders/dm-0");
        let host = fx.host();

        let stack = holder_stack(&host, "sda1", &host.path("/sys/block/sda/sda1"));
        assert_eq!(stack.len(), 2);
    }

    #[test]
    fn btrfs_summary_is_none_for_a_device_it_does_not_hold() {
        let fx = omarchy_install();

        assert_eq!(btrfs_summary(&fx.host(), "sda1"), None);
    }

    #[test]
    fn usage_row_carries_df_style_figures() {
        let row = usage_row(FsUsage::from_blocks(4096, 100, 40, 30));

        assert!(format!("{row:?}").contains("67%"), "{row:?}");
    }
}
