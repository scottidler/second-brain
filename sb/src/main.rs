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
    logger::init_for(&cli)?;
    cli.cmd.run().await
}
