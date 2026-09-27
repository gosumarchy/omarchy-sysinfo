//! Reading files under `/proc` and `/sys`.
//!
//! The kernel reports absence by not creating a file, so "the device is not
//! there" and "the file is empty" both arrive as `None` and the collectors carry
//! on. A file that exists but cannot be *read* is a different story, and
//! [`try_read`] is how a caller tells the two apart.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Read a file and return its trimmed contents, or `None` if it holds nothing.
///
/// # What `None` means
///
/// A file that is not there is absence, and the collectors treat it as such. A
/// file that exists but cannot be read is *not* absence — a permission problem
/// and a missing device mean very different things to someone trying to work
/// out why a machine is misbehaving. `read` cannot tell them apart: both arrive
/// as `None`. Use [`try_read`] where the difference is worth reporting.
///
/// # Trimming
///
/// The result is already trimmed, and a file holding only whitespace reads as
/// `None`. Every parser in this crate relies on that, so it is part of the
/// contract rather than an accident of the implementation.
pub fn read(path: impl AsRef<Path>) -> Option<String> {
    try_read(path).ok().flatten()
}

/// Like [`read`], but separating "no such file" from "could not be read".
///
/// `Ok(None)` means the path is not there, or is there but empty. `Err(_)` means
/// it is there and reading it failed — a permission problem, a directory, a
/// dangling symlink.
pub fn try_read(path: impl AsRef<Path>) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let trimmed = raw.trim();

            Ok(if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Read a float, e.g. a temperature in millidegrees or a load average.
pub fn read_f64(path: impl AsRef<Path>) -> Option<f64> {
    read(path).and_then(|s| s.parse().ok())
}

/// Raw bytes, for files that are not text. An empty file is absence.
pub fn read_bytes(path: impl AsRef<Path>) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

/// Read a whole number, e.g. a capacity in bytes.
///
/// Parsed straight to `u64` rather than through [`read_f64`]: a value that is not
/// a whole number is unavailable, not zero, and an `f64` would quietly round off
/// anything above 2^53.
pub fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read(path).and_then(|s| s.parse().ok())
}

/// The entries of a directory, sorted so that repeated runs agree.
///
/// A directory that cannot be listed reads as empty, matching how the callers
/// treat a path that is not there.
pub fn list_dir(path: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut out: Vec<_> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();

    out.sort();

    out
}

/// Last path component of a `/sys` symlink target that we read as a string.
pub fn basename(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// `sysfs` exposes bound drivers as symlinks to `/sys/bus/pci/drivers/i915`,
/// which cannot be read as a file. This resolves one to its name.
pub fn driver_name(link: impl AsRef<Path>) -> Option<String> {
    let target = std::fs::read_link(link).ok()?;

    Some(basename(&target.to_string_lossy()))
}

/// The kernel command line. It does not change until reboot.
pub fn cmdline() -> Option<String> {
    static CACHED: OnceLock<Option<String>> = OnceLock::new();
    CACHED.get_or_init(|| read("/proc/cmdline")).clone()
}

/// The value the kernel was booted with for one `key=` on the command line.
pub fn kernel_param(key: &str) -> Option<String> {
    let cmdline = cmdline()?;
    let prefix = format!("{key}=");
    cmdline
        .split_whitespace()
        .find_map(|p| p.strip_prefix(&prefix).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch path so tests can exercise the real filesystem without a
    /// tempdir dependency. We are std-only, so this stands in for `tempfile`.
    fn scratch(name: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut p = std::env::temp_dir();

        p.push(format!(
            "omarchy-sysinfo-test-{}-{}-{}",
            std::process::id(),
            n,
            name
        ));

        p
    }

    fn write_scratch(name: &str, body: &str) -> PathBuf {
        let p = scratch(name);

        std::fs::write(&p, body).expect("write scratch file");

        p
    }

    // ---- read / read_f64 / read_u64 -------------------------------------

    #[test]
    fn read_trims_and_rejects_empty_or_blank_files() {
        let a = write_scratch("trim", "  hello  \n");
        assert_eq!(read(&a).as_deref(), Some("hello"));
        let b = write_scratch("blank", "\n\n   \n");
        assert_eq!(read(&b), None);
        let c = write_scratch("empty", "");
        assert_eq!(read(&c), None);

        for p in [a, b, c] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn read_returns_none_for_missing_files_and_directories() {
        assert_eq!(read(scratch("does-not-exist")), None);
        // A directory is not readable as a string either.
        let d = std::env::temp_dir();
        assert_eq!(read(&d), None);
    }

    #[test]
    fn read_f64_and_u64_handle_trailing_whitespace_and_garbage() {
        let a = write_scratch("f64", " 42.5 \n");
        assert_eq!(read_f64(&a), Some(42.5));
        let b = write_scratch("f64-bad", "not a number");
        assert_eq!(read_f64(&b), None);
        let c = write_scratch("u64", " 7 \n");
        assert_eq!(read_u64(&c), Some(7));

        for p in [a, b, c] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn read_u64_rejects_rather_than_truncating_garbage() {
        // A negative or fractional count is not zero, it is unreadable. Parsing
        // straight to `u64` says so, where routing through `f64` reported `0` and
        // quietly rounded off anything above 2^53.
        for (name, body) in [("u64-neg", "-3"), ("u64-frac", "12.7")] {
            let p = write_scratch(name, body);
            assert_eq!(read_u64(&p), None, "{name} ({body}) must not parse");
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn read_u64_keeps_precision_above_the_f64_exact_range() {
        // 2^53 + 1 is the smallest integer an f64 cannot represent, so the old
        // `as u64` cast rounded it to 2^53.
        let exact = 9_007_199_254_740_993u64;
        let p = write_scratch("u64-big", &format!("{exact}\n"));
        assert_eq!(read_u64(&p), Some(exact), "large counts must round-trip");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn try_read_separates_a_missing_file_from_an_unreadable_one() {
        // The whole point of `try_read`: `read` cannot tell these apart, so a
        // permission problem used to look exactly like absent hardware.
        let missing = scratch("definitely-not-here-xyz");
        assert_eq!(
            try_read(&missing).expect("a missing file is not an error"),
            None
        );
        // A directory exists but cannot be read as a string, so it is an error
        // rather than absence.
        let dir = std::env::temp_dir();
        assert!(
            try_read(&dir).is_err(),
            "a directory must not read as absent"
        );
        // And `read` still collapses both to `None` for the collectors that do
        // not care about the difference.
        assert_eq!(read(&missing), None);
        assert_eq!(read(&dir), None);
    }

    // ---- list_dir --------------------------------------------------------

    #[test]
    fn list_dir_is_sorted_and_tolerates_missing_paths() {
        assert!(list_dir(scratch("nope-not-here")).is_empty());
        // A real directory lists deterministically.
        let out = list_dir("/sys/class/net");
        let mut sorted = out.clone();
        sorted.sort();
        assert_eq!(out, sorted);
    }

    // ---- basename --------------------------------------------------------

    #[test]
    fn basename_takes_the_last_component() {
        assert_eq!(basename("/sys/bus/pci/drivers/i915"), "i915");
        assert_eq!(basename("i915"), "i915");
        assert_eq!(basename("a/b/c/"), "c");
    }

    #[test]
    fn basename_survives_degenerate_paths() {
        // No non-empty component to find: fall back to the input rather than
        // panicking or returning an empty label.
        assert_eq!(basename("/"), "/");
        assert_eq!(basename(""), "");
    }

    // ---- kernel_param ----------------------------------------------------

    #[test]
    fn kernel_param_requires_the_exact_key_prefix() {
        let cmdline = read("/proc/cmdline");

        if let Some(cmdline) = cmdline {
            for token in cmdline.split_whitespace() {
                if let Some((k, v)) = token.split_once('=') {
                    assert_eq!(
                        kernel_param(k).as_deref(),
                        Some(v),
                        "kernel_param must round-trip every cmdline key"
                    );
                }
            }
        }

        // A key nobody passes must be absent, and must not match a longer key
        // that merely starts with the same letters.
        assert_eq!(kernel_param("definitely_not_a_real_param_xyz"), None);
    }
}
