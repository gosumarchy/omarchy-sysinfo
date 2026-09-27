//! Live counters parsed straight out of `/proc`. Two samples are needed before
//! CPU usage means anything, so `sample()` is called repeatedly over time.

use super::{
    fs::{read, read_u64},
    Row,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Times {
    total: u64,
    idle: u64,
}

impl Times {
    /// Usage as a percentage of the interval between two samples.
    fn usage_since(&self, prev: &Times) -> Option<f64> {
        let d_total = self.total.checked_sub(prev.total)?;
        let d_idle = self.idle.checked_sub(prev.idle)?;
        if d_total == 0 {
            return None;
        }
        // Idle can legitimately exceed the total across a hotplug or a kernel
        // that added a counter, so this must not wrap: a wrapped subtraction
        // reported a wildly wrong percentage and panicked in debug builds.
        let busy = d_total.saturating_sub(d_idle);
        Some((busy as f64 / d_total as f64) * 100.0)
    }
}

#[derive(Clone, Copy, Default)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub free: u64,
    pub cached: u64,
    pub buffers: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    pub huge_pages_total: u64,
    pub huge_pages_free: u64,
    pub swap_devices: usize,
}

impl Memory {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Header("Memory".into())];
        rows.push(Row::field_with(
            "Physical",
            format!(
                "{} / {}",
                super::units::human_bytes(self.used()),
                super::units::human_bytes(self.total)
            ),
            fraction(self.used(), self.total),
        ));
        rows.push(Row::field(
            "Available",
            super::units::human_bytes(self.available),
        ));
        rows.push(Row::field("Free", super::units::human_bytes(self.free)));
        if self.cached > 0 {
            rows.push(Row::field(
                "Page cache",
                super::units::human_bytes(self.cached),
            ));
        }
        if self.buffers > 0 {
            rows.push(Row::field(
                "Buffers",
                super::units::human_bytes(self.buffers),
            ));
        }

        if self.swap_total > 0 {
            rows.push(Row::Header("Swap".into()));
            rows.push(Row::field_with(
                "Total",
                format!(
                    "{} / {}",
                    super::units::human_bytes(self.swap_used()),
                    super::units::human_bytes(self.swap_total)
                ),
                fraction(self.swap_used(), self.swap_total),
            ));
            rows.push(Row::field("Swap devices", self.swap_devices.to_string()));
        }

        // zram is compressed, so its backing size is not its useful size.
        if let Some(zram) = zram_rows() {
            rows.push(Row::Header("Compressed".into()));
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

fn zram_rows() -> Option<Vec<Row>> {
    use std::path::Path;
    let base = Path::new("/sys/block/zram0");
    if !base.exists() {
        return None;
    }
    let disksize = read_u64(base.join("disksize"))?;
    // mm_stat is whitespace separated and in bytes: how much data is stored
    // uncompressed, how much it compresses down to, then the memory cost.
    let (orig, compressed) = parse_mm_stat(&read(base.join("mm_stat"))?)?;

    let mut rows = vec![Row::field_with(
        "zram0",
        format!(
            "{} stored, {} on disk",
            super::units::human_bytes(orig),
            super::units::human_bytes(compressed)
        ),
        fraction(orig, disksize),
    )];
    if orig > 0 && compressed > 0 {
        rows.push(Row::field(
            "Ratio",
            format!("{:.1}x smaller", orig as f64 / compressed as f64),
        ));
    }
    if let Some(algorithm) = read(base.join("comp_algorithm")) {
        match active_algorithm(&algorithm) {
            Some(name) => rows.push(Row::field("Algorithm", name.to_string())),
            None => rows.push(Row::field("Algorithms", algorithm)),
        }
    }
    Some(rows)
}

fn fraction(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Everything that has to be sampled over time to show a live figure.
pub struct Stats {
    prev_total: Times,
    prev_cores: Vec<Times>,
    per_core: Vec<f64>,
    core_names: Vec<String>,
    memory: Memory,
    uptime: u64,
    load: (f64, f64, f64),
    runnable: (u32, u32),
    booted_ago: u64,
    /// When the counters were last read, and whether usage is a real delta yet.
    last_sample: Option<Instant>,
    primed: bool,
}

/// Below this interval a `/proc/stat` delta is mostly scheduler noise.
const MIN_INTERVAL: Duration = Duration::from_millis(100);

impl Stats {
    pub fn new() -> Stats {
        let mut stats = Stats {
            prev_total: Times::default(),
            prev_cores: Vec::new(),
            per_core: Vec::new(),
            core_names: Vec::new(),
            memory: Memory::default(),
            uptime: 0,
            load: (0.0, 0.0, 0.0),
            runnable: (0, 0),
            booted_ago: 0,
            last_sample: None,
            primed: false,
        };
        stats.sample();
        stats
    }

    /// Whether per-core usage is a real measurement yet, as opposed to a
    /// placeholder for reads that were too close together to mean anything.
    pub fn primed(&self) -> bool {
        self.primed
    }

    /// Re-read `/proc/stat`, `/proc/meminfo`, `/proc/uptime` and `/proc/loadavg`.
    pub fn sample(&mut self) {
        // Usage is a difference between two reads, so it is only meaningful
        // once enough wall-clock time has passed for the counters to move.
        // Back-to-back reads would otherwise report a confident zero.
        let now = Instant::now();
        let elapsed = self.last_sample.map(|t| now.duration_since(t));
        let (total, cores, names) = read_cpu_times();

        if elapsed.is_some_and(|e| e >= MIN_INTERVAL) {
            self.per_core = cores
                .iter()
                .enumerate()
                .map(|(i, current)| {
                    current
                        .usage_since(self.prev_cores.get(i).unwrap_or(&Times::default()))
                        .unwrap_or(0.0)
                        .clamp(0.0, 100.0)
                })
                .collect();
            self.primed = true;
        } else if self.per_core.len() != cores.len() {
            // Keep the row count stable so the UI does not jump around.
            self.per_core = vec![0.0; cores.len()];
        }
        if names.is_empty() {
            self.core_names = (0..cores.len()).map(|i| format!("cpu{i}")).collect();
        } else {
            self.core_names = names;
        }
        self.prev_total = total;
        self.prev_cores = cores;
        self.last_sample = Some(now);

        self.memory = read_memory();
        self.uptime = read("/proc/uptime").map(|u| parse_uptime(&u)).unwrap_or(0);
        self.booted_ago = self.uptime;

        if let Some(loadavg) = read("/proc/loadavg") {
            let (load, runnable) = parse_loadavg(&loadavg);
            self.load = load;
            self.runnable = runnable;
        }
    }

    pub fn cpu_usage(&self) -> f64 {
        if self.per_core.is_empty() {
            return 0.0;
        }
        self.per_core.iter().sum::<f64>() / self.per_core.len() as f64
    }

    pub fn per_core(&self) -> &[f64] {
        &self.per_core
    }

    pub fn core_names(&self) -> &[String] {
        &self.core_names
    }

    pub fn memory(&self) -> Memory {
        self.memory
    }

    pub fn uptime(&self) -> u64 {
        self.uptime
    }

    pub fn load(&self) -> (f64, f64, f64) {
        self.load
    }

    /// How many threads are runnable right now, and total threads.
    pub fn process_load(&self) -> String {
        let (running, total) = self.runnable;
        format!("{running} runnable / {total} threads")
    }

    /// The kernel's own idle ratio since boot, from the aggregate `cpu` line
    /// `sample` already parsed.
    pub fn idle_since_boot(&self) -> Option<f64> {
        if self.prev_total.total == 0 {
            return None;
        }
        Some(self.prev_total.idle as f64 / self.prev_total.total as f64 * 100.0)
    }
}

/// Aggregate and per-core jiffy counters from `/proc/stat`.
fn read_cpu_times() -> (Times, Vec<Times>, Vec<String>) {
    match read("/proc/stat") {
        Some(stat) => parse_cpu_times(&stat),
        None => (Times::default(), Vec::new(), Vec::new()),
    }
}

/// The `cpu` and `cpuN` lines, and nothing else. Split out from the file read
/// so the parsing can be driven by a fixture.
fn parse_cpu_times(stat: &str) -> (Times, Vec<Times>, Vec<String>) {
    let mut total = Times::default();
    let mut cores = Vec::new();
    let mut names = Vec::new();

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
        // user nice system idle iowait irq softirq steal ...
        // A missing iowait (exactly four columns) is zero, not a panic.
        let mut sum = 0u64;
        let mut idle = 0u64;
        let mut n = 0usize;
        for value in fields {
            let Ok(value) = value.parse::<u64>() else {
                continue;
            };
            sum += value;
            if n == 3 || n == 4 {
                idle += value;
            }
            n += 1;
        }
        if n < 4 {
            continue;
        }
        let times = Times { total: sum, idle };

        if label == "cpu" {
            total = times;
        } else {
            cores.push(times);
            names.push(label.to_string());
        }
    }
    (total, cores, names)
}

fn read_memory() -> Memory {
    let mut mem = match read("/proc/meminfo") {
        Some(info) => parse_memory(&info),
        None => Memory::default(),
    };
    mem.swap_devices = read("/proc/swaps")
        .map(|s| swap_device_count(&s))
        .unwrap_or(0);
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
        match key {
            "MemTotal:" => mem.total = raw * 1024,
            "MemAvailable:" => mem.available = raw * 1024,
            "MemFree:" => mem.free = raw * 1024,
            "Cached:" => mem.cached = raw * 1024,
            "Buffers:" => mem.buffers = raw * 1024,
            "SwapTotal:" => mem.swap_total = raw * 1024,
            "SwapFree:" => mem.swap_free = raw * 1024,
            "HugePages_Total:" => mem.huge_pages_total = raw,
            "HugePages_Free:" => mem.huge_pages_free = raw,
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
fn parse_loadavg(text: &str) -> ((f64, f64, f64), (u32, u32)) {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let num = |i: usize| parts.get(i).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let load = (num(0), num(1), num(2));
    let mut runnable = (0, 0);
    if let Some(tasks) = parts.get(3) {
        let bits: Vec<&str> = tasks.split('/').collect();
        if bits.len() == 2 {
            runnable = (bits[0].parse().unwrap_or(0), bits[1].parse().unwrap_or(0));
        }
    }
    (load, runnable)
}

/// `/proc/uptime` is seconds with a fractional part.
fn parse_uptime(text: &str) -> u64 {
    text.split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|s| s as u64)
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
mod tests {
    use super::*;

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
        assert_eq!(now.usage_since(&prev), Some(50.0));
    }

    #[test]
    fn usage_is_none_when_the_clock_did_not_move() {
        let t = Times {
            total: 100,
            idle: 50,
        };
        assert_eq!(t.usage_since(&t), None, "an identical sample means nothing");
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
        assert_eq!(now.usage_since(&prev), None);
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
        let usage = now.usage_since(&prev).expect("a delta exists");
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
            .usage_since(&prev),
            Some(100.0)
        );
        assert_eq!(
            Times {
                total: 100,
                idle: 100
            }
            .usage_since(&prev),
            Some(0.0)
        );
    }

    // ---- parse_cpu_times -------------------------------------------------

    #[test]
    fn parse_cpu_times_splits_the_aggregate_from_the_cores() {
        let (total, cores, names) = parse_cpu_times(PROC_STAT);
        // 1000+20+300+8000+40+0+10 = 9370, idle 8000+40 = 8040
        assert_eq!(
            total,
            Times {
                total: 9370,
                idle: 8040
            }
        );
        assert_eq!(cores.len(), 2);
        assert_eq!(names, vec!["cpu0".to_string(), "cpu1".to_string()]);
        assert_eq!(
            cores[0],
            Times {
                total: 4685,
                idle: 4020
            }
        );
    }

    #[test]
    fn parse_cpu_times_ignores_unrelated_lines() {
        let (_, cores, names) = parse_cpu_times(PROC_STAT);
        assert_eq!(cores.len(), 2, "only cpuN lines become cores");
        assert!(names.iter().all(|n| n.starts_with("cpu") && n.len() == 4));
    }

    #[test]
    fn parse_cpu_times_ignores_a_cpu_prefixed_line_that_is_not_a_core() {
        // A bare starts_with("cpu") used to let these through as cores.
        let stat = "cpu 1 1 1 1 1\ncpufreq 5 5 5 5\ncpufoo 1 1 1 1\ncpu 1 1\n";
        let (total, cores, names) = parse_cpu_times(stat);
        assert_eq!(cores.len(), 0, "cpufreq/cpufoo are not cores");
        assert!(names.is_empty());
        assert_ne!(total, Times::default(), "the real cpu line still parses");
    }

    #[test]
    fn parse_cpu_times_skips_a_cpu_line_with_too_few_columns() {
        // Fewer than four values means we cannot tell idle from busy.
        let (total, cores, _) = parse_cpu_times("cpu 1 2 3\ncpu0 1 2 3\n");
        assert_eq!(total, Times::default());
        assert_eq!(cores.len(), 0);
    }

    #[test]
    fn parse_cpu_times_treats_a_missing_iowait_as_zero() {
        // Four columns is a legal older `/proc/stat`. Indexing iowait used to panic.
        let (total, cores, _) = parse_cpu_times("cpu 10 0 0 90\ncpu0 10 0 0 90\n");
        assert_eq!(
            total,
            Times {
                total: 100,
                idle: 90
            }
        );
        assert_eq!(cores.len(), 1);
        assert_eq!(cores[0].idle, 90);
    }

    #[test]
    fn parse_cpu_times_handles_a_single_core_machine() {
        let (total, cores, names) = parse_cpu_times("cpu 10 0 0 90 0\ncpu0 10 0 0 90 0\n");
        assert_eq!(
            total,
            Times {
                total: 100,
                idle: 90
            }
        );
        assert_eq!(cores.len(), 1);
        assert_eq!(names, vec!["cpu0".to_string()]);
    }

    #[test]
    fn parse_cpu_times_survives_junk() {
        assert_eq!(parse_cpu_times(""), (Times::default(), vec![], vec![]));
        assert_eq!(
            parse_cpu_times("\n\n\n"),
            (Times::default(), vec![], vec![])
        );
        // Non-numeric columns are dropped, which can leave too few to use.
        let (total, cores, _) = parse_cpu_times("cpu a b c d e f\ncpu0 a b c d e f\n");
        assert_eq!(total, Times::default());
        assert_eq!(cores.len(), 0);
    }

    #[test]
    fn parse_cpu_times_includes_steal_time_in_the_total() {
        // guest/guest_nice are already counted in user/nice, so summing every
        // column slightly overstates the total. The real kernel also has fewer
        // columns on older kernels; either way the ratio stays sane.
        let (total, _, _) = parse_cpu_times("cpu 100 0 0 800 0 0 0 50\n");
        assert_eq!(total.total, 950);
        assert_eq!(total.idle, 800);
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
        let text = rows_text(&m.rows());
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
        let text = rows_text(&m.rows());
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
        let (load, tasks) = parse_loadavg("0.52 0.58 0.59 2/431 12345");
        assert_eq!(load, (0.52, 0.58, 0.59));
        assert_eq!(tasks, (2, 431));
    }

    #[test]
    fn parse_loadavg_tolerates_missing_and_malformed_fields() {
        assert_eq!(parse_loadavg("").0, (0.0, 0.0, 0.0));
        assert_eq!(parse_loadavg("0.5").0, (0.5, 0.0, 0.0));
        assert_eq!(parse_loadavg("a b c 1/2").0, (0.0, 0.0, 0.0));
        assert_eq!(parse_loadavg("0.1 0.2 0.3 1/2").1, (1, 2));
        // A task field that is not "n/m" leaves the counts at zero.
        assert_eq!(parse_loadavg("0.1 0.2 0.3 7").1, (0, 0));
        // A half-written "n/" or "/m" still has two fields, so it parses
        // partially rather than being rejected outright.
        assert_eq!(parse_loadavg("0.1 0.2 0.3 7/").1, (7, 0));
        assert_eq!(parse_loadavg("0.1 0.2 0.3 /9").1, (0, 9));
        assert_eq!(parse_loadavg("0.1 0.2 0.3 1/2/3").1, (0, 0));
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
            Some((1048576, 262144))
        );
        assert_eq!(parse_mm_stat("  10   20  30 "), Some((10, 20)));
    }

    #[test]
    fn parse_mm_stat_defaults_a_missing_second_column_to_zero() {
        assert_eq!(parse_mm_stat("1048576"), Some((1048576, 0)));
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

    // ---- Stats (against the live machine) --------------------------------

    #[test]
    fn new_stats_reads_this_machine_without_panicking() {
        let s = Stats::new();
        assert!(!s.per_core().is_empty(), "a Linux machine has cores");
        assert!(s.uptime() > 0, "the machine is up");
        assert!(s.memory().total > 0, "meminfo should be readable");
        assert_eq!(s.load().0, s.load().0, "load must not be NaN");
    }

    #[test]
    fn usage_is_not_reported_before_a_real_interval_has_passed() {
        // A back-to-back read would otherwise claim a confident 0%.
        let mut s = Stats::new();
        assert!(!s.primed(), "the very first sample is not a measurement");
        s.sample();
        assert!(!s.primed(), "still too soon to mean anything");
    }

    #[test]
    fn sampling_twice_keeps_the_row_count_stable() {
        let mut s = Stats::new();
        let before = s.per_core().len();
        s.sample();
        assert_eq!(s.per_core().len(), before);
        assert_eq!(s.core_names().len(), before);
    }

    #[test]
    fn cpu_usage_is_the_mean_of_the_cores_and_within_range() {
        let s = Stats::new();
        let usage = s.cpu_usage();
        assert!(usage.is_finite());
        assert!((0.0..=100.0).contains(&usage), "out of range: {usage}");
        for core in s.per_core() {
            assert!(
                (0.0..=100.0).contains(core),
                "per-core usage out of range: {core}"
            );
        }
    }

    #[test]
    fn idle_since_boot_is_a_percentage_when_counters_exist() {
        let s = Stats::new();
        if let Some(idle) = s.idle_since_boot() {
            assert!((0.0..=100.0).contains(&idle), "out of range: {idle}");
        }
    }

    #[test]
    fn process_load_mentions_both_numbers() {
        let s = Stats::new();
        let text = s.process_load();
        assert!(text.contains("runnable"), "{text}");
        assert!(text.contains("threads"), "{text}");
    }

    #[test]
    fn fraction_of_nothing_is_zero_not_nan() {
        assert_eq!(fraction(5, 0), 0.0);
        assert_eq!(fraction(0, 0), 0.0);
        assert_eq!(fraction(5, 10), 0.5);
    }
}
