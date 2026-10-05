#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

use clap::Parser;
use eyre::Result;

use sb::cli::Cli;
use sb::{error, logger};

#[tokio::main]
async fn main() -> Result<()> {
    // Pre-parse the verbose flag so the eyre hook can be installed before
    // anything else has a chance to construct an eyre::Report. Clap errors are
    // separate (they don't flow through eyre) so the hook only affects our
    // own errors.
    let verbose = std::env::args().any(|a| a == "-v" || a == "--verbose");
    error::install(verbose);

    let cli = Cli::parse();
    // `oracle serve` logs through a background writer thread; this guard is what
    // flushes it. Bound here, in the only frame that outlives both the command
    // and its shutdown log line. `None` on every other path.
    let _log_guard = logger::init_for(&cli)?;
    // A command that already printed its own output returns `SilentFailure`
    // (exit 1) or `ExitWith(n)` (exit n, e.g. `sb borg wait`'s 3/4/5); map it
    // here (the one place exit codes are decided) instead of
    // `std::process::exit` deep in a print helper. Clap's own exit 2 for a
    // usage error happens in `Cli::parse()` above and never reaches this.
    match cli.cmd.run().await {
        Ok(()) => Ok(()),
        Err(e) => match error::exit_code(&e) {
            Some(code) => {
                // `process::exit` runs no destructors, so flush the log writer by hand.
                drop(_log_guard);
                std::process::exit(i32::from(code))
            }
            None => Err(e),
        },
    }
}
