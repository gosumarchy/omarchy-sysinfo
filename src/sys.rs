//! Every foreign function this program calls, and nothing else.
//!
//! The standard library covers files, processes and sockets but not three
//! small corners of POSIX that a terminal program needs: the window size
//! (`ioctl(TIOCGWINSZ)`), waiting for input with a timeout (`poll`), and
//! filesystem usage (`statvfs`). Each is wrapped here in a safe function
//! whose signature cannot be misused, so no other module needs `unsafe`.
//!
//! The declarations are Linux-specific (struct layouts and request numbers
//! differ elsewhere). On other targets each wrapper reports "unavailable",
//! which the callers already handle, so the crate still builds and its tests
//! still run on a developer's non-Linux machine.

use std::path::Path;
use std::time::Duration;

/// A filesystem's size and free space, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FsUsage {
    pub(crate) size: u64,
    pub(crate) used: u64,
    /// Free space an unprivileged user can actually write, which is what `df`
    /// calls "Avail": it excludes the blocks reserved for root.
    pub(crate) avail: u64,
}

impl FsUsage {
    /// From `statvfs` block counts, the way `df` computes its columns.
    #[cfg_attr(
        not(any(test, all(target_os = "linux", target_pointer_width = "64"))),
        expect(dead_code, reason = "only the Linux statvfs wrapper builds one")
    )]
    pub(crate) fn from_blocks(fragment: u64, blocks: u64, free: u64, avail: u64) -> FsUsage {
        FsUsage {
            size: blocks.saturating_mul(fragment),
            used: blocks.saturating_sub(free).saturating_mul(fragment),
            avail: avail.saturating_mul(fragment),
        }
    }

    /// Used as a share of what a user could ever have, `df`'s `Use%`: the
    /// root reserve counts as neither used nor available.
    pub(crate) fn fraction(self) -> f64 {
        super::collect::units::fraction(self.used, self.used.saturating_add(self.avail))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_short, c_ulong};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// `struct winsize` from `<asm-generic/termios.h>`.
    #[repr(C)]
    #[expect(clippy::struct_field_names, reason = "the fields keep their C names")]
    #[derive(Default)]
    pub(super) struct Winsize {
        pub(super) ws_row: u16,
        pub(super) ws_col: u16,
        pub(super) ws_xpixel: u16,
        pub(super) ws_ypixel: u16,
    }

    /// `TIOCGWINSZ` from `<asm-generic/ioctls.h>`, which every architecture
    /// Arch Linux and Arch Linux ARM ship shares (32- and 64-bit x86 and
    /// ARM, plus RISC-V and `LoongArch`). PowerPC, MIPS and SPARC number it
    /// differently.
    #[cfg(any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    ))]
    pub(super) const TIOCGWINSZ: c_ulong = 0x5413;

    /// `struct pollfd` from `<poll.h>`.
    #[repr(C)]
    pub(super) struct PollFd {
        pub(super) fd: c_int,
        pub(super) events: c_short,
        pub(super) revents: c_short,
    }

    pub(super) const POLLIN: c_short = 0x001;

    /// `struct statvfs` as glibc and musl lay it out on 64-bit Linux.
    #[cfg(target_pointer_width = "64")]
    #[repr(C)]
    #[derive(Default)]
    #[expect(clippy::struct_field_names, reason = "the fields keep their C names")]
    pub(super) struct Statvfs {
        pub(super) f_bsize: c_ulong,
        pub(super) f_frsize: c_ulong,
        pub(super) f_blocks: u64,
        pub(super) f_bfree: u64,
        pub(super) f_bavail: u64,
        pub(super) f_files: u64,
        pub(super) f_ffree: u64,
        pub(super) f_favail: u64,
        pub(super) f_fsid: c_ulong,
        pub(super) f_flag: c_ulong,
        pub(super) f_namemax: c_ulong,
        pub(super) f_spare: [c_int; 6],
    }

    // The C library writes exactly this many bytes; a smaller Rust struct
    // would be overrun.
    #[cfg(target_pointer_width = "64")]
    const _: () = assert!(size_of::<Statvfs>() == 112);

    unsafe extern "C" {
        /// `int ioctl(int fd, unsigned long request, ...)`. Variadic in C, so
        /// it is declared variadic here: calling a variadic function through
        /// a non-variadic declaration is an ABI mismatch.
        pub(super) fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;

        /// `int poll(struct pollfd *fds, nfds_t nfds, int timeout)`, where
        /// `nfds_t` is `unsigned long` on Linux.
        pub(super) fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;

        #[cfg(target_pointer_width = "64")]
        pub(super) fn statvfs(path: *const c_char, buf: *mut Statvfs) -> c_int;
    }

    pub(super) fn c_path(path: &Path) -> Option<CString> {
        CString::new(path.as_os_str().as_bytes()).ok()
    }
}

/// The terminal size on `fd` as `(columns, rows)`, if `fd` is a terminal that
/// knows its size.
#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    )
))]
pub(crate) fn window_size(fd: i32) -> Option<(u16, u16)> {
    let mut size = linux::Winsize::default();

    // SAFETY: TIOCGWINSZ takes one `struct winsize *` argument and writes at
    // most `size_of::<winsize>()` bytes through it. `size` is a live, properly
    // aligned `Winsize` with the kernel's layout, borrowed mutably for the
    // duration of the call only. A bad or non-terminal `fd` makes the call
    // fail with -1 and leaves `size` untouched.
    let status = unsafe { linux::ioctl(fd, linux::TIOCGWINSZ, &raw mut size) };

    (status == 0 && size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
}

#[cfg(not(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    )
)))]
pub(crate) fn window_size(_fd: i32) -> Option<(u16, u16)> {
    None
}

/// Wait up to `timeout` for `fd` to have something to read.
///
/// `false` means the time ran out, or that this platform cannot wait, in
/// which case the caller simply stops waiting sooner.
#[cfg(target_os = "linux")]
pub(crate) fn wait_readable(fd: i32, timeout: Duration) -> bool {
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut fds = linux::PollFd {
        fd,
        events: linux::POLLIN,
        revents: 0,
    };

    // SAFETY: `fds` is a single live `pollfd` with the C layout, and `nfds`
    // is 1, so poll reads and writes exactly that one struct and nothing past
    // it. It holds no pointer to it after returning.
    let ready = unsafe { linux::poll(&raw mut fds, 1, millis) };

    // Zero is a timeout. A negative result is EINTR (a signal such as
    // SIGWINCH) or a bad fd; both read as "nothing yet".
    ready > 0
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn wait_readable(_fd: i32, _timeout: Duration) -> bool {
    false
}

/// Size and free space of the filesystem holding `path`.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub(crate) fn fs_usage(path: &Path) -> Option<FsUsage> {
    let path = linux::c_path(path)?;
    let mut buf = linux::Statvfs::default();

    // SAFETY: `path` is a NUL-terminated string that outlives the call, and
    // `buf` is a live `Statvfs` whose size is asserted above to match the C
    // library's `struct statvfs`, so the write stays inside it.
    let status = unsafe { linux::statvfs(path.as_ptr(), &raw mut buf) };
    if status != 0 {
        return None;
    }

    Some(FsUsage::from_blocks(
        buf.f_frsize,
        buf.f_blocks,
        buf.f_bfree,
        buf.f_bavail,
    ))
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub(crate) fn fs_usage(_path: &Path) -> Option<FsUsage> {
    None
}

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "these tests pin exact, exactly representable results"
)]
mod tests {
    use super::*;

    #[test]
    fn usage_follows_df_arithmetic() {
        // 100 blocks of 4 KiB, 40 free of which 30 are available to users.
        let usage = FsUsage::from_blocks(4096, 100, 40, 30);

        assert_eq!(usage.size, 409_600);
        assert_eq!(usage.used, 60 * 4096);
        assert_eq!(usage.avail, 30 * 4096);
        // 60 / (60 + 30): the 10 reserved blocks count for neither side.
        assert!((usage.fraction() - 60.0 / 90.0).abs() < 1e-12);
    }

    #[test]
    fn usage_of_nonsense_counts_saturates() {
        let usage = FsUsage::from_blocks(u64::MAX, u64::MAX, 0, u64::MAX);

        assert_eq!(usage.size, u64::MAX);
        assert!(usage.fraction().is_finite());
        assert_eq!(FsUsage::from_blocks(4096, 0, 0, 0).fraction(), 0.0);
    }

    #[test]
    fn a_file_descriptor_that_is_not_a_terminal_has_no_size() {
        assert_eq!(window_size(-1), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_root_filesystem_has_a_size() {
        let usage = fs_usage(Path::new("/")).expect("statvfs on /");

        assert!(usage.size > 0);
        assert!(usage.used <= usage.size);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_missing_path_has_no_usage() {
        assert_eq!(fs_usage(Path::new("/definitely/not/here")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn waiting_on_a_quiet_pipe_times_out() {
        use std::os::fd::AsRawFd;

        let (reader, _writer) = std::io::pipe().expect("pipe");

        assert!(!wait_readable(
            reader.as_raw_fd(),
            Duration::from_millis(10)
        ));
    }
}
