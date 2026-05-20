#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

use clap::Parser;
use eyre::Result;

use sb::cli::Cli;
use sb::logger;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    logger::init_for(&cli)?;
    cli.cmd.run().await
}
