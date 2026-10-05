//! Test-only helpers shared across oracle's test modules.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Points `$HOME` at a fixture directory for the life of the guard, holding
/// the crate-wide lock so no other test sees the change. `~/...` in a config
/// then resolves under the fixture, never under the real home.
pub(crate) struct HomeGuard {
    original: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl HomeGuard {
    pub(crate) fn set(home: &Path) -> Self {
        let lock = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let original = std::env::var_os("HOME");
        // SAFETY: serialized by HOME_LOCK; restored in Drop.
        unsafe { std::env::set_var("HOME", home) };
        Self { original, _lock: lock }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: still holding HOME_LOCK.
        unsafe {
            match self.original.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

/// Write a borg.yml fixture into `dir` and return its path.
pub(crate) fn write_borg_yml(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("borg.yml");
    std::fs::write(&path, body).expect("write borg.yml");
    path
}

/// Write a canonical-tags file holding `tags` under one group.
pub(crate) fn write_vocab(path: &Path, tags: &[&str]) {
    let list = tags.iter().map(|t| format!("    - {t}\n")).collect::<String>();
    std::fs::write(path, format!("max-per-note: 7\ntags:\n  fixture:\n{list}")).expect("write vocab");
}
