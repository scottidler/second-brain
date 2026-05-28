//! `sb glean` subcommand surface.

use clap::{Args, Subcommand};
use eyre::{Context, Result};

use glean::Ledger;
use glean::opts;

#[derive(Args)]
pub struct GleanCli {
    /// Override the glean config path (default ~/.config/sb/glean.yml).
    #[arg(short = 'c', long)]
    pub config: Option<std::path::PathBuf>,

    /// Vault root override.
    #[arg(short = 'r', long = "vault")]
    pub vault: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Tier-1: classify all unprocessed JSONL sessions.
    Harvest(HarvestArgs),
    /// Rematerialize the work_items table from sessions.
    Cluster,
    /// Tier-2: distill one or all work-items.
    Distill(DistillArgs),
    /// Run the dreaming consolidation pass.
    Dream,
    /// Inspect or drop quarantine entries.
    Quarantine(QuarantineArgs),
    /// Print one work-item's source sessions.
    Show(ShowArgs),
    /// Counts + cadence.
    Status,
    /// Daemon lifecycle.
    Daemon(DaemonArgs),
}

#[derive(Args)]
pub struct HarvestArgs {
    /// Reclassify every session even if jsonl_sha256 is unchanged.
    #[arg(long)]
    pub force: bool,
    /// Restrict harvest to one JSONL file.
    #[arg(long)]
    pub only: Option<std::path::PathBuf>,
}

#[derive(Args)]
pub struct DistillArgs {
    /// Distill only one work-item (content_hash prefix or slug).
    #[arg(long = "work-item")]
    pub work_item: Option<String>,
    /// Force re-distill even if the chunk already exists.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args)]
pub struct QuarantineArgs {
    #[command(subcommand)]
    pub action: QuarantineAction,
}

#[derive(Subcommand)]
pub enum QuarantineAction {
    /// List quarantine rows.
    List,
    /// Print one quarantined session's last reason and JSONL path.
    Inspect { session_uuid: String },
    /// Drop quarantine rows for one session.
    Drop { session_uuid: String },
}

#[derive(Args)]
pub struct ShowArgs {
    /// Work-item content_hash (prefix accepted) or slug.
    pub work_item: String,
}

#[derive(Args)]
pub struct DaemonArgs {
    #[arg(long)]
    pub install: bool,
    #[arg(long)]
    pub uninstall: bool,
    #[arg(long)]
    pub status: bool,
}

impl GleanCli {
    pub async fn run(self) -> Result<()> {
        let mut config = glean::Config::load().context("load glean config")?;
        if let Some(v) = self.vault {
            config.vault.root_path = v;
        }
        match self.command {
            Command::Harvest(a) => {
                let ledger = open_ledger()?;
                let report = glean::harvest(
                    &ledger,
                    &config,
                    &opts::HarvestOpts {
                        force: a.force,
                        only_jsonl: a.only,
                    },
                )
                .context("run harvest")?;
                println!(
                    "discovered={} classified={} skipped={} quarantined={}",
                    report.n_discovered, report.n_classified, report.n_skipped_unchanged, report.n_quarantined
                );
                Ok(())
            }
            Command::Cluster => {
                let ledger = open_ledger()?;
                let r = glean::cluster::run(&ledger, &config.cluster).context("cluster")?;
                println!(
                    "work_items: total={} design-doc={} theme={} singleton={}",
                    r.n_total, r.n_design_doc, r.n_theme, r.n_singletons
                );
                Ok(())
            }
            Command::Distill(a) => {
                let ledger = open_ledger()?;
                if let Some(wi) = a.work_item {
                    let item = resolve_work_item(&ledger, &wi)?;
                    let _ = a.force;
                    let r = glean::distill::distill_one(&ledger, &config, &item).context("distill_one")?;
                    println!("wrote {}", r.chunk_path.display());
                } else {
                    let reports = glean::distill::distill_all(&ledger, &config).context("distill_all")?;
                    for r in reports {
                        println!("{}", r.chunk_path.display());
                    }
                }
                Ok(())
            }
            Command::Dream => {
                let ledger = open_ledger()?;
                let r = glean::dream::run_all(&ledger, &config).context("dream")?;
                println!("dreams: dedup={} xref={} stale={}", r.n_dedup, r.n_xref, r.n_stale);
                Ok(())
            }
            Command::Quarantine(a) => {
                let ledger = open_ledger()?;
                match a.action {
                    QuarantineAction::List => {
                        for row in ledger.list_quarantine().context("list quarantine")? {
                            println!(
                                "{}\t{}\t{}\t{}",
                                row.id,
                                row.session_uuid,
                                row.reason,
                                row.jsonl_path.display()
                            );
                        }
                    }
                    QuarantineAction::Inspect { session_uuid } => {
                        if let Some(row) = ledger.get_quarantine_for(&session_uuid).context("inspect quarantine")? {
                            println!(
                                "session={} reason={} jsonl={} at={}",
                                row.session_uuid,
                                row.reason,
                                row.jsonl_path.display(),
                                row.quarantined_at
                            );
                        } else {
                            println!("no quarantine entry for {session_uuid}");
                        }
                    }
                    QuarantineAction::Drop { session_uuid } => {
                        let n = ledger.drop_quarantine(&session_uuid).context("drop quarantine")?;
                        println!("dropped {n} row(s)");
                    }
                }
                Ok(())
            }
            Command::Show(a) => {
                let ledger = open_ledger()?;
                let item = resolve_work_item(&ledger, &a.work_item)?;
                println!(
                    "work-item content_hash={} key_type={} key_value={}",
                    item.content_hash,
                    item.key_type.as_str(),
                    item.key_value
                );
                println!("sessions:");
                for u in &item.session_uuids {
                    if let Some(s) = ledger.get_session(u).context("get_session")? {
                        println!("  - {}  ({})  {}", u, s.summary_one_line, s.jsonl_path.display());
                    } else {
                        println!("  - {u}  (missing from sessions table)");
                    }
                }
                Ok(())
            }
            Command::Status => {
                let ledger = open_ledger()?;
                let sessions = ledger.all_sessions().context("all_sessions")?;
                let work_items = ledger.all_work_items().context("all_work_items")?;
                let quarantine = ledger.list_quarantine().context("list_quarantine")?;
                println!(
                    "sessions={} work_items={} quarantine={}",
                    sessions.len(),
                    work_items.len(),
                    quarantine.len()
                );
                Ok(())
            }
            Command::Daemon(a) => {
                let outcome = glean::daemon::run(
                    &config,
                    &opts::DaemonOpts {
                        install: a.install,
                        uninstall: a.uninstall,
                        status: a.status,
                    },
                )
                .await
                .context("daemon")?;
                for line in outcome.lines {
                    println!("{line}");
                }
                Ok(())
            }
        }
    }
}

fn open_ledger() -> Result<Ledger> {
    Ledger::open(vault::paths::glean_db_path()).context("open glean ledger")
}

fn resolve_work_item(ledger: &Ledger, key: &str) -> Result<glean::WorkItem> {
    let items = ledger.all_work_items().context("all_work_items")?;
    if let Some(item) = items
        .iter()
        .find(|w| w.content_hash == key || w.content_hash.starts_with(key))
    {
        return Ok(item.clone());
    }
    let needle = key.trim_end_matches(".md");
    if let Some(item) = items.iter().find(|w| glean::render::slug_for_work_item(w) == needle) {
        return Ok(item.clone());
    }
    eyre::bail!("no work-item matched {key:?}")
}
