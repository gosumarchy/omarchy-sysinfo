//! The operating system: distribution, kernel, boot, and what it runs on.

use std::time::{SystemTime, UNIX_EPOCH};

use super::stats::Stats;
use super::units::{dash, human_secs, utc_timestamp};
use super::{Host, Row};

pub(crate) fn rows(host: &Host, stats: &Stats) -> Vec<Row> {
    let os_release = host.read("/etc/os-release").unwrap_or_default();

    let mut rows = vec![Row::header("System")];
    // Hostnames are often a person's name (`georgios-thinkpad`).
    rows.push(Row::identifier("Hostname", hostname(host)));
    rows.push(Row::field("Distribution", distro_from(&os_release)));
    rows.push(Row::field(
        "Build",
        os_release_field(&os_release, "BUILD_ID").unwrap_or_else(|| "-".into()),
    ));
    rows.push(Row::field("Kernel", kernel(host)));
    rows.push(Row::field("Kernel build", dash(host.read("/proc/version"))));
    rows.push(Row::field("Architecture", std::env::consts::ARCH));
    rows.push(Row::field("Uptime", human_secs(stats.uptime())));
    let boot = boot_epoch(host);
    if boot > 0 {
        rows.push(Row::field("Booted", format_boot_time(boot, now_epoch())));
    }

    rows.push(Row::header("Boot"));
    // The command line names the root and resume devices, usually by UUID
    // (`cryptdevice=PARTUUID=...`, `resume=UUID=...`).
    rows.push(Row::with_embedded_identifiers(
        "Command line",
        dash(host.cmdline()),
    ));
    for key in [
        "quiet",
        "splash",
        "zswap.enabled",
        "rootflags",
        "resume",
        "mitigations",
        "nowatchdog",
    ] {
        if let Some(value) = host.kernel_param(key) {
            rows.push(Row::with_embedded_identifiers(format!("  {key}"), value));
        }
    }
    rows.push(Row::field(
        "Init system",
        if host.exists("/run/systemd/system") {
            "systemd"
        } else {
            "other"
        },
    ));

    // The load average lives in the CPU section, next to the per-core gauges;
    // repeating it here printed the same three numbers twice in the report.

    rows.push(Row::header("Platform"));
    let sys_vendor = host
        .read("/sys/class/dmi/id/sys_vendor")
        .unwrap_or_default();
    let product_name = host
        .read("/sys/class/dmi/id/product_name")
        .unwrap_or_default();
    rows.push(Row::field(
        "Virtualization",
        detect_virtualization(&sys_vendor, &product_name).unwrap_or("-"),
    ));
    rows.push(Row::field("Container", container(host)));
    // Secure Boot and Kernel lockdown belong to the firmware group in
    // `dmi::rows`; repeating them here printed both twice in the report.
    rows.push(Row::field("Timezone", timezone(host)));
    rows.push(Row::field("Locale", dash(std::env::var("LANG").ok())));

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
        // Every other vendor string is real hardware.
        _ => None,
    }
}

fn container(host: &Host) -> &'static str {
    if host.exists("/.dockerenv") {
        return "docker";
    }
    if host.read("/run/systemd/container").is_some() {
        return "systemd-nspawn";
    }

    let cgroup = host.read("/proc/1/cgroup").unwrap_or_default();
    if ["docker", "lxc", "libpod", "kubepods"]
        .iter()
        .any(|marker| cgroup.contains(marker))
    {
        "cgroup-based"
    } else {
        "none"
    }
}

/// Host name, or a placeholder inside an unusual session.
pub(crate) fn hostname(host: &Host) -> String {
    dash(host.read("/proc/sys/kernel/hostname"))
}

pub(crate) fn distro(host: &Host) -> String {
    distro_from(&host.read("/etc/os-release").unwrap_or_default())
}

fn distro_from(os_release: &str) -> String {
    os_release_field(os_release, "PRETTY_NAME")
        .or_else(|| os_release_field(os_release, "NAME"))
        .unwrap_or_else(|| "Linux".into())
}

pub(crate) fn kernel(host: &Host) -> String {
    dash(host.read("/proc/sys/kernel/osrelease"))
}

pub(crate) fn timezone(host: &Host) -> String {
    host.read("/etc/timezone")
        .or_else(|| {
            let target = std::fs::read_link(host.path("/etc/localtime")).ok()?;

            Some(zone_from_link(&target.to_string_lossy()))
        })
        .unwrap_or_else(|| "unknown".into())
}

/// `/etc/localtime` points into the zoneinfo tree, possibly relatively
/// (`../usr/share/zoneinfo/Europe/Oslo`); the zone is what follows it.
fn zone_from_link(target: &str) -> String {
    const MARKER: &str = "zoneinfo/";

    match target.rfind(MARKER) {
        Some(at) => target[at + MARKER.len()..].to_string(),
        None => target.to_string(),
    }
}

/// Seconds since the epoch the kernel recorded for this boot, or 0.
pub(crate) fn boot_epoch(host: &Host) -> u64 {
    host.read("/proc/stat")
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("btime "))
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(0)
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
        .map(|v| {
            let quoted = v.len() > 1
                && ((v.starts_with('"') && v.ends_with('"'))
                    || (v.starts_with('\'') && v.ends_with('\'')));
            if quoted { &v[1..v.len() - 1] } else { v }
        })
        .map(str::to_string);

    value.filter(|v| !v.is_empty())
}

/// `2026-09-28 14:05 UTC (3h 2m 1s ago)`.
///
/// A clock set before the boot time reads as zero ago rather than wrapping.
fn format_boot_time(epoch: u64, now: u64) -> String {
    format!(
        "{} ({} ago)",
        utc_timestamp(epoch),
        human_secs(now.saturating_sub(epoch))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

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
    fn distro_falls_back_to_name_then_linux() {
        assert_eq!(distro_from(OS_RELEASE), "Arch Linux");
        assert_eq!(distro_from("NAME=Omarchy\n"), "Omarchy");
        assert_eq!(distro_from(""), "Linux");
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

    // ---- against a fixture -------------------------------------------------

    #[test]
    fn host_values_come_from_the_host() {
        let fx = Fixture::new();
        fx.write("proc/sys/kernel/hostname", "omarchy-box\n");
        fx.write("proc/sys/kernel/osrelease", "6.16.8-arch1-1\n");
        fx.symlink("etc/localtime", "../usr/share/zoneinfo/Europe/Oslo");
        let host = fx.host();

        assert_eq!(hostname(&host), "omarchy-box");
        assert_eq!(kernel(&host), "6.16.8-arch1-1");
        assert_eq!(timezone(&host), "Europe/Oslo");
        assert_eq!(container(&host), "none");
    }

    #[test]
    fn missing_values_have_placeholders() {
        let fx = Fixture::new();
        let host = fx.host();

        assert_eq!(hostname(&host), "-");
        assert_eq!(timezone(&host), "unknown");
        assert_eq!(boot_epoch(&host), 0);
    }

    #[test]
    fn zone_from_link_strips_the_zoneinfo_prefix() {
        assert_eq!(zone_from_link("/usr/share/zoneinfo/UTC"), "UTC");
        assert_eq!(
            zone_from_link("../usr/share/zoneinfo/America/New_York"),
            "America/New_York"
        );
        assert_eq!(zone_from_link("/etc/custom"), "/etc/custom");
    }

    #[test]
    fn boot_epoch_reads_btime_and_nothing_that_merely_starts_with_it() {
        let fx = Fixture::new();
        fx.write("proc/stat", "cpu 1 1 1 1\nbtimex 5\nbtime 1700000000\n");

        assert_eq!(boot_epoch(&fx.host()), 1_700_000_000);
    }

    #[test]
    fn a_container_is_recognised_by_its_cgroup() {
        let fx = Fixture::new();
        fx.write("proc/1/cgroup", "0::/kubepods/besteffort/pod1\n");

        assert_eq!(container(&fx.host()), "cgroup-based");
    }

    #[test]
    fn boot_time_is_a_date_and_an_age() {
        assert_eq!(
            format_boot_time(1_790_604_300, 1_790_604_300 + 3_661),
            "2026-09-28 14:05 UTC (1h 1m 1s ago)"
        );
    }

    #[test]
    fn boot_time_tolerates_a_clock_set_backwards() {
        // A boot time in the future must not wrap into a huge uptime.
        assert!(format_boot_time(2_000, 1_000).ends_with("(0m 0s ago)"));
    }

    #[test]
    fn the_hostname_is_an_identifier() {
        let fx = Fixture::new();
        fx.write("proc/sys/kernel/hostname", "georgios-thinkpad\n");
        let host = fx.host();

        assert!(
            rows(&host, &Stats::new(&host))
                .contains(&Row::identifier("Hostname", "georgios-thinkpad"))
        );
    }

    #[test]
    fn rows_render_without_panicking_on_an_empty_machine() {
        let fx = Fixture::new();
        let host = fx.host();
        let rows = rows(&host, &Stats::new(&host));

        assert!(rows.len() > 5);
    }
}
