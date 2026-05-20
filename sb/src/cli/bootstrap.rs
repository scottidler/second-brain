use eyre::Result;

#[derive(clap::Args)]
pub struct BootstrapArgs {}

pub fn run(_args: BootstrapArgs) -> Result<()> {
    eyre::bail!("sb bootstrap: not yet implemented (Phase 2)")
}
