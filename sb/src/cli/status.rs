use eyre::Result;

#[derive(clap::Args)]
pub struct StatusArgs {}

pub fn run(_args: StatusArgs) -> Result<()> {
    eyre::bail!("sb status: not yet implemented (Phase 2)")
}
