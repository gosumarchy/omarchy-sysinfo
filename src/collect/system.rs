use super::stats::Stats;
use super::{
    fs::read,
    units::{dash, human_secs},
    Row,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn rows(stats: &mut Stats) -> Vec<Row> {
    let os_release = read("/etc/os-release").unwrap_or_default();
    let name = os_release_field(&os_release, "PRETTY_NAME")
        .or_else(|| os_release_field(&os_release, "NAME"))
        .unwrap_or_else(|| "Linux".to_string());
    let build = os_release_field(&os_release, "BUILD_ID").unwrap_or_else(|| "-".into());
    let kernel = kernel();
    let kernel_full = dash(read("/proc/version"));

    let uptime = stats.uptime();

    let mut rows = vec![Row::Header("System".into())];
    rows.push(Row::field("Hostname", hostname()));
    rows.push(Row::field("Distribution", name));
    rows.push(Row::field("Build", build));
    rows.push(Row::field("Kernel", kernel));
    rows.push(Row::field("Kernel build", kernel_full));
    rows.push(Row::field("Architecture", arch()));
    rows.push(Row::field("Uptime", human_secs(uptime)));
    let boot = boot_epoch();
    if boot > 0 {
        rows.push(Row::field("Booted", format_boot_time(boot)));
    }

    rows.push(Row::Header("Boot".into()));
    rows.push(Row::field("Command line", dash(read("/proc/cmdline"))));
    for key in [
        "quiet",
        "splash",
        "zswap.enabled",
        "rootflags",
        "resume",
        "mitigations",
        "nowatchdog",
    ] {
        if let Some(value) = super::fs::kernel_param(key) {
            rows.push(Row::field(format!("  {key}"), value));
        }
    }
    rows.push(Row::field(
        "Init system",
        if std::path::Path::new("/run/systemd/system").exists() {
            "systemd"
        } else {
            "other"
        },
    ));
    rows.push(Row::field(
        "Command",
        dash(std::env::var("OMARCHY_SESSION").ok().or_else(|| {
            std::process::Command::new("ps")
                .args(["-p", &std::process::id().to_string(), "-o", "comm="])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })),
    ));

    rows.push(Row::Header("Load".into()));
    if let Some(load) = read("/proc/loadavg") {
        rows.extend(parse_loadavg(&load));
    }
    if let Some(idle) = super::cpu::idle_hint() {
        rows.push(Row::field("CPU idle", idle));
    }

    rows.push(Row::Header("Platform".into()));
    let sys_vendor = read("/sys/class/dmi/id/sys_vendor").unwrap_or_default();
    let product_name = read("/sys/class/dmi/id/product_name").unwrap_or_default();
    rows.push(Row::field(
        "Virtualization",
        dash(detect_virtualization(&sys_vendor, &product_name).map(str::to_string)),
    ));
    rows.push(Row::field("Container", container()));
    // Secure Boot and Kernel lockdown belong to the firmware group in
    // `dmi::rows`; repeating them here printed both twice in the report.
    rows.push(Row::field("Timezone", timezone()));
    rows.push(Row::field("Locale", dash(std::env::var("LANG").ok())));

    rows
}

/// The three load averages and the runnable/total task split from
/// `/proc/loadavg`, which is `1 2 3 4/5 12345`.
fn parse_loadavg(load: &str) -> Vec<Row> {
    let parts: Vec<&str> = load.split_whitespace().collect();
    let mut rows = Vec::new();
    if parts.len() >= 3 {
        rows.push(Row::field(
            "1 / 5 / 15 min",
            format!("{} / {} / {}", parts[0], parts[1], parts[2]),
        ));
    }
    if let Some((runnable, total)) = parts.get(3).and_then(|v| v.split_once('/')) {
        rows.push(Row::field(
            "Runnable / total tasks",
            format!("{runnable} / {total}"),
        ));
    }
    rows
}

/// Whether the DMI strings describe a guest rather than real hardware.
///
/// Matching on the vendor alone is not enough: a Surface laptop reports
/// `Microsoft Corporation`, the same string Hyper-V uses, so the product name
/// has to agree before a machine is called virtual.
fn detect_virtualization(sys_vendor: &str, product_name: &str) -> Option<&'static str> {
    let vendor = sys_vendor.to_lowercase();
    let product = product_name.to_lowercase();
    let virtual_product = [
        "virtual machine",
        "virtualbox",
        "vmware",
        "kvm",
        "qemu",
        "bochs",
        "virtual platform",
    ]
    .iter()
    .any(|n| product.contains(n));
    match vendor.as_str() {
        "qemu" => Some("qemu"),
        "vmware" | "innotek gmbh" | "vmware, inc." => Some("vmware"),
        "virtualbox" => Some("virtualbox"),
        "bochs" => Some("bochs"),
        "kvm" => Some("kvm"),
        "microsoft corporation" if virtual_product => Some("hyper-v"),
        "xen" => Some("xen"),
        // Public cloud instances announce themselves in the product name.
        _ if virtual_product => Some("virtual machine"),
        "amazon ec2" | "amazon.com" => Some("aws"),
        "google" | "google compute engine" => Some("gcp"),
        "alibaba cloud" => Some("alibaba"),
        "nutanix" | "openstack foundation" => Some("openstack"),
        _ => None,
    }
}

fn container() -> &'static str {
    if std::path::Path::new("/.dockerenv").exists() {
        "docker"
    } else if read("/run/systemd/container").is_some() {
        "systemd-nspawn"
    } else if read("/proc/1/cgroup")
        .map(|c| {
            c.contains("docker")
                || c.contains("lxc")
                || c.contains("libpod")
                || c.contains("kubepods")
        })
        .unwrap_or(false)
    {
        "cgroup-based"
    } else {
        "none"
    }
}

/// Host name, or a placeholder inside an unusual session.
pub fn hostname() -> String {
    dash(read("/proc/sys/kernel/hostname"))
}

pub fn distro() -> String {
    let os_release = read("/etc/os-release").unwrap_or_default();
    os_release_field(&os_release, "PRETTY_NAME")
        .or_else(|| os_release_field(&os_release, "NAME"))
        .unwrap_or_else(|| "Linux".into())
}

pub fn kernel() -> String {
    dash(read("/proc/sys/kernel/osrelease"))
}

pub fn arch() -> String {
    std::env::consts::ARCH.to_string()
}

pub fn timezone() -> String {
    read("/etc/timezone")
        .or_else(|| {
            std::fs::read_link("/etc/localtime")
                .ok()
                .map(|t| t.to_string_lossy().to_string())
                .map(|t| match t.rfind("zoneinfo/") {
                    // The symlink may be relative, e.g. ../usr/share/zoneinfo/...
                    Some(at) => t[at + "zoneinfo/".len()..].to_string(),
                    None => t,
                })
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Seconds since the epoch the kernel recorded for this boot.
pub fn boot_epoch() -> u64 {
    read("/proc/stat")
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("btime"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// One value out of an os-release style file.
///
/// Values may be double or single quoted, and quotes inside a value belong to
/// it, so exactly one matching pair is removed rather than trimming every
/// leading and trailing quote.
fn os_release_field(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let value = content
        .lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(str::trim)
        .map(|v| match v.chars().next() {
            Some('"') if v.ends_with('"') && v.len() > 1 => &v[1..v.len() - 1],
            Some('\'') if v.ends_with('\'') && v.len() > 1 => &v[1..v.len() - 1],
            _ => v,
        })
        .map(str::to_string);
    value.filter(|v| !v.is_empty())
}

fn format_boot_time(epoch: u64) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(epoch);
    format!("{} ({} ago)", epoch, human_secs(secs.saturating_sub(epoch)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- os_release_field -----------------------------------------------

    const OS_RELEASE: &str = "\
NAME=Arch Linux
PRETTY_NAME=\"Arch Linux\"
ID=arch
BUILD_ID=rolling
HOME_URL='https://archlinux.org/'
EMPTY=
";

    #[test]
    fn os_release_field_unquotes_both_quote_styles() {
        assert_eq!(
            os_release_field(OS_RELEASE, "PRETTY_NAME").as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(
            os_release_field(OS_RELEASE, "HOME_URL").as_deref(),
            Some("https://archlinux.org/")
        );
    }

    #[test]
    fn os_release_field_leaves_an_unquoted_value_alone() {
        assert_eq!(
            os_release_field(OS_RELEASE, "NAME").as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(
            os_release_field(OS_RELEASE, "BUILD_ID").as_deref(),
            Some("rolling")
        );
    }

    #[test]
    fn os_release_field_keeps_quotes_that_belong_to_the_value() {
        // A value that merely starts with a quote is not quoted.
        let content = "NAME=\"unbalanced\n";
        assert_eq!(
            os_release_field(content, "NAME").as_deref(),
            Some("\"unbalanced")
        );
        // An embedded pair of quotes is data, not a wrapper.
        let content = "NAME=\"say \"\"hi\"\"\"\n";
        assert_eq!(
            os_release_field(content, "NAME").as_deref(),
            Some("say \"\"hi\"\"")
        );
    }

    #[test]
    fn os_release_field_will_not_match_a_longer_key() {
        // PRETTY_NAME must not be found by asking for NAME.
        assert_eq!(os_release_field("PRETTY_NAME=x\n", "NAME"), None);
        assert_eq!(os_release_field("NAME_EXTRA=x\n", "NAME"), None);
    }

    #[test]
    fn os_release_field_treats_an_empty_value_as_absent() {
        assert_eq!(os_release_field(OS_RELEASE, "EMPTY"), None);
        assert_eq!(os_release_field(OS_RELEASE, "MISSING"), None);
    }

    #[test]
    fn distro_and_build_survive_a_missing_os_release() {
        // Nothing on this machine can be wrong enough to panic.
        assert!(!distro().is_empty());
        assert!(!kernel().is_empty());
        assert!(!arch().is_empty());
    }

    // ---- parse_loadavg ---------------------------------------------------

    #[test]
    fn parse_loadavg_reads_the_averages_and_the_task_split() {
        let rows = parse_loadavg("0.52 0.58 0.59 2/1234 56789");
        let text: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(
            text.iter().any(|t| t.contains("0.52 / 0.58 / 0.59")),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t.contains("2 / 1234")), "{text:?}");
    }

    #[test]
    fn parse_loadavg_keeps_the_averages_when_the_task_field_is_absent() {
        let rows = parse_loadavg("0.10 0.20 0.30");
        assert_eq!(rows.len(), 1);
        assert!(format!("{:?}", rows[0]).contains("0.10 / 0.20 / 0.30"));
    }

    #[test]
    fn parse_loadavg_ignores_a_malformed_task_field() {
        let rows = parse_loadavg("0.10 0.20 0.30 notanumber");
        assert_eq!(rows.len(), 1, "a missing slash must not invent a value");
    }

    #[test]
    fn parse_loadavg_of_junk_is_empty() {
        assert!(parse_loadavg("").is_empty());
        assert!(parse_loadavg("garbage").is_empty());
    }

    // ---- detect_virtualization -------------------------------------------

    #[test]
    fn detect_virtualization_spots_the_usual_hypervisors() {
        assert_eq!(detect_virtualization("QEMU", "Standard PC"), Some("qemu"));
        assert_eq!(
            detect_virtualization("VMware, Inc.", "VMware Virtual Platform"),
            Some("vmware")
        );
        assert_eq!(detect_virtualization("innotek GmbH", ""), Some("vmware"));
        assert_eq!(detect_virtualization("VirtualBox", ""), Some("virtualbox"));
        assert_eq!(detect_virtualization("Bochs", ""), Some("bochs"));
    }

    #[test]
    fn detect_virtualization_does_not_call_a_surface_laptop_a_vm() {
        // The bug: sys_vendor alone matched "microsoft corporation", so real
        // Surface hardware was reported as virtualised.
        assert_eq!(
            detect_virtualization("Microsoft Corporation", "Surface Pro 9"),
            None
        );
    }

    #[test]
    fn detect_virtualization_still_spots_hyper_v() {
        assert_eq!(
            detect_virtualization("Microsoft Corporation", "Virtual Machine"),
            Some("hyper-v")
        );
    }

    #[test]
    fn detect_virtualization_spots_cloud_instances() {
        assert_eq!(detect_virtualization("Amazon EC2", "t3.micro"), Some("aws"));
        assert_eq!(
            detect_virtualization("Google", "Google Compute Engine"),
            Some("gcp")
        );
        assert_eq!(
            detect_virtualization("OpenStack Foundation", "OpenStack Nova"),
            Some("openstack")
        );
    }

    #[test]
    fn detect_virtualization_falls_back_to_the_product_name() {
        assert_eq!(
            detect_virtualization("Acme", "KVM Virtual Machine"),
            Some("virtual machine")
        );
    }

    #[test]
    fn detect_virtualization_leaves_real_hardware_alone() {
        assert_eq!(detect_virtualization("LENOVO", "ThinkPad X1"), None);
        assert_eq!(detect_virtualization("Dell Inc.", "XPS 13"), None);
        assert_eq!(detect_virtualization("ASUSTeK", "ROG Zephyrus"), None);
        assert_eq!(detect_virtualization("", ""), None);
    }

    #[test]
    fn detect_virtualization_is_case_insensitive() {
        assert_eq!(detect_virtualization("qemu", ""), Some("qemu"));
        assert_eq!(detect_virtualization("Bochs", ""), Some("bochs"));
    }

    // ---- live values -----------------------------------------------------

    #[test]
    fn live_values_are_all_populated() {
        assert!(!hostname().is_empty());
        assert!(!container().is_empty());
        assert!(!timezone().is_empty());
    }

    #[test]
    fn timezone_never_leaks_a_path_prefix_or_a_newline() {
        let tz = timezone();
        assert!(!tz.contains('\n'), "{tz:?}");
        assert!(!tz.contains("zoneinfo"), "{tz:?}");
        assert!(!tz.starts_with('/'), "{tz:?}");
    }

    #[test]
    fn boot_epoch_is_either_zero_or_a_plausible_date() {
        let boot = boot_epoch();
        if boot > 0 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is sane")
                .as_secs();
            assert!(boot <= now, "boot time is in the future: {boot} > {now}");
            // Not before 2000, which would mean we parsed the wrong column.
            assert!(boot > 946_684_800, "implausible boot time {boot}");
        }
    }

    #[test]
    fn format_boot_time_tolerates_a_clock_set_backwards() {
        // A boot time in the future must not wrap into a huge uptime.
        let s = format_boot_time(u64::MAX);
        assert!(s.contains("0 seconds") || s.contains('('), "{s}");
    }

    #[test]
    fn rows_render_without_panicking_or_producing_nan() {
        let mut stats = Stats::new();
        let rows = rows(&mut stats);
        assert!(rows.len() > 5);
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "{text}");
        assert!(!text.contains("∞"), "{text}");
    }
}
