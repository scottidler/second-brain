//! Test-only XDG sandbox flag; see the `sandbox` declaration in `receipts.rs`.

use std::cell::Cell;
use std::marker::PhantomData;

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// RAII flag for the current thread. `!Send` so it cannot follow a future
/// onto another thread where the flag would not be set.
pub(crate) struct Entered(PhantomData<*const ()>);

pub(crate) fn enter() -> Entered {
    DEPTH.with(|d| d.set(d.get() + 1));
    Entered(PhantomData)
}

impl Drop for Entered {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get() - 1));
    }
}

/// The current thread's sandbox state, carried into a `spawn_blocking`
/// closure the sandboxed thread awaits (`routes::queue`): the blocking
/// pool thread has no flag of its own, but runs inside the caller's
/// sandbox window because the caller holds the lock until it joins.
#[derive(Clone, Copy)]
pub(crate) struct Carried(bool);

pub(crate) fn carry() -> Carried {
    Carried(is_active())
}

impl Carried {
    pub(crate) fn enter(self) -> Option<Entered> {
        self.0.then(enter)
    }
}

pub(crate) fn is_active() -> bool {
    DEPTH.with(|d| d.get() > 0)
}

pub(crate) fn assert_active(caller: &str) {
    assert!(
        is_active(),
        "{caller} called outside an XDG sandbox: wrap the test in \
         `harvest::with_xdg_data_home` (or hold `sandbox::enter()` with \
         `TEST_XDG_LOCK` while `XDG_DATA_HOME` points at a tempdir), or it \
         opens the live receipts DB"
    );
}
