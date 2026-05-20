use eyre::Result;

#[derive(clap::Args)]
pub struct DoctorArgs {}

pub fn run(_args: DoctorArgs) -> Result<()> {
    eyre::bail!("sb doctor: not yet implemented (Phase 2)")
}
