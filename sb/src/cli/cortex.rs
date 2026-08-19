use clap::{Args, Subcommand};
use colored::Colorize;
use eyre::{Context, Result};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::LazyLock;

use cortex::opts;

static AFTER_HELP: LazyLock<String> = LazyLock::new(after_help_text);

fn after_help_text() -> String {
    let fabric_status = check_tool("fabric", &["--version"]);
    let log_path = crate::logger::log_path("cortex");

    format!(
        "REQUIRED TOOLS:\n{fabric_status}\n\nLogs are written to: {log_path}",
        log_path = log_path.display()
    )
}

fn check_tool(name: &str, version_args: &[&str]) -> String {
    match ProcessCommand::new(name).args(version_args).output() {
        Ok(output) if output.status.success() => {
            let ver = String::from_utf8_lossy(&output.stdout)
                .trim()
                .lines()
                .next()
                .unwrap_or("unknown")
                .to_string();
            format!("  \u{2705} {name:<10} {ver}")
        }
        _ => format!("  \u{274c} {name:<10} NOT FOUND"),
    }
}

#[derive(Args)]
#[command(after_help = AFTER_HELP.as_str())]
pub struct CortexCli {
    /// Path to config file
    #[arg(short = 'c', long)]
    pub config: Option<PathBuf>,

    /// Vault root directory (default: CWD)
    #[arg(short = 'r', long = "vault")]
    pub vault: Option<PathBuf>,

    /// Log level: trace, debug, info, warn, error
    #[arg(short, long)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Classify inbox notes by domain and promote to notes/
    Classify(ClassifyArgs),
    /// Validate vault against rules
    Lint(LintArgs),
    /// Scan for and create wikilinks
    Link(LinkArgs),
    /// Strip stoplisted wikilinks the auto-linker already landed
    Unlink(UnlinkArgs),
    /// Generate intelligence (daily/weekly notes)
    Intel(IntelArgs),
    /// Vault state fingerprinting
    State(StateArgs),
    /// Watch mode - run actions on change
    Daemon(DaemonArgs),
    /// Schema evolution and vault structure migration
    Migrate(MigrateArgs),
    /// Sweep tags: consolidate to canonical vocabulary
    Sweep(SweepArgs),
    /// Distill legacy notes into the structured L2 contract (backfill)
    Summarize(SummarizeArgs),
    /// Embed note summaries (and Phase B transcripts) into the search DB
    Embed(EmbedArgs),
    /// Build the deterministic edge graph oracle's graph retrieval reads
    Graph(GraphArgs),
    /// Merge or cross-link same-slug harvest session notes (dry-run unless --apply)
    Associate(AssociateArgs),
    /// Stub/refresh entity hub notes (concepts, creators, sources, dense tags)
    Hub(HubArgs),
    /// Discover candidate glossary entities from ingested notes (LLM)
    Entities(EntitiesArgs),
    /// Promote a proposed concept from entity-proposals.yml into glossary.yml
    /// (reviewable diff; dry-run unless --apply)
    ConceptPromote(ConceptPromoteArgs),
    /// One-time historical multi-repo backfill: LLM pass over pre-files-touched
    /// sessions proposing cross-repo bridges to bridge-proposals.yml (needs the
    /// live clyde catalog + fabric; the parent sequences the real env)
    BridgeBackfill(BridgeBackfillArgs),
    /// Apply a pending cross-repo bridge from bridge-proposals.yml as a hub-body
    /// wikilink diff (reviewable; dry-run unless --apply; never touches a note)
    BridgeApply(BridgeApplyArgs),
}

#[derive(Args)]
pub struct ConceptPromoteArgs {
    /// The proposal slug to promote (must be a pending entity-proposals.yml entry).
    pub slug: String,
    /// Write the change. Without it, prints the diff and writes nothing.
    #[arg(long)]
    pub apply: bool,
}

#[derive(Args)]
pub struct BridgeBackfillArgs {
    /// Cap the number of candidate notes processed this run (bounds LLM fan-out).
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Args)]
pub struct BridgeApplyArgs {
    /// The landed member note (vault-relative path) named by a pending proposal.
    #[arg(long)]
    pub member: String,
    /// The secondary repo (`<org>/<repo>`) whose hub gains the bridge.
    #[arg(long)]
    pub repo: String,
    /// Write the change. Without it, prints the diff and writes nothing.
    #[arg(long)]
    pub apply: bool,
}

#[derive(Args)]
pub struct ClassifyArgs {
    #[arg(long)]
    pub apply: bool,
    #[arg(long)]
    pub path: Option<String>,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub review_only: bool,
    #[arg(long)]
    pub reclassify_domain: Option<String>,
}
impl From<ClassifyArgs> for opts::ClassifyOpts {
    fn from(a: ClassifyArgs) -> Self {
        Self {
            apply: a.apply,
            path: a.path,
            force: a.force,
            review_only: a.review_only,
            reclassify_domain: a.reclassify_domain,
        }
    }
}

#[derive(Args)]
pub struct LintArgs {
    #[arg(long)]
    pub apply: bool,
    #[arg(long, value_enum, ignore_case = true, default_value_t = opts::LintFormat::Human)]
    pub format: opts::LintFormat,
    #[arg(long)]
    pub rule: Vec<String>,
    #[arg(long)]
    pub path: Option<String>,
}
impl From<LintArgs> for opts::LintOpts {
    fn from(a: LintArgs) -> Self {
        Self {
            apply: a.apply,
            format: a.format,
            rule: a.rule,
            path: a.path,
        }
    }
}

#[derive(Args)]
pub struct LinkArgs {
    #[arg(long)]
    pub apply: bool,
    #[arg(long, value_enum, ignore_case = true, default_value_t = opts::ScanScope::All)]
    pub scan: opts::ScanScope,
}
impl From<LinkArgs> for opts::LinkOpts {
    fn from(a: LinkArgs) -> Self {
        Self {
            apply: a.apply,
            scan: a.scan,
        }
    }
}

/// Retract wikilinks whose target is in `graph.wikilink-stopwords`.
///
/// The inverse of `link --apply`, for markup that landed before the writer
/// was gated. Reports by default; `--apply` edits note bodies.
#[derive(Args)]
pub struct UnlinkArgs {
    #[arg(long)]
    pub apply: bool,
    /// Also retract inside `origin: authored` notes. Off by default: the
    /// linker exempts authored notes, so a link there is normally the
    /// author's own. Use it to clean up links the linker wrote into authored
    /// notes before that exemption existed.
    #[arg(long)]
    pub include_authored: bool,
}
impl From<UnlinkArgs> for opts::UnlinkOpts {
    fn from(a: UnlinkArgs) -> Self {
        Self {
            apply: a.apply,
            include_authored: a.include_authored,
        }
    }
}

#[derive(Args)]
pub struct IntelArgs {
    #[arg(long)]
    pub daily: bool,
    #[arg(long)]
    pub weekly: bool,
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Treat this date (YYYY-MM-DD) as "today" instead of the system clock,
    /// to regenerate (backfill) a past day's digest/review.
    #[arg(long)]
    pub date: Option<chrono::NaiveDate>,
}
impl From<IntelArgs> for opts::IntelOpts {
    fn from(a: IntelArgs) -> Self {
        let mode = if a.weekly {
            cortex::intel::IntelMode::Weekly
        } else {
            cortex::intel::IntelMode::Daily
        };
        Self {
            mode,
            output: a.output,
            as_of: a.date,
        }
    }
}

#[derive(Args)]
pub struct StateArgs {
    #[arg(long)]
    pub refresh: bool,
    #[arg(long)]
    pub diff: bool,
}
impl From<StateArgs> for opts::StateOpts {
    fn from(a: StateArgs) -> Self {
        Self {
            refresh: a.refresh,
            diff: a.diff,
        }
    }
}

#[derive(Args)]
pub struct DaemonArgs {
    /// Write the systemd user unit for the cortex daemon
    #[arg(long)]
    pub install: bool,
    /// Remove the systemd user unit
    #[arg(long)]
    pub uninstall: bool,
    /// Run the daemon in the foreground (the no-flag default)
    #[arg(long)]
    pub start: bool,
    /// Print how to stop the running daemon
    #[arg(long)]
    pub stop: bool,
    /// Show the daemon's systemd status
    #[arg(long)]
    pub status: bool,
}
impl From<DaemonArgs> for opts::DaemonOpts {
    fn from(a: DaemonArgs) -> Self {
        Self {
            install: a.install,
            uninstall: a.uninstall,
            start: a.start,
            stop: a.stop,
            status: a.status,
        }
    }
}

#[derive(Args)]
pub struct MigrateArgs {
    #[arg(long)]
    pub apply: bool,
    #[arg(long)]
    pub plan: Option<PathBuf>,
}
impl From<MigrateArgs> for opts::MigrateOpts {
    fn from(a: MigrateArgs) -> Self {
        Self {
            apply: a.apply,
            plan: a.plan,
        }
    }
}

#[derive(Args)]
pub struct SweepArgs {
    #[arg(long)]
    pub migrate: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub proposals: bool,
    #[arg(long)]
    pub cold: bool,
}
impl From<SweepArgs> for opts::SweepOpts {
    fn from(a: SweepArgs) -> Self {
        Self {
            migrate: a.migrate,
            dry_run: a.dry_run,
            proposals: a.proposals,
            cold: a.cold,
        }
    }
}

#[derive(Args)]
pub struct EmbedArgs {
    #[arg(long)]
    pub backfill: bool,
    #[arg(long)]
    pub kind: Option<String>,
    /// Rollback verb: delete every embedding row of this kind
    /// (summary | transcript-chunk | claim) and exit.
    #[arg(long)]
    pub drop_kind: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, default_value_t = cortex::embed::DEFAULT_BATCH_SIZE)]
    pub batch_size: usize,
    #[arg(long)]
    pub prefetch_model: bool,
    #[arg(long, hide = true)]
    pub use_mock: bool,
}
impl From<EmbedArgs> for opts::EmbedOpts {
    fn from(a: EmbedArgs) -> Self {
        Self {
            backfill: a.backfill,
            kind: a.kind,
            drop_kind: a.drop_kind,
            model: a.model,
            batch_size: a.batch_size,
            prefetch_model: a.prefetch_model,
            use_mock: a.use_mock,
        }
    }
}

#[derive(Args)]
pub struct GraphArgs {
    /// Force a full rebuild of the edge graph (clear-then-rebuild every note),
    /// bypassing the incremental per-note watermarks.
    #[arg(long)]
    pub backfill: bool,
}
impl From<GraphArgs> for opts::GraphOpts {
    fn from(a: GraphArgs) -> Self {
        Self { backfill: a.backfill }
    }
}

#[derive(Args)]
pub struct AssociateArgs {
    /// Execute the plan: merge/cross-link same-slug session note groups
    /// (default: dry-run reporting what would happen, writes zero bytes).
    #[arg(long)]
    pub apply: bool,
}
impl From<AssociateArgs> for opts::AssociateOpts {
    fn from(a: AssociateArgs) -> Self {
        Self { apply: a.apply }
    }
}

#[derive(Args)]
pub struct HubArgs {
    /// Write hub notes to disk (default: report what would be stubbed).
    #[arg(long)]
    pub apply: bool,
    /// Re-synthesize each materialized hub's body from its membership
    /// (requires --apply; a failed pass preserves the prior body).
    #[arg(long)]
    pub synthesize: bool,
    /// Report each hub's source/session membership split
    /// (both/learned-not-applied/applied-not-read/unlinked). Read-only:
    /// writes nothing to the vault or the index, regardless of
    /// --apply/--synthesize.
    #[arg(long)]
    pub asymmetry: bool,
}
impl From<HubArgs> for opts::HubOpts {
    fn from(a: HubArgs) -> Self {
        Self {
            apply: a.apply,
            synthesize: a.synthesize,
            asymmetry: a.asymmetry,
        }
    }
}

#[derive(Args)]
pub struct EntitiesArgs {
    /// Run the LLM discovery pass, writing proposals to entity-proposals.yml.
    #[arg(long)]
    pub discover: bool,
    /// Override the max ingested notes processed this run.
    #[arg(long)]
    pub limit: Option<usize>,
}
impl From<EntitiesArgs> for opts::EntitiesOpts {
    fn from(a: EntitiesArgs) -> Self {
        Self {
            discover: a.discover,
            limit: a.limit,
        }
    }
}

#[derive(Args)]
pub struct SummarizeArgs {
    #[arg(long)]
    pub backfill: bool,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub domain: Option<String>,
    #[arg(long)]
    pub extractor: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub resume: bool,
}
impl From<SummarizeArgs> for opts::SummarizeOpts {
    fn from(a: SummarizeArgs) -> Self {
        Self {
            backfill: a.backfill,
            since: a.since,
            domain: a.domain,
            extractor: a.extractor,
            dry_run: a.dry_run,
            resume: a.resume,
        }
    }
}

impl CortexCli {
    pub async fn run(self) -> Result<()> {
        let config = cortex::config::Config::load(self.config.as_ref()).context("failed to load configuration")?;
        // Resolve the vault root lazily-ish: only the daemon verbs that DON'T
        // touch the vault (status/stop/uninstall) get the CWD fallback. `--start`
        // watches the vault and `--install` bakes the root into the systemd unit
        // - both must fail loudly on a missing root rather than silently using
        // `.` (which would watch / bake CWD). The no-flag default is `--start`,
        // so it propagates too.
        let vault_root = match &self.command {
            Command::Daemon(d) if d.status || d.stop || d.uninstall => config
                .vault_root(self.vault.as_ref())
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
            _ => config.vault_root(self.vault.as_ref())?,
        };
        log::debug!("cortex starting (version={})", env!("GIT_DESCRIBE"));
        log::debug!("resolved vault root: {}", vault_root.display());

        match self.command {
            Command::Classify(a) => {
                let apply = a.apply;
                let (report, _written) = cortex::classify::run(&vault_root, &config, &a.into())?;
                if apply {
                    println!("Classified {} note(s).", report.applied);
                    for line in report.format_human(true) {
                        println!("{line}");
                    }
                } else {
                    for line in report.format_human(false) {
                        println!("{line}");
                    }
                }
            }
            Command::Lint(a) => {
                let opts: cortex::opts::LintOpts = a.into();
                let (report, _lint_apply) = cortex::lint(&vault_root, &config, &opts)?;
                if opts.format == cortex::opts::LintFormat::Json {
                    println!("{}", report.format_json()?);
                } else {
                    for line in report.format_human(opts.apply) {
                        println!("{line}");
                    }
                }
            }
            Command::Link(a) => {
                let apply = a.apply;
                let report = cortex::link(&vault_root, &config, &a.into())?;
                if apply {
                    println!("Inserted wikilinks in {} file(s).", report.applied);
                } else {
                    for line in report.format_human(false) {
                        println!("{line}");
                    }
                }
            }
            Command::Unlink(a) => {
                let stats = cortex::unlink(&vault_root, &config, &a.into())?;
                print_unlink_stats(&stats);
            }
            Command::Intel(a) => {
                let report = cortex::intel::run(&vault_root, &config, &a.into())?;
                print_intel_report(&report);
            }
            Command::State(a) => {
                let report = cortex::state::run(&vault_root, &config, &a.into())?;
                print_state_report(&report);
            }
            Command::Daemon(a) => {
                let outcome = cortex::daemon::run(&vault_root, &config, &a.into()).await?;
                for line in &outcome.lines {
                    println!("{line}");
                }
            }
            Command::Migrate(a) => {
                let apply = a.apply;
                let report = cortex::migrate::run(&vault_root, &config, &a.into())?;
                if apply {
                    println!("Migrated {} file(s).", report.applied);
                } else {
                    for line in report.format_human(false) {
                        println!("{line}");
                    }
                }
            }
            Command::Sweep(a) => {
                let report = cortex::sweep::run(&vault_root, &config, &a.into())?;
                print_sweep_report(&report);
            }
            Command::Summarize(a) => {
                let summary = cortex::summarize::run(&vault_root, &config, &a.into()).await?;
                for line in &summary.would_distill {
                    println!("{line}");
                }
                log::info!(
                    "summarize complete: attempted={} distilled={} skipped={} failed={}",
                    summary.attempted,
                    summary.distilled,
                    summary.skipped,
                    summary.failed,
                );
            }
            Command::Embed(a) => {
                let opts_struct: cortex::opts::EmbedOpts = a.into();
                if opts_struct.prefetch_model {
                    let resolved = cortex::embed::prefetch(opts_struct.model.as_deref())?;
                    println!("Prefetched embedding model {resolved}.");
                } else if let Some(kind) = opts_struct.drop_kind.as_deref() {
                    let deleted = cortex::embed::drop_kind(&config, kind)?;
                    println!("dropped {deleted} embedding rows for kind={kind}");
                } else {
                    let stats = cortex::embed::run(&vault_root, &config, &opts_struct)?;
                    println!(
                        "embed complete: scanned={} embedded={} skipped_empty={} failed={}",
                        stats.scanned, stats.embedded, stats.skipped_empty, stats.failed,
                    );
                }
            }
            Command::Graph(a) => {
                let stats = cortex::graph::run(&vault_root, &config, &a.into())?;
                println!(
                    "graph complete: full_rebuild={} notes={} semantic={} wikilink={} shared_tag={} metadata={} repo_member={} creator_member={} source_member={} skipped={}",
                    stats.full_rebuild,
                    stats.notes_processed,
                    stats.semantic,
                    stats.wikilink,
                    stats.shared_tag,
                    stats.metadata,
                    stats.repo_member,
                    stats.creator_member,
                    stats.source_member,
                    stats.skipped,
                );
            }
            Command::Associate(a) => {
                let apply = a.apply;
                let report = cortex::association::run(&vault_root, &config, &a.into())?;
                print_association_report(apply, &report);
            }
            Command::Entities(a) => {
                if !a.discover {
                    eyre::bail!("nothing to do: pass --discover to run the entity discovery pass");
                }
                let report = cortex::entities::run(&vault_root, &config, &a.into())?;
                match &report.proposals_path {
                    Some(path) => println!(
                        "entities discover: scanned {} note(s), {} proposal(s) -> {path}",
                        report.notes_scanned, report.proposals,
                    ),
                    None => println!(
                        "entities discover: scanned {} note(s), no new proposals",
                        report.notes_scanned,
                    ),
                }
            }
            Command::ConceptPromote(a) => {
                let report = cortex::entities::promote_concept(
                    &vault::paths::entity_proposals(),
                    &vault::paths::glossary(),
                    &a.slug,
                    a.apply,
                )?;
                if report.already_present {
                    println!(
                        "concept-promote: `{}` is already a glossary concept; nothing to do",
                        report.slug
                    );
                } else if report.applied {
                    println!("concept-promote: promoted `{}`", report.slug);
                    println!("{}", report.diff);
                } else {
                    println!("concept-promote (dry-run - pass --apply to write):");
                    println!("{}", report.diff);
                }
            }
            Command::BridgeBackfill(a) => {
                run_bridge_backfill(&vault_root, &config, a.limit).await?;
            }
            Command::BridgeApply(a) => {
                let report = cortex::bridge::apply_bridge(
                    &vault::paths::bridge_proposals(),
                    &vault_root,
                    &a.member,
                    &a.repo,
                    a.apply,
                )?;
                if report.already_present {
                    println!(
                        "bridge-apply: {} already links `{}`; nothing to do",
                        report.hub_path, report.member
                    );
                } else if report.applied {
                    println!("bridge-apply: bridged `{}` into `{}`", report.member, report.repo);
                    println!("{}", report.diff);
                } else {
                    println!("bridge-apply (dry-run - pass --apply to write):");
                    println!("{}", report.diff);
                }
            }
            Command::Hub(a) => {
                let apply = a.apply;
                let synthesize = a.synthesize;
                let asymmetry = a.asymmetry;
                let report = cortex::hub::run(&vault_root, &config, &a.into())?;
                if asymmetry {
                    if let Some(asymmetry_report) = &report.asymmetry {
                        print!("{}", asymmetry_report.render());
                    }
                } else if apply {
                    println!(
                        "hub complete: created={} existing={} entities_recorded={}",
                        report.created, report.existing, report.entities_recorded,
                    );
                    if synthesize {
                        // Per-branch counts, so a systematic member-load
                        // breakage is visible instead of hiding in a total.
                        println!(
                            "hub bodies: written={} unchanged={} reset={} stubs_kept={} manual={} preserved={} members_skipped={}",
                            report.bodies_written,
                            report.bodies_unchanged,
                            report.bodies_reset,
                            report.stubs_kept,
                            report.bodies_manual,
                            report.bodies_preserved,
                            report.members_skipped,
                        );
                    }
                } else {
                    println!(
                        "hub dry-run: would create {} hub note(s) ({} already exist):",
                        report.created, report.existing,
                    );
                    for path in &report.stubs {
                        println!("  + {path}");
                    }
                }
            }
        }
        Ok(())
    }
}

/// Composition root for the one-time historical multi-repo backfill
/// (harvest-completion Phase 7). cortex owns the LLM detector + the pure
/// backfill/proposal logic; borg owns the clyde transcript reader; this glues
/// the two: scan the vault for pre-`files-touched` candidate notes, fetch each
/// survivor's transcript via borg's reader, and run the fail-closed backfill.
///
/// REAL RUN, DEFERRED: this needs the live clyde catalog (surviving transcripts),
/// a fabric-ready `ANTHROPIC_API_KEY`, and the `extract-repos-touched` pattern in
/// `~/.config/sb/patterns/`. The parent sequences that env; a transcript that has
/// been reaped surfaces as a reader error and is logged as unreachable (bounded
/// reach by design), never a hard abort.
async fn run_bridge_backfill(
    vault_root: &std::path::Path,
    config: &cortex::config::Config,
    limit: Option<usize>,
) -> Result<()> {
    log::debug!(
        "run_bridge_backfill: vault_root={} limit={:?}",
        vault_root.display(),
        limit
    );
    let notes = cortex::vault::scan_vault(vault_root, &config.vault)?;
    let mut candidates = cortex::bridge::candidate_members(&notes);
    if let Some(n) = limit {
        candidates.truncate(n);
    }
    if candidates.is_empty() {
        println!("bridge-backfill: no pre-files-touched candidate notes found; nothing to do");
        return Ok(());
    }

    // borg owns the clyde coupling: reuse its configured clyde binary + reader.
    use borg::harvest::reader::ExportReader;
    let borg_config: borg::config::Config = borg::config::load_config(None)?;
    let reader = borg::harvest::reader::ClydeExportReader::new(borg_config.harvest.clyde_binary.clone());

    let mut sessions: Vec<cortex::bridge::BackfillSession> = Vec::new();
    let mut unreachable = 0usize;
    for cand in &candidates {
        match reader.export_with_body(&cand.session_id).await {
            Ok(record) => match &record.body {
                Some(msgs) if !msgs.is_empty() => {
                    let transcript = borg::harvest::watermark::canonical_body_text(msgs);
                    sessions.push(cortex::bridge::BackfillSession {
                        session_id: cand.session_id.clone(),
                        note_path: cand.note_path.clone(),
                        primary_repo: cand.primary_repo.clone(),
                        transcript,
                    });
                }
                _ => {
                    unreachable += 1;
                    log::warn!(
                        "run_bridge_backfill: session {} has no surviving transcript body (reaped/empty); skipping",
                        cand.session_id
                    );
                }
            },
            Err(e) => {
                unreachable += 1;
                log::warn!(
                    "run_bridge_backfill: session {} transcript unreachable ({e:#}); skipping (bounded reach)",
                    cand.session_id
                );
            }
        }
    }

    let detector = cortex::bridge::FabricBridgeDetector {
        fabric: &config.fabric,
        pattern: cortex::bridge::BRIDGE_DETECT_PATTERN,
        max_input_tokens: config.entities.max_input_tokens,
        timeout_secs: config.entities.fabric_timeout_secs,
    };

    // Fail-closed: any detector failure aborts with ZERO proposals + a visible
    // error, never a silent partial write.
    let proposals = cortex::bridge::backfill(&sessions, &detector)?;
    let count = proposals.len();
    if count == 0 {
        println!(
            "bridge-backfill: scanned {} candidate(s), {} reachable, {} unreachable; no cross-repo bridges proposed",
            candidates.len(),
            sessions.len(),
            unreachable,
        );
        return Ok(());
    }
    let path = vault::paths::bridge_proposals();
    cortex::bridge::write_bridge_proposals(&path, proposals)?;
    println!(
        "bridge-backfill: scanned {} candidate(s), {} reachable, {} unreachable; proposed {} cross-repo bridge(s) -> {}",
        candidates.len(),
        sessions.len(),
        unreachable,
        count,
        path.display(),
    );
    println!("review with `sb cortex bridge-apply --member <note> --repo <org/repo>` (dry-run), then --apply");
    Ok(())
}

fn print_state_report(r: &cortex::state::StateReport) {
    if let Some(diff) = &r.diff {
        if diff.has_changes() {
            if !diff.added.is_empty() {
                println!("{}", "Added:".green().bold());
                for p in &diff.added {
                    println!("  + {}", p.display());
                }
            }
            if !diff.removed.is_empty() {
                println!("{}", "Removed:".red().bold());
                for p in &diff.removed {
                    println!("  - {}", p.display());
                }
            }
            if !diff.modified.is_empty() {
                println!("{}", "Modified:".yellow().bold());
                for p in &diff.modified {
                    println!("  ~ {}", p.display());
                }
            }
            println!(
                "\n{}: {} added, {} removed, {} modified",
                "Summary".bold(),
                diff.added.len(),
                diff.removed.len(),
                diff.modified.len()
            );
        } else {
            println!("{}", "No changes since last scan.".green());
        }
    } else if r.diff_requested {
        println!("{}", "No previous manifest found. Run with --refresh first.".yellow());
    }

    if let Some(count) = r.refreshed_count {
        println!("{} manifest saved ({} files)", "Refreshed:".green().bold(), count);
    }

    if r.diff.is_none() && r.refreshed_count.is_none() && !r.diff_requested {
        match &r.current {
            Some(snap) => {
                println!(
                    "Last scan: {} ({} files)",
                    snap.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
                    snap.file_count
                );
            }
            None => {
                println!(
                    "{}",
                    "No manifest found. Run `sb cortex state --refresh` to create one.".yellow()
                );
            }
        }
    }
}

fn print_unlink_stats(s: &cortex::unlink::UnlinkStats) {
    if s.changes.is_empty() {
        println!("No stoplisted wikilinks found in {} note(s).", s.scanned);
        return;
    }
    for change in &s.changes {
        println!(
            "  {} [[{}]] x{}",
            change.path.display(),
            change.target,
            change.occurrences
        );
    }
    if s.applied {
        println!(
            "Retracted {} wikilink(s) across {} file(s).",
            s.occurrences, s.files_changed
        );
    } else {
        println!(
            "Dry run: would retract {} wikilink(s) across {} file(s). Re-run with --apply.",
            s.occurrences, s.files_changed
        );
    }
}

fn print_sweep_report(r: &cortex::sweep::SweepReport) {
    use cortex::sweep::SweepMode;
    match &r.mode {
        SweepMode::Cold {
            scanned,
            surfaced,
            pinned_excluded,
        } => {
            println!("Cold sweep: scanned={scanned} surfaced={surfaced} pinned_excluded={pinned_excluded}");
            return;
        }
        SweepMode::WouldMigrate { count } => {
            println!("Dry run: would modify {count} note(s).");
        }
        SweepMode::Migrated { count } => {
            println!("Migrated tags in {count} note(s).");
        }
        SweepMode::Proposals => {}
    }
    if let Some(proposals) = &r.proposals {
        if proposals.is_empty() {
            println!("No new tag proposals.");
        } else {
            println!("Found {} tag(s) needing review:", proposals.len());
            for proposal in proposals {
                println!("  {} (on {} notes)", proposal.tag, proposal.frequency);
            }
            if let Some(path) = &r.proposals_path {
                println!("Proposals written to {path}");
            }
        }
    }
}

/// Format a `cortex associate` report. Mirrors the `SweepMode` precedent:
/// `AssociationReport`'s two variants already say whether anything was
/// written, so this formats purely off the report - `apply` is only used to
/// pick the dry-run-vs-applied HEADER wording, never to re-decide behavior.
fn print_association_report(apply: bool, report: &cortex::association::AssociationReport) {
    use cortex::association::AssociationOutcome;

    let outcomes = report.outcomes();
    if apply {
        let merges = outcomes
            .iter()
            .filter(|o| matches!(o, AssociationOutcome::Merge { .. }))
            .count();
        let cross_links = outcomes.len() - merges;
        println!("associate complete: {merges} merge(s), {cross_links} cross-link(s)");
    } else if outcomes.is_empty() {
        println!("associate dry-run: no same-slug groups to associate");
    } else {
        println!("associate dry-run (pass --apply to write):");
    }

    for outcome in outcomes {
        match outcome {
            AssociationOutcome::Merge {
                survivor,
                absorbed,
                session_ids,
            } => {
                println!(
                    "  merge: {} absorbs {} ({} session id(s))",
                    survivor.display(),
                    absorbed
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    session_ids.len(),
                );
            }
            AssociationOutcome::CrossLink { notes } => {
                println!(
                    "  cross-link: {}",
                    notes
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" <-> "),
                );
            }
        }
    }
}

fn print_intel_report(r: &cortex::intel::IntelReport) {
    let label = match r.mode {
        cortex::intel::IntelMode::Daily => "daily digest",
        cortex::intel::IntelMode::Weekly => "weekly review",
    };
    println!("Generated {label}: {}", r.output_path.display());
}

#[cfg(test)]
mod tests;
