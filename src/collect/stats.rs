//! Live counters parsed straight out of `/proc`. Two samples are needed before
//! CPU usage means anything, so `sample()` is called repeatedly over time.

use std::path::Path;
use std::time::{Duration, Instant};

use super::Host;
use super::Row;
use super::fs::read;
use super::units::{approx_f64, fraction, human_bytes};

#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Times {
    total: u64,
    idle: u64,
}

impl Times {
    /// Usage as a percentage of the interval between two samples.
    fn usage_since(self, prev: Times) -> Option<f64> {
        let d_total = self.total.checked_sub(prev.total)?;
        let d_idle = self.idle.checked_sub(prev.idle)?;
        if d_total == 0 {
            return None;
        }
        // Idle can legitimately exceed the total across a hotplug or a kernel
        // that added a counter, so this must not wrap: a wrapped subtraction
        // reported a wildly wrong percentage and panicked in debug builds.
        let busy = d_total.saturating_sub(d_idle);

        Some(approx_f64(busy) / approx_f64(d_total) * 100.0)
    }
}

/// One `cpuN` line of `/proc/stat`.
#[derive(Clone, Debug, PartialEq)]
struct CoreTimes {
    name: String,
    times: Times,
}

/// Everything `/proc/stat` says about CPU time.
#[derive(Clone, Debug, Default, PartialEq)]
struct CpuTimes {
    total: Times,
    cores: Vec<CoreTimes>,
}

/// One logical CPU's usage over the last interval.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Core {
    /// The kernel's name for it, `cpu3`. Offline CPUs are missing from
    /// `/proc/stat`, so the name, not the position, is what identifies it.
    pub(crate) name: String,
    /// Busy share of the interval, `0.0..=100.0`.
    pub(crate) usage: f64,
}

/// `/proc/loadavg`: the three averages and the task counts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Load {
    pub(crate) one: f64,
    pub(crate) five: f64,
    pub(crate) fifteen: f64,
    pub(crate) runnable: u32,
    pub(crate) threads: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Memory {
    pub(crate) total: u64,
    pub(crate) available: u64,
    pub(crate) free: u64,
    pub(crate) cached: u64,
    pub(crate) buffers: u64,
    pub(crate) swap_total: u64,
    pub(crate) swap_free: u64,
    pub(crate) huge_pages_total: u64,
    pub(crate) huge_pages_free: u64,
    pub(crate) swap_devices: usize,
}

impl Memory {
    pub(crate) fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub(crate) fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }

    pub(crate) fn rows(&self, host: &Host) -> Vec<Row> {
        let mut rows = vec![Row::header("Memory")];
        rows.push(Row::field_with(
            "Physical",
            format!("{} / {}", human_bytes(self.used()), human_bytes(self.total)),
            fraction(self.used(), self.total),
        ));
        rows.push(Row::field("Available", human_bytes(self.available)));
        rows.push(Row::field("Free", human_bytes(self.free)));
        if self.cached > 0 {
            rows.push(Row::field("Page cache", human_bytes(self.cached)));
        }
        if self.buffers > 0 {
            rows.push(Row::field("Buffers", human_bytes(self.buffers)));
        }

        if self.swap_total > 0 {
            rows.push(Row::header("Swap"));
            rows.push(Row::field_with(
                "Total",
                format!(
                    "{} / {}",
                    human_bytes(self.swap_used()),
                    human_bytes(self.swap_total)
                ),
                fraction(self.swap_used(), self.swap_total),
            ));
            rows.push(Row::field("Swap devices", self.swap_devices.to_string()));
        }

        // zram is compressed, so its backing size is not its useful size.
        if let Some(zram) = zram_rows(&host.path("/sys/block/zram0")) {
            rows.push(Row::header("Compressed"));
            rows.extend(zram);
        }

        if self.huge_pages_total > 0 {
            rows.push(Row::field(
                "Huge pages",
                format!("{} / {} free", self.huge_pages_free, self.huge_pages_total),
            ));
        }

        rows
    }
}

fn zram_rows(base: &Path) -> Option<Vec<Row>> {
    let disksize = super::fs::read_u64(base.join("disksize"))?;
    // mm_stat is whitespace separated and in bytes: how much data is stored
    // uncompressed, how much it compresses down to, then the memory cost.
    let (orig, compressed) = parse_mm_stat(&read(base.join("mm_stat"))?)?;

    let mut rows = vec![Row::field_with(
        "zram0",
        format!(
            "{} stored, {} on disk",
            human_bytes(orig),
            human_bytes(compressed)
        ),
        fraction(orig, disksize),
    )];
    if orig > 0 && compressed > 0 {
        rows.push(Row::field(
            "Ratio",
            format!("{:.1}x smaller", fraction(orig, compressed)),
        ));
    }
    if let Some(algorithm) = read(base.join("comp_algorithm")) {
        match active_algorithm(&algorithm) {
            Some(name) => rows.push(Row::field("Algorithm", name)),
            None => rows.push(Row::field("Algorithms", algorithm)),
        }
    }

    Some(rows)
}

/// Everything that has to be sampled over time to show a live figure.
#[derive(Debug)]
pub(crate) struct Stats {
    prev: CpuTimes,
    cores: Vec<Core>,
    memory: Memory,
    uptime: u64,
    load: Load,
    /// When the counters were last read, and whether usage is a real delta yet.
    last_sample: Option<Instant>,
    primed: bool,
}

/// Below this interval a `/proc/stat` delta is mostly scheduler noise.
const MIN_INTERVAL: Duration = Duration::from_millis(100);

impl Stats {
    pub(crate) fn new(host: &Host) -> Stats {
        let mut stats = Stats {
            prev: CpuTimes::default(),
            cores: Vec::new(),
            memory: Memory::default(),
            uptime: 0,
            load: Load::default(),
            last_sample: None,
            primed: false,
        };
        stats.sample(host);

        stats
    }

    /// Whether per-core usage is a real measurement yet, as opposed to a
    /// placeholder for reads that were too close together to mean anything.
    pub(crate) fn primed(&self) -> bool {
        self.primed
    }

    /// Re-read `/proc/stat`, `/proc/meminfo`, `/proc/uptime` and `/proc/loadavg`.
    pub(crate) fn sample(&mut self, host: &Host) {
        self.sample_cpu(host);

        self.memory = read_memory(host);
        self.uptime = host.read("/proc/uptime").map_or(0, |u| parse_uptime(&u));
        if let Some(loadavg) = host.read("/proc/loadavg") {
            self.load = parse_loadavg(&loadavg);
        }
    }

    /// Usage is a difference between two reads, so it is only meaningful once
    /// enough wall-clock time has passed for the counters to move.
    ///
    /// A read that comes too soon changes nothing: it keeps the last real
    /// measurement on screen and keeps the old baseline, so the next read
    /// measures over the whole gap. Zeroing every core instead (as a held
    /// `r` did) showed a confident 0% as if it had been measured.
    fn sample_cpu(&mut self, host: &Host) {
        let now = Instant::now();
        let elapsed = self.last_sample.map(|t| now.duration_since(t));
        if elapsed.is_some_and(|e| e < MIN_INTERVAL) {
            return;
        }

        let current = host
            .read("/proc/stat")
            .map(|s| parse_cpu_times(&s))
            .unwrap_or_default();
        // The very first read has nothing to measure against.
        let measured = elapsed.is_some();
        self.cores = current
            .cores
            .iter()
            .map(|core| Core {
                name: core.name.clone(),
                usage: if measured { self.usage_of(core) } else { 0.0 },
            })
            .collect();
        self.primed |= measured;
        self.prev = current;
        self.last_sample = Some(now);
    }

    /// A core's usage since the previous sample, matched by name so an
    /// offline CPU does not shift every core after it onto its neighbour's
    /// counters.
    fn usage_of(&self, core: &CoreTimes) -> f64 {
        let Some(prev) = self.prev.cores.iter().find(|p| p.name == core.name) else {
            // Just came online: no baseline to measure against yet.
            return 0.0;
        };

        core.times
            .usage_since(prev.times)
            .unwrap_or(0.0)
            .clamp(0.0, 100.0)
    }

    pub(crate) fn cpu_usage(&self) -> f64 {
        if self.cores.is_empty() {
            return 0.0;
        }

        let sum: f64 = self.cores.iter().map(|c| c.usage).sum();

        sum / approx_f64(self.cores.len() as u64)
    }

    pub(crate) fn cores(&self) -> &[Core] {
        &self.cores
    }

    pub(crate) fn memory(&self) -> Memory {
        self.memory
    }

    pub(crate) fn uptime(&self) -> u64 {
        self.uptime
    }

    pub(crate) fn load(&self) -> Load {
        self.load
    }

    /// The kernel's own idle ratio since boot, from the aggregate `cpu` line
    /// `sample` already parsed.
    pub(crate) fn idle_since_boot(&self) -> Option<f64> {
        let total = self.prev.total;
        if total.total == 0 {
            return None;
        }

        Some(fraction(total.idle, total.total) * 100.0)
    }
}

/// Columns of a `cpu` line that make up its time: user, nice, system, idle,
/// iowait, irq, softirq, steal. The two after them, `guest` and `guest_nice`, are
/// already included in user and nice, so counting them again overstated the
/// total on a machine running VMs.
const TIME_COLUMNS: usize = 8;

/// The `cpu` and `cpuN` lines, and nothing else.
fn parse_cpu_times(stat: &str) -> CpuTimes {
    let mut out = CpuTimes::default();

    for line in stat.lines() {
        let mut fields = line.split_whitespace();
        let Some(label) = fields.next() else {
            continue;
        };
        // Only `cpu` and `cpuN`. A bare starts_with("cpu") would let a
        // hypothetical `cpufreq` line masquerade as a core.
        let is_core = label
            .strip_prefix("cpu")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if label != "cpu" && !is_core {
            continue;
        }

        let values: Vec<u64> = fields
            .take(TIME_COLUMNS)
            .filter_map(|v| v.parse().ok())
            .collect();
        // A missing iowait (exactly four columns) is zero, not a panic.
        if values.len() < 4 {
            continue;
        }
        let idle = values[3] + values.get(4).copied().unwrap_or(0);
        let times = Times {
            total: values.iter().sum(),
            idle,
        };

        if is_core {
            out.cores.push(CoreTimes {
                name: label.to_string(),
                times,
            });
        } else {
            out.total = times;
        }
    }

    out
}

fn read_memory(host: &Host) -> Memory {
    let mut mem = host
        .read("/proc/meminfo")
        .map(|info| parse_memory(&info))
        .unwrap_or_default();
    mem.swap_devices = host
        .read("/proc/swaps")
        .map_or(0, |s| swap_device_count(&s));

    mem
}

/// `/proc/meminfo` reports every size in kibibytes. Huge-page lines are counts.
fn parse_memory(info: &str) -> Memory {
    let mut mem = Memory::default();
    for line in info.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(raw) = parts.next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        let bytes = raw.saturating_mul(1024);
        match key {
            "MemTotal:" => mem.total = bytes,
            "MemAvailable:" => mem.available = bytes,
            "MemFree:" => mem.free = bytes,
            "Cached:" => mem.cached = bytes,
            "Buffers:" => mem.buffers = bytes,
            "SwapTotal:" => mem.swap_total = bytes,
            "SwapFree:" => mem.swap_free = bytes,
            "HugePages_Total:" => mem.huge_pages_total = raw,
            "HugePages_Free:" => mem.huge_pages_free = raw,
            // /proc/meminfo has dozens of other keys this report does not show.
            _ => {}
        }
    }

    mem
}

/// `/proc/swaps` has a header line, then one row per swap area.
fn swap_device_count(swaps: &str) -> usize {
    swaps
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .count()
}

/// `/proc/loadavg`: three averages, then "running/total" tasks.
fn parse_loadavg(text: &str) -> Load {
    let mut parts = text.split_whitespace();
    let mut average = || parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let (one, five, fifteen) = (average(), average(), average());
    let mut load = Load {
        one,
        five,
        fifteen,
        ..Load::default()
    };

    if let Some(tasks) = parts.next() {
        let bits: Vec<&str> = tasks.split('/').collect();
        if let [runnable, threads] = bits.as_slice() {
            load.runnable = runnable.parse().unwrap_or(0);
            load.threads = threads.parse().unwrap_or(0);
        }
    }

    load
}

/// `/proc/uptime` is seconds with a fractional part.
fn parse_uptime(text: &str) -> u64 {
    text.split_whitespace()
        .next()
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// `mm_stat`: bytes stored uncompressed, then bytes actually occupied.
fn parse_mm_stat(stat: &str) -> Option<(u64, u64)> {
    let mut fields = stat.split_whitespace();
    let orig: u64 = fields.next()?.parse().ok()?;
    let compressed: u64 = fields.next().unwrap_or("0").parse().unwrap_or(0);

    Some((orig, compressed))
}

/// The kernel lists every algorithm with the active one in brackets.
fn active_algorithm(list: &str) -> Option<&str> {
    list.split_whitespace()
        .find_map(|a| a.strip_prefix('[').and_then(|a| a.strip_suffix(']')))
}

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "these tests pin exact, exactly representable results"
)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    const PROC_STAT: &str = "\
cpu  1000 20 300 8000 40 0 10 0 0 0
cpu0 500 10 150 4000 20 0 5 0 0 0
cpu1 500 10 150 4000 20 0 5 0 0 0
intr 12345
ctxt 6789
btime 1700000000
processes 4242
procs_running 2
procs_blocked 0
softirq 999 0 1 2
";

    const MEMINFO: &str = "\
MemTotal:       16384000 kB
MemFree:         2000000 kB
MemAvailable:    8192000 kB
Buffers:          500000 kB
Cached:          6000000 kB
SwapCached:       100000 kB
SwapTotal:       4194300 kB
SwapFree:        3145728 kB
HugePages_Total:       2
HugePages_Free:        1
Hugepagesize:       2048 kB
";

    // ---- Times::usage_since ----------------------------------------------

    #[test]
    fn usage_is_the_busy_share_of_the_interval() {
        let prev = Times {
            total: 1000,
            idle: 500,
        };
        // 200 jiffies passed, 100 of them idle: 50% busy.
        let now = Times {
            total: 1200,
            idle: 600,
        };
        assert_eq!(now.usage_since(prev), Some(50.0));
    }

    #[test]
    fn usage_is_none_when_the_clock_did_not_move() {
        let t = Times {
            total: 100,
            idle: 50,
        };
        assert_eq!(t.usage_since(t), None, "an identical sample means nothing");
    }

    #[test]
    fn usage_is_none_when_the_counters_went_backwards() {
        // A read that lands before the previous one must not report a huge
        // negative-then-wrapped number.
        let now = Times {
            total: 100,
            idle: 50,
        };
        let prev = Times {
            total: 200,
            idle: 100,
        };
        assert_eq!(now.usage_since(prev), None);
    }

    #[test]
    fn usage_does_not_wrap_when_idle_exceeds_the_total_delta() {
        // The bug: `d_total - d_idle` underflowed. It panicked in debug and
        // produced an enormous percentage in release.
        let prev = Times {
            total: 1000,
            idle: 900,
        };
        let now = Times {
            total: 1010,
            idle: 1000,
        };
        let usage = now.usage_since(prev).expect("a delta exists");
        assert!(usage.is_finite(), "usage must stay finite, got {usage}");
        assert!(
            (0.0..=100.0).contains(&usage),
            "usage must stay in range, got {usage}"
        );
        assert_eq!(usage, 0.0, "idle beyond the total means no busy time");
    }

    #[test]
    fn usage_reaches_both_ends_of_the_scale() {
        let prev = Times { total: 0, idle: 0 };
        assert_eq!(
            Times {
                total: 100,
                idle: 0
            }
            .usage_since(prev),
            Some(100.0)
        );
        assert_eq!(
            Times {
                total: 100,
                idle: 100
            }
            .usage_since(prev),
            Some(0.0)
        );
    }

    // ---- parse_cpu_times -------------------------------------------------

    fn names(times: &CpuTimes) -> Vec<&str> {
        times.cores.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn parse_cpu_times_splits_the_aggregate_from_the_cores() {
        let times = parse_cpu_times(PROC_STAT);
        // 1000+20+300+8000+40+0+10 = 9370, idle 8000+40 = 8040
        assert_eq!(
            times.total,
            Times {
                total: 9370,
                idle: 8040
            }
        );
        assert_eq!(names(&times), ["cpu0", "cpu1"]);
        assert_eq!(
            times.cores[0].times,
            Times {
                total: 4685,
                idle: 4020
            }
        );
    }

    #[test]
    fn parse_cpu_times_ignores_a_cpu_prefixed_line_that_is_not_a_core() {
        // A bare starts_with("cpu") used to let these through as cores.
        let times = parse_cpu_times("cpu 1 1 1 1 1\ncpufreq 5 5 5 5\ncpufoo 1 1 1 1\n");
        assert!(times.cores.is_empty(), "cpufreq/cpufoo are not cores");
        assert_ne!(
            times.total,
            Times::default(),
            "the real cpu line still parses"
        );
    }

    #[test]
    fn parse_cpu_times_skips_a_cpu_line_with_too_few_columns() {
        // Fewer than four values means we cannot tell idle from busy.
        let times = parse_cpu_times("cpu 1 2 3\ncpu0 1 2 3\n");
        assert_eq!(times, CpuTimes::default());
    }

    #[test]
    fn parse_cpu_times_treats_a_missing_iowait_as_zero() {
        // Four columns is a legal older `/proc/stat`. Indexing iowait used to panic.
        let times = parse_cpu_times("cpu 10 0 0 90\ncpu0 10 0 0 90\n");
        assert_eq!(
            times.total,
            Times {
                total: 100,
                idle: 90
            }
        );
        assert_eq!(times.cores[0].times.idle, 90);
    }

    #[test]
    fn parse_cpu_times_survives_junk() {
        assert_eq!(parse_cpu_times(""), CpuTimes::default());
        assert_eq!(parse_cpu_times("\n\n\n"), CpuTimes::default());
        // Non-numeric columns are dropped, which can leave too few to use.
        assert_eq!(
            parse_cpu_times("cpu a b c d e f\ncpu0 a b c d e f\n"),
            CpuTimes::default()
        );
    }

    #[test]
    fn parse_cpu_times_includes_steal_time_in_the_total() {
        let times = parse_cpu_times("cpu 100 0 0 800 0 0 0 50\n");
        assert_eq!(times.total.total, 950);
        assert_eq!(times.total.idle, 800);
    }

    #[test]
    fn parse_cpu_times_does_not_count_guest_time_twice() {
        // guest (30) and guest_nice (5) are already inside user and nice.
        let times = parse_cpu_times("cpu 100 0 0 800 0 0 0 50 30 5\n");
        assert_eq!(times.total.total, 950);
    }

    #[test]
    fn parse_cpu_times_keeps_the_kernel_names_of_sparse_cores() {
        // cpu1 is offline, so it is simply absent.
        let times = parse_cpu_times("cpu 1 1 1 1\ncpu0 1 1 1 1\ncpu2 1 1 1 1\n");
        assert_eq!(names(&times), ["cpu0", "cpu2"]);
    }

    // ---- parse_memory ----------------------------------------------------

    #[test]
    fn parse_memory_converts_kibibytes_to_bytes() {
        let m = parse_memory(MEMINFO);
        assert_eq!(m.total, 16_384_000 * 1024);
        assert_eq!(m.free, 2_000_000 * 1024);
        assert_eq!(m.available, 8_192_000 * 1024);
        assert_eq!(m.buffers, 500_000 * 1024);
        assert_eq!(m.cached, 6_000_000 * 1024);
    }

    #[test]
    fn parse_memory_reads_cached_without_catching_swap_cached() {
        let m = parse_memory(MEMINFO);
        // SwapCached is 100000 kB and must not be folded into Cached.
        assert_eq!(m.cached, 6_000_000 * 1024);
    }

    #[test]
    fn parse_memory_reads_swap() {
        let m = parse_memory(MEMINFO);
        assert_eq!(m.swap_total, 4_194_300 * 1024);
        assert_eq!(m.swap_free, 3_145_728 * 1024);
        assert_eq!(m.swap_used(), 4_194_300 * 1024 - 3_145_728 * 1024);
    }

    #[test]
    fn parse_memory_keeps_huge_pages_as_counts() {
        let m = parse_memory(MEMINFO);
        assert_eq!(m.huge_pages_total, 2, "a count, not bytes");
        assert_eq!(m.huge_pages_free, 1);
    }

    #[test]
    fn parse_memory_defaults_missing_keys_to_zero() {
        let m = parse_memory("MemTotal: 1024 kB\n");
        assert_eq!(m.total, 1024 * 1024);
        assert_eq!(m.free, 0);
        assert_eq!(m.available, 0);
        assert_eq!(m.swap_total, 0);
        assert_eq!(m.huge_pages_total, 0);
    }

    #[test]
    fn parse_memory_of_an_empty_file_is_all_zero() {
        let m = parse_memory("");
        assert_eq!(m.total, 0);
        assert_eq!(m.used(), 0, "used must not underflow");
        assert_eq!(m.swap_used(), 0);
    }

    // ---- Memory arithmetic ----------------------------------------------

    #[test]
    fn used_is_total_minus_available() {
        let m = Memory {
            total: 1000,
            available: 400,
            ..Memory::default()
        };
        assert_eq!(m.used(), 600);
    }

    #[test]
    fn used_saturates_instead_of_underflowing() {
        // Available above total should not be possible, but if it ever were,
        // the bar must not wrap to an absurd number.
        let m = Memory {
            total: 100,
            available: 500,
            ..Memory::default()
        };
        assert_eq!(m.used(), 0);
    }

    #[test]
    fn swap_used_saturates_instead_of_underflowing() {
        let m = Memory {
            swap_total: 100,
            swap_free: 500,
            ..Memory::default()
        };
        assert_eq!(m.swap_used(), 0);
    }

    #[test]
    fn memory_rows_omit_swap_sections_when_there_is_no_swap() {
        let m = Memory {
            total: 2048,
            available: 1024,
            ..Memory::default()
        };
        let text = rows_text(&m.rows(&Fixture::new().host()));
        assert!(!text.contains("Swap"), "no swap block expected: {text}");
    }

    #[test]
    fn memory_rows_include_a_swap_block_when_swap_exists() {
        let m = Memory {
            total: 2048,
            available: 1024,
            swap_total: 4096,
            swap_free: 1024,
            ..Memory::default()
        };
        let text = rows_text(&m.rows(&Fixture::new().host()));
        assert!(text.contains("Swap"), "expected a swap block: {text}");
    }

    fn rows_text(rows: &[Row]) -> String {
        rows.iter()
            .map(|r| match r {
                Row::Header(t) => format!("# {t}\n"),
                Row::Field { label, value, .. } => format!("{label}: {value}\n"),
                Row::Note(t) => format!("({t})\n"),
                Row::Blank => "\n".to_string(),
            })
            .collect()
    }

    // ---- parse_loadavg ---------------------------------------------------

    #[test]
    fn parse_loadavg_reads_the_three_averages_and_task_counts() {
        let load = parse_loadavg("0.52 0.58 0.59 2/431 12345");
        assert_eq!((load.one, load.five, load.fifteen), (0.52, 0.58, 0.59));
        assert_eq!((load.runnable, load.threads), (2, 431));
    }

    #[test]
    fn parse_loadavg_tolerates_missing_and_malformed_fields() {
        let tasks = |s: &str| {
            let l = parse_loadavg(s);
            (l.runnable, l.threads)
        };
        assert_eq!(parse_loadavg(""), Load::default());
        assert_eq!(parse_loadavg("0.5").one, 0.5);
        assert_eq!(parse_loadavg("a b c 1/2").one, 0.0);
        assert_eq!(tasks("0.1 0.2 0.3 1/2"), (1, 2));
        // A task field that is not "n/m" leaves the counts at zero.
        assert_eq!(tasks("0.1 0.2 0.3 7"), (0, 0));
        // A half-written "n/" or "/m" still has two fields, so it parses
        // partially rather than being rejected outright.
        assert_eq!(tasks("0.1 0.2 0.3 7/"), (7, 0));
        assert_eq!(tasks("0.1 0.2 0.3 /9"), (0, 9));
        assert_eq!(tasks("0.1 0.2 0.3 1/2/3"), (0, 0));
    }

    // ---- parse_uptime ----------------------------------------------------

    #[test]
    fn parse_uptime_truncates_to_whole_seconds() {
        assert_eq!(parse_uptime("12345.67 98765.43"), 12345);
        assert_eq!(parse_uptime("0.00 0.00"), 0);
        assert_eq!(parse_uptime("junk"), 0);
        assert_eq!(parse_uptime(""), 0);
    }

    // ---- parse_mm_stat ---------------------------------------------------

    #[test]
    fn parse_mm_stat_reads_the_first_two_columns() {
        assert_eq!(
            parse_mm_stat("1048576 262144 1536"),
            Some((1_048_576, 262_144))
        );
        assert_eq!(parse_mm_stat("  10   20  30 "), Some((10, 20)));
    }

    #[test]
    fn parse_mm_stat_defaults_a_missing_second_column_to_zero() {
        assert_eq!(parse_mm_stat("1048576"), Some((1_048_576, 0)));
    }

    #[test]
    fn parse_mm_stat_rejects_empty_and_non_numeric_input() {
        assert_eq!(parse_mm_stat(""), None);
        assert_eq!(parse_mm_stat("   "), None);
        assert_eq!(parse_mm_stat("abc def"), None);
    }

    // ---- active_algorithm ------------------------------------------------

    #[test]
    fn active_algorithm_finds_the_bracketed_entry() {
        assert_eq!(active_algorithm("lz4 zstd [zstd] brl"), Some("zstd"));
        assert_eq!(active_algorithm("[lz4] zstd brl"), Some("lz4"));
        assert_eq!(active_algorithm("lz4 [brl] zstd"), Some("brl"));
    }

    #[test]
    fn active_algorithm_is_none_when_nothing_is_bracketed() {
        assert_eq!(active_algorithm("lz4 zstd brl"), None);
        assert_eq!(active_algorithm(""), None);
        // Half-bracketed entries are not a match either.
        assert_eq!(active_algorithm("lz4 [zstd brl"), None);
        assert_eq!(active_algorithm("lz4 zstd]"), None);
    }

    // ---- swap_device_count -----------------------------------------------

    #[test]
    fn swap_device_count_skips_the_header_and_blank_lines() {
        let swaps = "Filename\t\t\t\tType\t\tSize\tUsed\tPriority\n\
                     /dev/zram0                               partition\t8388604\t0\n\
                     /swapfile                                file\t\t2097148\t0\n";
        assert_eq!(swap_device_count(swaps), 2);
        assert_eq!(swap_device_count(""), 0);
        assert_eq!(swap_device_count("Filename Type Size\n"), 0);
    }

    // ---- Stats (against a fixture) ----------------------------------------

    fn machine(stat: &str) -> Fixture {
        let fx = Fixture::new();
        fx.write("proc/stat", stat);
        fx.write("proc/meminfo", MEMINFO);
        fx.write("proc/uptime", "12345.67 98765.43\n");
        fx.write("proc/loadavg", "0.52 0.58 0.59 2/431 12345\n");
        fx
    }

    #[test]
    fn new_stats_reads_the_machine() {
        let fx = machine(PROC_STAT);
        let s = Stats::new(&fx.host());

        assert_eq!(s.cores().len(), 2);
        assert_eq!(s.uptime(), 12345);
        assert_eq!(s.memory().total, 16_384_000 * 1024);
        assert_eq!(s.load().threads, 431);
    }

    #[test]
    fn usage_is_not_reported_before_a_real_interval_has_passed() {
        // A back-to-back read would otherwise claim a confident 0%.
        let fx = machine(PROC_STAT);
        let host = fx.host();
        let mut s = Stats::new(&host);
        assert!(!s.primed(), "the very first sample is not a measurement");
        s.sample(&host);
        assert!(!s.primed(), "still too soon to mean anything");
    }

    #[test]
    fn usage_is_measured_per_core_once_time_has_passed() {
        let fx = machine("cpu 0 0 0 0\ncpu0 0 0 0 0\ncpu1 0 0 0 0\n");
        let host = fx.host();
        let mut s = Stats::new(&host);

        // cpu0 fully busy, cpu1 fully idle over the interval.
        fx.write(
            "proc/stat",
            "cpu 100 0 0 100\ncpu0 100 0 0 0\ncpu1 0 0 0 100\n",
        );
        std::thread::sleep(MIN_INTERVAL);
        s.sample(&host);

        assert!(s.primed());
        assert_eq!(s.cores()[0].usage, 100.0);
        assert_eq!(s.cores()[1].usage, 0.0);
        assert_eq!(s.cpu_usage(), 50.0);
    }

    #[test]
    fn a_sample_too_soon_after_a_measurement_keeps_the_measurement() {
        // A held `r` re-collects straight away; that used to zero every core
        // while still claiming to be a measurement.
        let fx = machine("cpu 0 0 0 0\ncpu0 0 0 0 0\n");
        let host = fx.host();
        let mut s = Stats::new(&host);
        fx.write("proc/stat", "cpu 100 0 0 0\ncpu0 100 0 0 0\n");
        std::thread::sleep(MIN_INTERVAL);
        s.sample(&host);
        assert_eq!(s.cores()[0].usage, 100.0);

        fx.write("proc/stat", "cpu 100 0 0 50\ncpu0 100 0 0 50\n");
        s.sample(&host);
        assert!(s.primed());
        assert_eq!(s.cores()[0].usage, 100.0, "the reading must survive");

        // The next real sample measures from the last kept baseline.
        std::thread::sleep(MIN_INTERVAL);
        s.sample(&host);
        assert_eq!(s.cores()[0].usage, 0.0, "only idle time since the baseline");
    }

    #[test]
    fn an_offline_core_does_not_shift_the_others_onto_its_counters() {
        let fx = machine("cpu 0 0 0 0\ncpu0 0 0 0 0\ncpu1 0 0 0 0\ncpu2 0 0 0 0\n");
        let host = fx.host();
        let mut s = Stats::new(&host);

        // cpu1 goes offline; cpu2 is fully busy. Matched by position, cpu2
        // would have been compared with cpu1's counters.
        fx.write("proc/stat", "cpu 0 0 0 0\ncpu0 0 0 0 100\ncpu2 100 0 0 0\n");
        std::thread::sleep(MIN_INTERVAL);
        s.sample(&host);

        let cpu2 = s.cores().iter().find(|c| c.name == "cpu2").expect("cpu2");
        assert_eq!(cpu2.usage, 100.0);
        assert_eq!(s.cores().len(), 2);
    }

    #[test]
    fn a_machine_without_proc_reads_as_empty_rather_than_panicking() {
        let fx = Fixture::new();
        let s = Stats::new(&fx.host());

        assert!(s.cores().is_empty());
        assert_eq!(s.cpu_usage(), 0.0);
        assert_eq!(s.idle_since_boot(), None);
    }

    #[test]
    fn idle_since_boot_is_the_aggregate_idle_share() {
        let fx = machine(PROC_STAT);
        let s = Stats::new(&fx.host());
        let idle = s.idle_since_boot().expect("counters exist");

        assert!((idle - 8040.0 / 9370.0 * 100.0).abs() < 1e-9);
    }

    #[test]
    fn zram_rows_describe_the_compressed_device() {
        let fx = Fixture::new();
        fx.write("sys/block/zram0/disksize", "4294967296\n");
        fx.write(
            "sys/block/zram0/mm_stat",
            "1073741824 268435456 300000000 0\n",
        );
        fx.write("sys/block/zram0/comp_algorithm", "lzo lz4 [zstd]\n");

        let text = rows_text(&Memory::default().rows(&fx.host()));
        assert!(text.contains("# Compressed"), "{text}");
        assert!(text.contains("4.0x smaller"), "{text}");
        assert!(text.contains("Algorithm: zstd"), "{text}");
    }
}
