#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

use clap::Parser;
use eyre::{Context, Result};

use strip_transcripts::{Disposition, ensure_clean_worktree, run};

/// One-shot backfill: strip `## Transcript`-to-EOF from Video/Article notes
/// ingested on or after 2026-06-28. Phase 6 of
/// docs/design/2026-07-07-distillation-output-restore.md. NOT a permanent
/// `sb` subcommand -- one-shot surgery does not earn a forever spot on the
/// CLI surface. Run this ONCE, on the daemon host, with the borg daemon
/// stopped (so no new notes are minted mid-sweep).
#[derive(Parser, Debug)]
#[command(about, long_about = None)]
struct Cli {
    /// Vault root. Same precedence as every other second-brain command:
    /// this flag, then a marker-gated CWD (a `.obsidian/` directory).
    #[arg(long)]
    vault: Option<String>,

    /// Log level: trace, debug, info, warn, error.
    #[arg(short = 'l', long, default_value = "info")]
    log_level: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    env_logger::Builder::new()
        .filter_level(cli.log_level.parse().unwrap_or(log::LevelFilter::Info))
        .init();

    let vault_override = cli.vault.as_deref().map(vault::paths::expand_tilde);
    let vault_root = vault::paths::resolve_vault_root(vault_override.as_deref(), None).context("resolve vault root")?;

    ensure_clean_worktree(&vault_root)?;

    let report = run(&vault_root)?;

    for outcome in &report.outcomes {
        match &outcome.disposition {
            Disposition::Stripped => println!("stripped  {}", outcome.path.display()),
            Disposition::Refused(reason) => println!("refused   {}  ({reason})", outcome.path.display()),
        }
    }
    println!(
        "\n{} candidates / {} stripped / {} refused",
        report.outcomes.len(),
        report.stripped(),
        report.refused()
    );

    Ok(())
}
