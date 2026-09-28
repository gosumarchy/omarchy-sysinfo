//! The processor: what it is, how it is laid out, and how busy it is.

use std::collections::HashSet;

use super::fs::{list_dir, read_u64};
use super::stats::Stats;
use super::units::{human_mhz, human_secs};
use super::{Host, Row};

/// What `/proc/cpuinfo` says about the chip. It does not change while the
/// process runs, so it is parsed once rather than on every refresh.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CpuInfo {
    brand: String,
    vendor: Option<String>,
    microcode: Option<String>,
    flags: String,
}

impl CpuInfo {
    pub(crate) fn read(host: &Host) -> CpuInfo {
        let cpuinfo = host.read("/proc/cpuinfo").unwrap_or_default();
        // The sysfs file is the authoritative revision and follows a late
        // microcode load; cpuinfo is the fallback for kernels without it.
        let microcode = host
            .read("/sys/devices/system/cpu/cpu0/microcode/version")
            .or_else(|| line_value(&cpuinfo, "microcode"))
            .map(|m| with_hex_prefix(&m));

        CpuInfo {
            brand: parse_brand(&cpuinfo),
            vendor: line_value(&cpuinfo, "vendor_id"),
            microcode,
            flags: parse_flags(&cpuinfo),
        }
    }

    /// The name of the socket, e.g. `12th Gen Intel(R) Core(TM) i7-1260P`.
    pub(crate) fn brand(&self) -> &str {
        &self.brand
    }
}

/// Both sysfs and cpuinfo already print `0x...`; only add the prefix when a
/// source did not, so the row never reads `0x0x...`.
fn with_hex_prefix(value: &str) -> String {
    if value.starts_with("0x") {
        value.to_string()
    } else {
        format!("0x{value}")
    }
}

/// `model name` on x86, but ARM boards say `Hardware`, `Processor` or `Model`.
fn parse_brand(cpuinfo: &str) -> String {
    ["model name", "Model name", "Hardware", "Processor", "Model"]
        .into_iter()
        .filter_map(|key| line_value(cpuinfo, key))
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown CPU".to_string())
}

pub(crate) fn rows(host: &Host, stats: &Stats, info: &CpuInfo) -> Vec<Row> {
    let mut rows = vec![
        Row::field("Model", info.brand.as_str()),
        Row::field("Vendor", info.vendor.as_deref().unwrap_or("-")),
        Row::field("Microcode", info.microcode.as_deref().unwrap_or("-")),
    ];

    let cores = stats.cores();
    if cores.is_empty() {
        rows.push(Row::note("no per-core counters in /proc/stat"));

        return rows;
    }

    rows.extend(topology_rows(host, cores.len(), info));
    rows.extend(load_rows(host, stats));

    let boot_epoch = super::system::boot_epoch(host);
    if boot_epoch > 0 {
        rows.push(Row::header("Time"));
        rows.push(Row::field("Since boot", human_secs(stats.uptime())));
        rows.push(Row::field(
            "Booted at",
            super::units::utc_timestamp(boot_epoch),
        ));
    }

    rows
}

fn topology_rows(host: &Host, logical: usize, info: &CpuInfo) -> Vec<Row> {
    let cpufreq = "/sys/devices/system/cpu/cpu0/cpufreq";
    let mut rows = vec![Row::header("Topology")];

    let cores = match physical_cores(host) {
        Some(physical) => format!("{physical} / {logical}"),
        None => format!("? / {logical} (no topology in sysfs)"),
    };
    rows.push(Row::field("Cores / threads", cores));

    if let Some(governor) = host.read(format!("{cpufreq}/scaling_governor")) {
        rows.push(Row::field("Frequency governor", governor));
    }

    let khz_to_mhz = |file: &str| {
        host.read_u64(format!("{cpufreq}/{file}"))
            .map(|k| k / 1000)
            .filter(|mhz| *mhz > 0)
    };
    match (
        khz_to_mhz("cpuinfo_min_freq"),
        khz_to_mhz("cpuinfo_max_freq"),
    ) {
        (Some(min), Some(max)) => rows.push(Row::field(
            "Frequency range",
            format!("{} – {}", human_mhz(min), human_mhz(max)),
        )),
        (None, Some(max)) => rows.push(Row::field("Frequency range", human_mhz(max))),
        (_, None) => {}
    }

    if let Some(online) = host.read("/sys/devices/system/cpu/online") {
        // The file is a CPU list (`0-15`, `0-3,8-11`), not a count. Parsing it
        // as a number meant the row never appeared.
        let value = match count_cpu_list(&online) {
            Some(n) => format!("{n} · {online}"),
            None => online,
        };
        rows.push(Row::field("Online CPUs", value));
    }
    rows.push(Row::field("Flags", info.flags.as_str()));

    rows
}

/// The load average, then one gauge per logical CPU. One read is not a
/// delta, so say so rather than print a confident zero.
fn load_rows(host: &Host, stats: &Stats) -> Vec<Row> {
    let mut rows = vec![Row::header("Load")];

    if !stats.primed() {
        rows.push(Row::note("sampling, refresh in a moment"));

        return rows;
    }

    let avg = stats.cpu_usage();
    let load = stats.load();
    rows.push(Row::field_with("Total", format!("{avg:.0}%"), avg / 100.0));
    rows.push(Row::field(
        "Average",
        format!("{:.2}  {:.2}  {:.2}", load.one, load.five, load.fifteen),
    ));
    rows.push(Row::field(
        "Threads",
        format!("{} runnable / {} threads", load.runnable, load.threads),
    ));
    if let Some(idle) = stats.idle_since_boot() {
        rows.push(Row::field("Idle since boot", format!("{idle:.1}%")));
    }

    rows.push(Row::header("Per-core load"));
    for core in stats.cores() {
        // Keyed by the core's own name: with a CPU offline, the position in
        // the list is not the CPU number.
        let mhz = host
            .read_u64(format!(
                "/sys/devices/system/cpu/{}/cpufreq/scaling_cur_freq",
                core.name
            ))
            .map(|k| k / 1000)
            .filter(|mhz| *mhz > 0);
        let usage = core.usage;
        let value = match mhz {
            Some(mhz) => format!("{usage:>3.0}%  {}", human_mhz(mhz)),
            None => format!("{usage:>3.0}%"),
        };
        rows.push(Row::field_with(core.name.as_str(), value, usage / 100.0));
    }

    rows
}

/// Physical cores from the topology exposed in sysfs, if the kernel shares it.
fn physical_cores(host: &Host) -> Option<usize> {
    let mut ids = HashSet::new();

    for entry in list_dir(host.path("/sys/devices/system/cpu")) {
        let name = super::fs::file_name(&entry);
        let is_cpu = name
            .strip_prefix("cpu")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if !is_cpu {
            continue;
        }
        // A CPU that is offline, or on a kernel without topology, has no such
        // file. Bailing out of the whole function on the first one threw away
        // the topology for every other CPU and silently fell back to a guess.
        let (Some(package), Some(core)) = (
            read_u64(entry.join("topology/physical_package_id")),
            read_u64(entry.join("topology/core_id")),
        ) else {
            continue;
        };
        ids.insert((package, core));
    }

    (!ids.is_empty()).then_some(ids.len())
}

fn line_value(cpuinfo: &str, key: &str) -> Option<String> {
    cpuinfo.lines().find_map(|l| {
        // The key must be followed by optional space and then a colon, so that
        // a longer key sharing the prefix ("model name extra") cannot match.
        let rest = l.strip_prefix(key)?.trim_start();
        let value = rest.strip_prefix(':')?;

        Some(value.trim().to_string())
    })
}

/// How many CPUs a sysfs list names. `0-15` is 16, `0-3,8-11` is 8.
fn count_cpu_list(list: &str) -> Option<u64> {
    let mut count = 0u64;
    for part in list.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = start.parse::<u64>().ok()?;
            let end = end.parse::<u64>().ok()?;
            if end < start {
                return None;
            }
            count = count.saturating_add(end - start + 1);
        } else {
            part.parse::<u64>().ok()?;
            count = count.saturating_add(1);
        }
    }

    (count > 0).then_some(count)
}

/// Group the ~100 CPU flags into the few that explain what this chip can do.
fn parse_flags(cpuinfo: &str) -> String {
    let flags: Vec<&str> = cpuinfo
        .lines()
        .find(|l| l.starts_with("flags"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, f)| f.split_whitespace().collect())
        .unwrap_or_default();
    if flags.is_empty() {
        return "-".to_string();
    }

    let has = |needle: &str| flags.contains(&needle);
    // The virtualisation mark is reported as vmx/svm because that is what the
    // flag is actually called, and smep is smep -- it says nothing about
    // simultaneous multithreading.
    let marks: Vec<&str> = [
        ("avx2", "avx2"),
        ("avx512", "avx512f"),
        ("fma", "fma"),
        ("aes", "aes"),
        ("sha_ni", "sha_ni"),
        ("vt_x", "vmx"),
        ("amd_v", "svm"),
        ("smep", "smep"),
        ("hypervisor", "hypervisor"),
    ]
    .into_iter()
    .filter(|(_, flag)| has(flag))
    .map(|(name, _)| name)
    .collect();

    format!("{}  (+{} more)", marks.join(" "), flags.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    const X86_CPUINFO: &str = "\
processor\t: 0
vendor_id\t: GenuineIntel
cpu family\t: 6
model name\t: 12th Gen Intel(R) Core(TM) i7-1260P
stepping\t: 1
flags\t\t: fpu vme de pse tsc msr pae mce cx8 apic sep aes avx2 avx512f fma sha_ni vmx smep hypervisor
";

    // ---- line_value ------------------------------------------------------

    #[test]
    fn line_value_finds_a_tab_separated_value() {
        let info = X86_CPUINFO;
        assert_eq!(
            line_value(info, "model name").as_deref(),
            Some("12th Gen Intel(R) Core(TM) i7-1260P")
        );
        assert_eq!(
            line_value(info, "vendor_id").as_deref(),
            Some("GenuineIntel")
        );
    }

    #[test]
    fn line_value_will_not_match_a_longer_key_that_shares_a_prefix() {
        // "model name" must not match a hypothetical "model name extra".
        let info = "model name extra\t: wrong\nmodel name\t: right\n";
        assert_eq!(line_value(info, "model name").as_deref(), Some("right"));
    }

    #[test]
    fn line_value_returns_none_for_a_missing_key() {
        assert_eq!(line_value(X86_CPUINFO, "nonesuch"), None);
        assert_eq!(line_value("", "model name"), None);
    }

    #[test]
    fn line_value_ignores_a_key_with_no_colon() {
        assert_eq!(
            line_value("model name\tno colon here\n", "model name"),
            None
        );
    }

    // ---- parse_brand -----------------------------------------------------

    #[test]
    fn parse_brand_reads_the_model_name() {
        assert_eq!(
            parse_brand(X86_CPUINFO),
            "12th Gen Intel(R) Core(TM) i7-1260P"
        );
    }

    #[test]
    fn parse_brand_falls_back_to_the_arm_keys() {
        assert_eq!(
            parse_brand("Hardware\t: Rockchip RK3588\n"),
            "Rockchip RK3588"
        );
        assert_eq!(parse_brand("Processor\t: ARMv7 rev 5\n"), "ARMv7 rev 5");
        // Apple and some ARM boards capitalise the key.
        assert_eq!(parse_brand("Model name\t: Apple M2\n"), "Apple M2");
    }

    #[test]
    fn parse_brand_of_junk_is_a_placeholder() {
        assert_eq!(parse_brand(""), "unknown CPU");
        assert_eq!(parse_brand("nothing useful here"), "unknown CPU");
        // An empty value must not produce an empty brand.
        assert_eq!(parse_brand("model name\t: \n"), "unknown CPU");
    }

    // ---- parse_flags -----------------------------------------------------

    #[test]
    fn parse_flags_keeps_only_the_marks_it_recognises() {
        let s = parse_flags(X86_CPUINFO);
        for expected in [
            "avx2",
            "avx512",
            "fma",
            "aes",
            "sha_ni",
            "vt_x",
            "smep",
            "hypervisor",
        ] {
            assert!(s.contains(expected), "missing {expected} in {s:?}");
        }
        assert!(!s.contains("amd_v"), "Intel has no svm flag: {s:?}");
    }

    #[test]
    fn parse_flags_reports_how_many_were_left_out() {
        let s = parse_flags(X86_CPUINFO);
        assert!(s.contains("(+"), "expected a remainder count: {s:?}");
        // 19 flags in the fixture, 8 of them named.
        assert!(s.contains("19"), "{s:?}");
    }

    #[test]
    fn parse_flags_does_not_mislabour_smep_as_multithreading() {
        // smep is a kernel hardening flag, not a statement about SMT.
        let s = parse_flags("flags\t\t: smep\n");
        assert!(!s.contains("amd_smt"), "{s:?}");
        assert!(s.contains("smep"), "{s:?}");
    }

    #[test]
    fn parse_flags_recognises_amd_virtualisation() {
        let s = parse_flags("flags\t\t: fpu vme de svm\n");
        assert!(s.contains("amd_v"), "{s:?}");
        assert!(!s.contains("vt_x"), "{s:?}");
    }

    #[test]
    fn parse_flags_of_a_cpu_with_no_flags_line_is_a_dash() {
        assert_eq!(parse_flags("processor\t: 0\n"), "-");
        assert_eq!(parse_flags(""), "-");
        // A flags key with nothing after the colon is also unknown.
        assert_eq!(parse_flags("flags\t\t:\n"), "-");
    }

    #[test]
    fn parse_flags_with_nothing_recognised_still_reports_the_count() {
        let s = parse_flags("flags\t\t: fpu vme de pse tsc\n");
        assert!(s.contains("(+5 more)"), "{s:?}");
    }

    // ---- CpuInfo -----------------------------------------------------------

    #[test]
    fn microcode_comes_from_sysfs_without_a_doubled_prefix() {
        // The old path, cpufreq/microcode, does not exist, so the row was
        // always "-"; and sysfs already prints the 0x.
        let fx = Fixture::new();
        fx.write("proc/cpuinfo", X86_CPUINFO);
        fx.write("sys/devices/system/cpu/cpu0/microcode/version", "0x4121\n");

        let info = CpuInfo::read(&fx.host());
        assert_eq!(info.microcode.as_deref(), Some("0x4121"));
    }

    #[test]
    fn microcode_falls_back_to_cpuinfo() {
        let fx = Fixture::new();
        fx.write(
            "proc/cpuinfo",
            &format!("{X86_CPUINFO}microcode\t: 0xa404102\n"),
        );

        let info = CpuInfo::read(&fx.host());
        assert_eq!(info.microcode.as_deref(), Some("0xa404102"));
    }

    #[test]
    fn a_bare_microcode_revision_gets_its_prefix() {
        assert_eq!(with_hex_prefix("4121"), "0x4121");
        assert_eq!(with_hex_prefix("0x4121"), "0x4121");
    }

    // ---- rows() against a fixture -----------------------------------------

    fn machine() -> Fixture {
        let fx = Fixture::new();
        fx.write("proc/cpuinfo", X86_CPUINFO);
        fx.write("proc/stat", "cpu 0 0 0 0\ncpu0 0 0 0 0\ncpu2 0 0 0 0\n");
        fx.write("proc/uptime", "100.0 100.0\n");
        fx.write("proc/loadavg", "0.10 0.20 0.30 1/200 1\n");
        for (cpu, core) in [("cpu0", "0"), ("cpu1", "0"), ("cpu2", "1")] {
            let base = format!("sys/devices/system/cpu/{cpu}");
            fx.write(&format!("{base}/topology/physical_package_id"), "0\n");
            fx.write(&format!("{base}/topology/core_id"), &format!("{core}\n"));
        }
        fx.write(
            "sys/devices/system/cpu/cpu2/cpufreq/scaling_cur_freq",
            "2400000\n",
        );
        fx.write("sys/devices/system/cpu/online", "0,2\n");
        fx
    }

    fn primed(fx: &Fixture) -> Stats {
        let host = fx.host();
        let mut stats = Stats::new(&host);
        fx.write("proc/stat", "cpu 0 0 0 0\ncpu0 50 0 0 50\ncpu2 100 0 0 0\n");
        std::thread::sleep(std::time::Duration::from_millis(110));
        stats.sample(&host);
        stats
    }

    fn text(rows: &[Row]) -> String {
        format!("{rows:?}")
    }

    #[test]
    fn rows_count_physical_cores_from_the_topology() {
        let fx = machine();
        let host = fx.host();
        let rows = rows(&host, &primed(&fx), &CpuInfo::read(&host));

        assert!(text(&rows).contains("\"2 / 2\""), "{}", text(&rows));
    }

    #[test]
    fn per_core_frequency_is_read_for_the_named_core() {
        // cpu1 is offline: the second core in the list is cpu2, and its
        // frequency lives under cpu2, not cpu1.
        let fx = machine();
        let host = fx.host();
        let rows = rows(&host, &primed(&fx), &CpuInfo::read(&host));
        let cpu2 = rows
            .iter()
            .find_map(|r| match r {
                Row::Field { label, value, .. } if label == "cpu2" => Some(value.to_string()),
                _ => None,
            })
            .expect("a cpu2 row");

        assert!(cpu2.contains("2.40 GHz"), "{cpu2}");
        assert!(cpu2.contains("100%"), "{cpu2}");
    }

    #[test]
    fn an_unsampled_machine_says_it_is_still_sampling() {
        let fx = machine();
        let host = fx.host();
        let rows = rows(&host, &Stats::new(&host), &CpuInfo::read(&host));

        assert!(text(&rows).contains("sampling"), "{}", text(&rows));
    }

    #[test]
    fn a_machine_without_topology_does_not_guess() {
        let fx = Fixture::new();
        fx.write("proc/stat", "cpu 0 0 0 0\ncpu0 0 0 0 0\ncpu1 0 0 0 0\n");
        let host = fx.host();
        let rows = rows(&host, &Stats::new(&host), &CpuInfo::read(&host));

        assert!(text(&rows).contains("? / 2"), "{}", text(&rows));
    }

    #[test]
    fn a_cpu_list_counts_ranges_and_holes() {
        assert_eq!(count_cpu_list("0-15"), Some(16));
        assert_eq!(count_cpu_list("0-3,8-11"), Some(8));
        assert_eq!(count_cpu_list("0"), Some(1));
        assert_eq!(count_cpu_list("0,2,4"), Some(3));
        assert_eq!(
            count_cpu_list("4-0"),
            None,
            "an inverted range is not a list"
        );
        assert_eq!(count_cpu_list("online"), None);
        assert_eq!(count_cpu_list(""), None);
    }
}
