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
    // A command that already printed its own failure returns `SilentFailure`;
    // map it to exit code 1 here (the one place exit codes are decided) instead
    // of `std::process::exit` deep in a print helper.
    match cli.cmd.run().await {
        Ok(()) => Ok(()),
        Err(e) if e.downcast_ref::<error::SilentFailure>().is_some() => {
            // `process::exit` runs no destructors, so flush the log writer by hand.
            drop(_log_guard);
            std::process::exit(1)
        }
        Err(e) => Err(e),
    }
}
