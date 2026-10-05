use clap::Parser;
use eyre::Result;

pub mod bootstrap;
pub mod borg;
pub mod checks;
pub mod cortex;
pub mod doctor;
pub mod oracle;
pub mod output;
pub mod status;

#[derive(Parser)]
#[command(
    name = "sb",
    about = "second-brain unified CLI: borg + cortex + oracle",
    version = env!("GIT_DESCRIBE"),
)]
pub struct Cli {
    /// Log level: trace, debug, info, warn, error.
    /// Applies globally; per-subsystem `--log-level` overrides this.
    #[arg(short = 'l', long, global = true)]
    pub log_level: Option<String>,

    /// Enable verbose output (sets log level to debug if --log-level not given)
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Ingestion daemon (URLs, notes, hotkey, log, audit, replay, daemon, ...)
    Borg(borg::BorgCli),
    /// Vault governance (lint, link, classify, sweep, embed, summarize, daemon, ...)
    Cortex(cortex::CortexCli),
    /// Knowledge retrieval MCP server (serve, index, stats, call)
    Oracle(oracle::OracleCli),
    /// Aggregated health across all subsystems
    Status(status::StatusArgs),
    /// Severity-tagged health check
    Doctor(doctor::DoctorArgs),
    /// First-time setup: config templates, systemd units, prefetch model
    Bootstrap(bootstrap::BootstrapArgs),
}

impl Cmd {
    /// True for the two long-running daemons: `borg daemon --start`, and
    /// `cortex daemon` in its foreground mode (`--start`, or no mode flag,
    /// matching `cortex::daemon::run`'s dispatch). They keep their own
    /// graceful-shutdown signal handling; systemd's control-group kill covers
    /// their children.
    pub fn is_long_running_daemon(&self) -> bool {
        match self {
            Cmd::Borg(c) => matches!(c.command.as_ref(), Some(borg::Command::Daemon(d)) if d.start),
            Cmd::Cortex(c) => matches!(
                &c.command,
                cortex::Command::Daemon(d) if !(d.install || d.uninstall || d.status || d.stop)
            ),
            Cmd::Oracle(_) | Cmd::Status(_) | Cmd::Doctor(_) | Cmd::Bootstrap(_) => false,
        }
    }

    pub async fn run(self) -> Result<()> {
        match self {
            Cmd::Borg(c) => c.run().await,
            Cmd::Cortex(c) => c.run().await,
            Cmd::Oracle(c) => c.run().await,
            Cmd::Status(a) => status::run(a),
            Cmd::Doctor(a) => doctor::run(a),
            Cmd::Bootstrap(a) => bootstrap::run(a).await,
        }
    }
}

#[cfg(test)]
mod tests;
