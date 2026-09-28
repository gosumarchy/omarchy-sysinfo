//! Reading files under `/proc` and `/sys`, relative to the machine being
//! described.
//!
//! The kernel reports absence by not creating a file, so "the device is not
//! there" and "the file is empty" both arrive as `None` and the collectors carry
//! on. A file that exists but cannot be *read* is a different story, and
//! [`try_read`] is how a caller tells the two apart.
//!
//! Every collector reaches the system through a [`Host`]. On a real run it is
//! rooted at `/`; in tests it is rooted at a fixture tree, which is what lets
//! a discrete GPU behind a bridge, or a LUKS root, be tested on any machine.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::command;

/// The machine a report describes: where its filesystem starts, where its
/// user's home is, and whether it may run programs.
#[derive(Clone, Debug)]
pub(crate) struct Host {
    root: PathBuf,
    home: Option<PathBuf>,
    programs: Programs,
    hypr_socket: Option<PathBuf>,
}

/// Whether collectors may start other programs to ask them questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Programs {
    Run,
    /// A fixture has no `hyprctl` or `iw` behind it, and a test must never
    /// pick up whatever the developer's machine happens to have installed.
    #[cfg_attr(not(test), expect(dead_code, reason = "only fixtures forbid programs"))]
    Never,
}

/// How long any one helper program gets before it is killed. A hung mount or
/// a wedged compositor must cost a blank row, not a frozen screen.
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(2);

impl Host {
    /// This machine, as the current user sees it.
    pub(crate) fn live() -> Host {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);

        Host {
            root: PathBuf::from("/"),
            home,
            programs: Programs::Run,
            hypr_socket: super::hypr::socket_from_env(),
        }
    }

    /// A machine that exists only as a directory tree, for tests.
    #[cfg(test)]
    pub(crate) fn fixture(root: &Path) -> Host {
        Host {
            root: root.to_path_buf(),
            home: Some(root.join("home/user")),
            programs: Programs::Never,
            hypr_socket: None,
        }
    }

    /// Where a system path such as `/sys/class/drm` lives on this host.
    pub(crate) fn path(&self, system_path: impl AsRef<Path>) -> PathBuf {
        let system_path = system_path.as_ref();
        let relative = system_path.strip_prefix("/").unwrap_or(system_path);

        self.root.join(relative)
    }

    /// A path under the user's home, or `None` when there is no home to look in.
    ///
    /// Guessing `/root` for a missing `$HOME` used to read another account's
    /// configuration and present it as this user's.
    pub(crate) fn home_path(&self, relative: impl AsRef<Path>) -> Option<PathBuf> {
        Some(self.home.as_ref()?.join(relative))
    }

    /// Run a helper program with a deadline, returning its trimmed stdout on
    /// success.
    pub(crate) fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        match self.programs {
            Programs::Run => command::run(program, args, PROGRAM_TIMEOUT),
            Programs::Never => None,
        }
    }

    /// Ask the running Hyprland instance a question over its socket.
    pub(crate) fn hypr(&self, request: &str) -> Option<String> {
        super::hypr::request(self.hypr_socket.as_deref()?, request)
    }

    // ---- reads relative to the root --------------------------------------

    pub(crate) fn read(&self, system_path: impl AsRef<Path>) -> Option<String> {
        read(self.path(system_path))
    }

    pub(crate) fn read_u64(&self, system_path: impl AsRef<Path>) -> Option<u64> {
        read_u64(self.path(system_path))
    }

    pub(crate) fn list_dir(&self, system_path: impl AsRef<Path>) -> Vec<PathBuf> {
        list_dir(self.path(system_path))
    }

    pub(crate) fn exists(&self, system_path: impl AsRef<Path>) -> bool {
        self.path(system_path).exists()
    }

    /// The kernel command line.
    pub(crate) fn cmdline(&self) -> Option<String> {
        self.read("/proc/cmdline")
    }

    /// The value the kernel was booted with for one `key=` on the command line.
    pub(crate) fn kernel_param(&self, key: &str) -> Option<String> {
        kernel_param(&self.cmdline()?, key)
    }
}

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
pub(crate) fn read(path: impl AsRef<Path>) -> Option<String> {
    try_read(path).ok().flatten()
}

/// Like [`read`], but separating "no such file" from "could not be read".
///
/// `Ok(None)` means the path is not there, or is there but empty. `Err(_)` means
/// it is there and reading it failed — a permission problem, a directory, a
/// dangling symlink.
pub(crate) fn try_read(path: impl AsRef<Path>) -> std::io::Result<Option<String>> {
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
pub(crate) fn read_f64(path: impl AsRef<Path>) -> Option<f64> {
    read(path).and_then(|s| s.parse().ok())
}

/// Raw bytes, for files that are not text. An empty file is absence.
pub(crate) fn read_bytes(path: impl AsRef<Path>) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;

    (!bytes.is_empty()).then_some(bytes)
}

/// Read a whole number, e.g. a capacity in bytes.
///
/// Parsed straight to `u64` rather than through [`read_f64`]: a value that is not
/// a whole number is unavailable, not zero, and an `f64` would quietly round off
/// anything above 2^53.
pub(crate) fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read(path).and_then(|s| s.parse().ok())
}

/// The entries of a directory, sorted so that repeated runs agree.
///
/// A directory that cannot be listed reads as empty, matching how the callers
/// treat a path that is not there.
pub(crate) fn list_dir(path: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut out: Vec<_> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();

    out.sort();

    out
}

/// The last component of a path as a `String`, or empty when it has none.
pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Last path component of a `/sys` symlink target that we read as a string.
pub(crate) fn basename(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// `sysfs` exposes bound drivers as symlinks to `/sys/bus/pci/drivers/i915`,
/// which cannot be read as a file. This resolves one to its name.
pub(crate) fn driver_name(link: impl AsRef<Path>) -> Option<String> {
    let target = std::fs::read_link(link).ok()?;

    Some(basename(&target.to_string_lossy()))
}

/// One `key=value` out of a kernel command line.
fn kernel_param(cmdline: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");

    cmdline
        .split_whitespace()
        .find_map(|p| p.strip_prefix(&prefix).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fixture::Fixture;

    // ---- Host ------------------------------------------------------------

    #[test]
    fn a_live_host_maps_system_paths_onto_themselves() {
        let host = Host::live();

        assert_eq!(host.path("/sys/class/drm"), PathBuf::from("/sys/class/drm"));
    }

    #[test]
    fn a_fixture_host_maps_system_paths_under_its_root() {
        let fx = Fixture::new();
        let host = fx.host();

        assert_eq!(host.path("/sys/class/drm"), fx.dir().join("sys/class/drm"));
        assert_eq!(host.path("proc/stat"), fx.dir().join("proc/stat"));
    }

    #[test]
    fn a_fixture_host_never_runs_programs() {
        let fx = Fixture::new();

        assert_eq!(fx.host().run("echo", &["hi"]), None);
        assert_eq!(fx.host().hypr("monitors"), None);
    }

    #[test]
    fn home_path_is_none_without_a_home() {
        let mut host = Host::live();
        host.home = None;

        assert_eq!(host.home_path(".config"), None);
    }

    // ---- read / read_f64 / read_u64 -------------------------------------

    #[test]
    fn read_trims_and_rejects_empty_or_blank_files() {
        let fx = Fixture::new();
        fx.write("trim", "  hello  \n");
        fx.write("blank", "\n\n   \n");
        fx.write("empty", "");

        assert_eq!(read(fx.dir().join("trim")).as_deref(), Some("hello"));
        assert_eq!(read(fx.dir().join("blank")), None);
        assert_eq!(read(fx.dir().join("empty")), None);
    }

    #[test]
    fn read_returns_none_for_missing_files_and_directories() {
        let fx = Fixture::new();

        assert_eq!(read(fx.dir().join("does-not-exist")), None);
        // A directory is not readable as a string either.
        assert_eq!(read(fx.dir()), None);
    }

    #[test]
    fn read_f64_and_u64_handle_trailing_whitespace_and_garbage() {
        let fx = Fixture::new();
        fx.write("f64", " 42.5 \n");
        fx.write("f64-bad", "not a number");
        fx.write("u64", " 7 \n");

        assert_eq!(read_f64(fx.dir().join("f64")), Some(42.5));
        assert_eq!(read_f64(fx.dir().join("f64-bad")), None);
        assert_eq!(read_u64(fx.dir().join("u64")), Some(7));
    }

    #[test]
    fn read_u64_rejects_rather_than_truncating_garbage() {
        // A negative or fractional count is not zero, it is unreadable. Parsing
        // straight to `u64` says so, where routing through `f64` reported `0` and
        // quietly rounded off anything above 2^53.
        let fx = Fixture::new();

        for (name, body) in [("u64-neg", "-3"), ("u64-frac", "12.7")] {
            fx.write(name, body);
            assert_eq!(
                read_u64(fx.dir().join(name)),
                None,
                "{name} ({body}) must not parse"
            );
        }
    }

    #[test]
    fn read_u64_keeps_precision_above_the_f64_exact_range() {
        // 2^53 + 1 is the smallest integer an f64 cannot represent, so the old
        // `as u64` cast rounded it to 2^53.
        let exact = 9_007_199_254_740_993u64;
        let fx = Fixture::new();
        fx.write("u64-big", &format!("{exact}\n"));

        assert_eq!(
            read_u64(fx.dir().join("u64-big")),
            Some(exact),
            "large counts must round-trip"
        );
    }

    #[test]
    fn try_read_separates_a_missing_file_from_an_unreadable_one() {
        // The whole point of `try_read`: `read` cannot tell these apart, so a
        // permission problem used to look exactly like absent hardware.
        let fx = Fixture::new();
        let missing = fx.dir().join("definitely-not-here-xyz");

        assert_eq!(
            try_read(&missing).expect("a missing file is not an error"),
            None
        );
        // A directory exists but cannot be read as a string, so it is an error
        // rather than absence.
        assert!(
            try_read(fx.dir()).is_err(),
            "a directory must not read as absent"
        );
        // And `read` still collapses both to `None` for the collectors that do
        // not care about the difference.
        assert_eq!(read(&missing), None);
        assert_eq!(read(fx.dir()), None);
    }

    // ---- list_dir --------------------------------------------------------

    #[test]
    fn list_dir_is_sorted_and_tolerates_missing_paths() {
        let fx = Fixture::new();
        fx.write("d/b", "");
        fx.write("d/a", "");
        fx.write("d/c", "");

        assert!(list_dir(fx.dir().join("nope-not-here")).is_empty());

        let names: Vec<String> = list_dir(fx.dir().join("d"))
            .iter()
            .map(|p| file_name(p))
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
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
        let cmdline = "quiet splash root=/dev/mapper/root rootflags=subvol=@ zswap.enabled=0";

        assert_eq!(
            kernel_param(cmdline, "root").as_deref(),
            Some("/dev/mapper/root")
        );
        assert_eq!(
            kernel_param(cmdline, "rootflags").as_deref(),
            Some("subvol=@")
        );
        assert_eq!(kernel_param(cmdline, "zswap.enabled").as_deref(), Some("0"));
        // A bare flag has no value, and a key must not match a longer key that
        // merely starts with the same letters.
        assert_eq!(kernel_param(cmdline, "quiet"), None);
        assert_eq!(kernel_param(cmdline, "roo"), None);
    }

    #[test]
    fn host_kernel_param_reads_the_fixture_cmdline() {
        let fx = Fixture::new();
        fx.write(
            "proc/cmdline",
            "BOOT_IMAGE=/vmlinuz-linux mitigations=off\n",
        );

        assert_eq!(
            fx.host().kernel_param("mitigations").as_deref(),
            Some("off")
        );
    }
}
