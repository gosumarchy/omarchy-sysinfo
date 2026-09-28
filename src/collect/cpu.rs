use super::stats::Stats;
use super::{
    Row,
    fs::{read, read_u64},
    units::{human_mhz, human_secs},
};
use std::sync::OnceLock;

/// `/proc/cpuinfo` does not change for the life of the process.
fn cpuinfo() -> String {
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED
        .get_or_init(|| read("/proc/cpuinfo").unwrap_or_default())
        .clone()
}

/// The name of the socket, e.g. `Ulysses-S`.
pub(crate) fn brand() -> String {
    parse_brand(&cpuinfo())
}

/// `model name` on x86, but ARM boards say `Hardware`, `Processor` or `Model`.
fn parse_brand(cpuinfo: &str) -> String {
    for key in ["model name", "Model name", "Hardware", "Processor", "Model"] {
        if let Some(value) = line_value(cpuinfo, key) {
            if !value.is_empty() {
                return value;
            }
        }
    }
    "unknown CPU".to_string()
}

pub(crate) fn rows(stats: &mut Stats) -> Vec<Row> {
    let mut rows = vec![
        Row::field("Model", brand()),
        Row::field(
            "Vendor",
            super::units::dash(line_value(&cpuinfo(), "vendor_id")),
        ),
        Row::field(
            "Microcode",
            super::units::dash(
                read("/sys/devices/system/cpu/cpu0/cpufreq/microcode").map(|m| format!("0x{m}")),
            ),
        ),
    ];

    let logical = stats.per_core().len();
    if logical == 0 {
        rows.push(Row::note("no cpu topology exposed"));

        return rows;
    }
    // Threads come in pairs unless the topology says otherwise; fall back to
    // the online cpu list when we cannot tell.
    let physical = physical_cores().unwrap_or(logical.div_ceil(2));

    let max_mhz = read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .map(|k| k / 1000)
        .unwrap_or(0);
    let min_mhz = read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq")
        .map(|k| k / 1000)
        .unwrap_or(0);
    let gov = read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").unwrap_or_default();

    rows.push(Row::Header("Topology".into()));
    rows.push(Row::field(
        "Cores / threads",
        format!("{physical} / {logical}"),
    ));
    if !gov.is_empty() {
        rows.push(Row::field("Frequency governor", gov));
    }
    if max_mhz > 0 {
        let range = if min_mhz > 0 {
            format!("{} – {}", human_mhz(min_mhz), human_mhz(max_mhz))
        } else {
            human_mhz(max_mhz)
        };
        rows.push(Row::field("Frequency range", range));
    }
    if let Some(online) = read("/sys/devices/system/cpu/online") {
        // The file is a CPU list (`0-15`, `0-3,8-11`), not a count. Parsing it
        // as a number meant the row never appeared.
        let value = match count_cpu_list(&online) {
            Some(n) => format!("{n} · {online}"),
            None => online,
        };
        rows.push(Row::field("Online CPUs", value));
    }
    rows.push(Row::field("Flags", flag_summary()));

    // Live load: average first, then one line per logical CPU. One read is
    // not a delta, so say so rather than print a confident zero.
    let usages: Vec<f64> = stats.per_core().to_vec();
    if usages.is_empty() {
        rows.push(Row::note("no per-core counters in /proc/stat"));

        return rows;
    }
    if !stats.primed() {
        rows.push(Row::Header("Load".into()));
        rows.push(Row::note("sampling, refresh in a moment"));
    } else {
        if let Some(avg) = mean(&usages) {
            let (load1, load5, load15) = stats.load();
            rows.push(Row::Header("Load".into()));
            rows.push(Row::field_with("Total", format!("{avg:.0}%"), avg / 100.0));
            rows.push(Row::field(
                "Average",
                format!("{load1:.2}  {load5:.2}  {load15:.2}"),
            ));
            rows.push(Row::field("Threads", stats.process_load()));
            if let Some(idle) = stats.idle_since_boot() {
                rows.push(Row::field("Idle since boot", format!("{idle:.1}%")));
            }
        }

        rows.push(Row::Header("Per-core load".into()));
        for (idx, usage) in usages.iter().enumerate() {
            let cur = read_u64(format!(
                "/sys/devices/system/cpu/cpu{idx}/cpufreq/scaling_cur_freq"
            ))
            .map(|k| k / 1000);
            let name = stats
                .core_names()
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("cpu{idx}"));
            let value = match cur {
                Some(mhz) if mhz > 0 => format!("{usage:>3.0}%  {}", human_mhz(mhz)),
                _ => format!("{usage:>3.0}%"),
            };
            rows.push(Row::field_with(name, value, usage / 100.0));
        }
    }

    let boot_epoch = super::system::boot_epoch();
    if boot_epoch > 0 {
        rows.push(Row::Header("Time".into()));
        rows.push(Row::field("Since boot", human_secs(stats.uptime())));
        rows.push(Row::field("Boot epoch", boot_epoch.to_string()));
    }

    rows
}

/// Physical cores from the topology exposed in sysfs, if the kernel shares it.
fn physical_cores() -> Option<usize> {
    let mut ids: Vec<(String, String)> = Vec::new();
    for entry in super::fs::list_dir("/sys/devices/system/cpu") {
        let Some(name) = entry.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        if !name.starts_with("cpu") || name.contains('-') {
            continue;
        }
        // A CPU that is offline, or on a kernel without topology, has no such
        // file. Bailing out of the whole function on the first one threw away
        // the topology for every other CPU and silently fell back to a guess.
        let (Some(package), Some(core)) = (
            read(entry.join("topology/physical_package_id")),
            read(entry.join("topology/core_id")),
        ) else {
            continue;
        };
        ids.push((package, core));
    }
    (!ids.is_empty()).then(|| {
        let mut unique: Vec<(String, String)> = Vec::new();
        for id in ids {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        unique.len()
    })
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
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

/// Group the ~50 CPU flags into the few that actually explain what this chip can do.
fn flag_summary() -> String {
    parse_flags(&cpuinfo())
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

fn parse_flags(cpuinfo: &str) -> String {
    let Some(line) = cpuinfo.lines().find(|l| l.starts_with("flags")) else {
        return "-".to_string();
    };
    let flags: Vec<&str> = line
        .split(':')
        .nth(1)
        .map(|f| f.split_whitespace().collect())
        .unwrap_or_default();
    if flags.is_empty() {
        return "-".to_string();
    }
    let has = |needle: &str| flags.contains(&needle);
    // Note: the virtualisation mark is reported as vmx/svm because that is
    // what the flag is actually called, and smep is smep -- it says nothing
    // about simultaneous multithreading.
    let mut marks = vec![
        ("avx2", has("avx2")),
        ("avx512", has("avx512f")),
        ("fma", has("fma")),
        ("aes", has("aes")),
        ("sha_ni", has("sha_ni")),
        ("vt_x", has("vmx")),
        ("amd_v", has("svm")),
        ("smep", has("smep")),
        ("hypervisor", has("hypervisor")),
    ];
    marks.retain(|(_, on)| *on);
    let joined = marks
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" ");
    format!("{joined}  (+{} more)", flags.len())
}

#[cfg(test)]
fn parse_idle_hint(stat: &str) -> Option<String> {
    let first = stat.lines().next()?;
    let mut parts = first.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }
    let values: Vec<f64> = parts.filter_map(|v| v.parse().ok()).collect();
    if values.len() < 5 {
        return None;
    }
    let total: f64 = values.iter().sum();
    let idle = values[3] + values[4];
    if total <= 0.0 {
        return None;
    }
    Some(format!("{:.0}% idle", idle / total * 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ---- parse_idle_hint -------------------------------------------------

    #[test]
    fn parse_idle_hint_uses_idle_plus_iowait() {
        // user 100, nice 0, system 100, idle 700, iowait 100 -> 800/1000
        let stat = "cpu  100 0 100 700 100 0 0 0\ncpu0 100 0 100 700 100 0 0 0\n";
        assert_eq!(parse_idle_hint(stat).as_deref(), Some("80% idle"));
    }

    #[test]
    fn parse_idle_hint_rounds_to_whole_percent() {
        let stat = "cpu  1 0 0 2 0 0 0 0\n";
        assert_eq!(parse_idle_hint(stat).as_deref(), Some("67% idle"));
    }

    #[test]
    fn parse_idle_hint_needs_five_columns_and_a_cpu_first_line() {
        assert_eq!(parse_idle_hint("cpu 1 2 3 4\n"), None, "too few columns");
        assert_eq!(
            parse_idle_hint("intr 1 2 3 4 5\n"),
            None,
            "not the cpu line"
        );
        assert_eq!(parse_idle_hint(""), None);
    }

    #[test]
    fn parse_idle_hint_refuses_a_zero_total() {
        assert_eq!(parse_idle_hint("cpu  0 0 0 0 0 0 0 0\n"), None);
    }

    // ---- mean ------------------------------------------------------------

    #[test]
    fn mean_averages_the_samples() {
        assert_eq!(mean(&[0.0, 100.0]), Some(50.0));
        assert_eq!(mean(&[25.0]), Some(25.0));
        assert_eq!(mean(&[]), None, "an empty set has no mean");
    }

    // ---- rows() against the live machine ---------------------------------

    #[test]
    fn rows_renders_without_panicking_on_this_machine() {
        let mut stats = Stats::new();
        let rows = rows(&mut stats);
        assert!(!rows.is_empty());
        let text: String = rows.iter().map(|r| format!("{r:?}")).collect();
        assert!(!text.contains("NaN"), "a NaN leaked into the CPU rows");
        assert!(!text.contains('∞'), "an infinity leaked into the CPU rows");
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

    #[test]
    fn brand_and_flags_always_produce_something() {
        assert!(!brand().is_empty());
        assert!(!flag_summary().is_empty());
    }
}
