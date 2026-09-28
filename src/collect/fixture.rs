//! Throwaway directory trees that stand in for `/proc` and `/sys` in tests.
//!
//! We are std-only, so this replaces `tempfile`: a unique directory under the
//! system temp dir, removed again when the fixture is dropped.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::fs::Host;

#[derive(Debug)]
pub(crate) struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    pub(crate) fn new() -> Fixture {
        static N: AtomicU32 = AtomicU32::new(0);

        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "omarchy-sysinfo-fixture-{}-{n}",
            std::process::id()
        ));

        // A leftover from a crashed run with the same pid must not leak in.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");

        Fixture { dir }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn host(&self) -> Host {
        Host::fixture(&self.dir)
    }

    /// Write a file at a path relative to the fixture root, creating parents.
    pub(crate) fn write(&self, relative: &str, contents: &str) {
        self.write_bytes(relative, contents.as_bytes());
    }

    pub(crate) fn write_bytes(&self, relative: &str, contents: &[u8]) {
        let path = self.dir.join(relative);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(&path, contents).expect("write fixture file");
    }

    pub(crate) fn mkdir(&self, relative: &str) {
        std::fs::create_dir_all(self.dir.join(relative)).expect("create fixture dir");
    }

    /// A symlink at `relative` pointing at `target`, which is taken as given
    /// (so a relative target resolves the way sysfs links do).
    pub(crate) fn symlink(&self, relative: &str, target: &str) {
        let path = self.dir.join(relative);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::os::unix::fs::symlink(target, &path).expect("create fixture symlink");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
