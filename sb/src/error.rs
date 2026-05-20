//! Eyre `Report` display customization.
//!
//! The default `eyre::Report` Debug impl - which is what `fn main() -> Result<()>`
//! uses to print the error chain - includes the `Location: <file>:<line>:<col>`
//! line from each `eyre!` / `wrap_err` call site. That noise leaks internal source
//! paths to users who hit a bad arg or a misconfigured vault.
//!
//! This hook keeps the error message and `Caused by:` chain, drops the Location
//! line, and only restores it when verbose mode is on or `RUST_BACKTRACE=1` is set.

use std::error::Error;
use std::fmt;
use std::panic::Location;

use eyre::EyreHandler;

/// Install our custom handler. Call once, before any `eyre::Report` is constructed
/// (so before `Cli::parse()` results are unwrapped).
///
/// `verbose` is a snapshot of the CLI flag; the runtime env vars `RUST_BACKTRACE`
/// and `RUST_LIB_BACKTRACE` also re-enable the verbose path.
pub fn install(verbose: bool) {
    // `set_hook` returns Err if a hook is already installed (e.g. running tests
    // that also set up eyre). That's fine - the existing hook is good enough.
    let _ = eyre::set_hook(Box::new(move |_| {
        Box::new(Handler {
            verbose,
            location: None,
        })
    }));
}

struct Handler {
    verbose: bool,
    location: Option<&'static Location<'static>>,
}

impl Handler {
    fn show_verbose(&self) -> bool {
        if self.verbose {
            return true;
        }
        std::env::var_os("RUST_BACKTRACE").is_some_and(|v| v != "0")
            || std::env::var_os("RUST_LIB_BACKTRACE").is_some_and(|v| v != "0")
    }
}

impl EyreHandler for Handler {
    fn track_caller(&mut self, location: &'static Location<'static>) {
        self.location = Some(location);
    }

    fn debug(&self, error: &(dyn Error + 'static), f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{error}")?;
        let mut source = error.source();
        let mut depth = 0;
        while let Some(cause) = source {
            depth += 1;
            if depth == 1 {
                write!(f, "\n\nCaused by:")?;
            }
            write!(f, "\n    {depth}: {cause}")?;
            source = cause.source();
        }
        if self.show_verbose()
            && let Some(location) = self.location
        {
            write!(f, "\n\nLocation:\n    {location}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
