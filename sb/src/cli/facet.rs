use clap::{Args, Subcommand};
use eyre::{Context, Result};
use std::path::PathBuf;

#[derive(Args)]
pub struct FacetCli {
    /// Path to config file. Defaults to `~/.config/sb/facet.yml`.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Vault root override (precedence: CLI > config > marker-gated CWD).
    #[arg(long)]
    pub vault: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// One-shot harvest tick.
    Harvest,
    /// One-shot spectra rollup. Synthesises one
    /// `notes/facet/spectra/<mode>.md` per scaffolding mode that has at
    /// least two moments in the configured window. Idempotent and
    /// merge-safe (operator content outside fenceposts is preserved).
    Spectra,
    /// Long-running daemon. `--install` writes the systemd unit;
    /// `--uninstall` removes it; no flag runs the cadence loop.
    Daemon {
        #[arg(long)]
        install: bool,
        #[arg(long)]
        uninstall: bool,
    },
    /// List work-items in the ledger.
    List {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long, default_value = "active")]
        status: String,
    },
    /// Show one work-item's ledger summary.
    Show { slug: String },
    /// Force a fresh render of one work-item from current ledger state.
    Render { slug: String },
    /// Re-process a session or work-item. For a session UUID: rewinds the
    /// cluster offset so the next tick re-clusters from there. For a
    /// work-item slug: flips its `cluster_assignments.extracted` rows
    /// back to 0 so the next tick re-extracts. Useful after fixing a
    /// transient LLM error or rolling a pattern file.
    Retry { target: String },
    /// Archive a work-item: marks status='archived' and moves the
    /// note via `rkvr rmrf` semantics (recoverable). The slug stays in
    /// the ledger so it does not collide with future work.
    Archive { slug: String },
    /// Last-tick status: counts, budget, last harvest.
    Status,
    /// Config + filesystem + LLM sanity check.
    Doctor,
    /// Merge mechanically-suffixed duplicate work-items into their
    /// base concept. Detects slugs like `<base>-2` when `<base>` also
    /// exists, re-points judgment moments / cluster assignments /
    /// session links / repo links at the base, deletes the duplicate
    /// row, and (unless --dry-run) archives the duplicate's prism
    /// note via `rkvr rmrf`.
    Dedupe {
        /// Print the merge plan; touch nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

impl FacetCli {
    pub async fn run(self) -> Result<()> {
        let config = facet::Config::load(self.config.as_deref()).context("load facet config")?;
        match self.command {
            Commands::Harvest => harvest(&config, self.vault.as_deref()).await,
            Commands::Spectra => spectra(&config, self.vault.as_deref()).await,
            Commands::Daemon { install, uninstall } => daemon(config, install, uninstall, self.vault.as_deref()).await,
            Commands::List { repo, mode, status } => list(&config, repo, mode, status),
            Commands::Show { slug } => show(&config, &slug),
            Commands::Render { slug } => render(&config, self.vault.as_deref(), &slug),
            Commands::Retry { target } => retry(&config, &target),
            Commands::Archive { slug } => archive(&config, self.vault.as_deref(), &slug),
            Commands::Status => status(&config),
            Commands::Doctor => doctor(&config, self.vault.as_deref()),
            Commands::Dedupe { dry_run } => dedupe(&config, self.vault.as_deref(), dry_run),
        }
    }
}

fn ledger_open(_config: &facet::Config) -> Result<facet::Ledger> {
    let path = vault::paths::facet_state_db();
    facet::Ledger::open(&path).with_context(|| format!("open facet ledger at {}", path.display()))
}

fn vault_root(cli_override: Option<&std::path::Path>) -> Result<PathBuf> {
    vault::paths::resolve_vault_root(cli_override, None).context("resolve vault root")
}

async fn harvest(config: &facet::Config, vault_override: Option<&std::path::Path>) -> Result<()> {
    let ledger = ledger_open(config)?;
    let vault = vault_root(vault_override)?;
    let report = facet::daemon::harvest_once(config, &ledger, &vault).await?;
    println!(
        "facet harvest complete:\n  sessions_seen: {}\n  cluster_assignments_created: {}\n  moments_extracted: {}\n  workitems_rendered: {}\n  failures: {}",
        report.sessions_seen,
        report.cluster_assignments_created,
        report.moments_extracted,
        report.workitems_rendered,
        report.failures
    );
    Ok(())
}

async fn spectra(config: &facet::Config, vault_override: Option<&std::path::Path>) -> Result<()> {
    let ledger = ledger_open(config)?;
    let vault = vault_root(vault_override)?;
    let written = facet::daemon::harvest::run_spectra_rollup(config, &ledger, &vault).await?;
    println!("facet spectra rollup complete: {written} spectra written");
    Ok(())
}

async fn daemon(
    config: facet::Config,
    install: bool,
    uninstall: bool,
    vault_override: Option<&std::path::Path>,
) -> Result<()> {
    if install && uninstall {
        eyre::bail!("--install and --uninstall are mutually exclusive");
    }
    if install {
        let outcome = facet::daemon::install_systemd_service()?;
        println!("Wrote {}", outcome.unit_path.display());
        println!("Run: systemctl --user daemon-reload && systemctl --user enable --now sb-facet.service");
        return Ok(());
    }
    if uninstall {
        match facet::daemon::uninstall_systemd_service()? {
            Some(p) => println!("Removed {}", p.display()),
            None => println!("No unit file present."),
        }
        return Ok(());
    }
    let ledger = ledger_open(&config)?;
    let vault = vault_root(vault_override)?;
    facet::daemon::run_loop(config, ledger, vault).await
}

fn list(config: &facet::Config, repo: Option<String>, mode: Option<String>, status: String) -> Result<()> {
    let ledger = ledger_open(config)?;
    ledger.with_conn(|c| {
        let mut sql = String::from(
            "SELECT DISTINCT w.id, w.slug, w.title, w.status, w.updated_at \
             FROM work_items w",
        );
        if repo.is_some() {
            sql.push_str(" JOIN work_item_repos r ON r.workitem_id = w.id");
        }
        if mode.is_some() {
            sql.push_str(" JOIN judgment_moments m ON m.workitem_id = w.id");
        }
        sql.push_str(" WHERE w.status = ?1");
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(status.clone())];
        if let Some(r) = &repo {
            sql.push_str(" AND r.repo_slug = ?2");
            params.push(Box::new(r.clone()));
        }
        if let Some(m) = &mode {
            let idx = params.len() + 1;
            sql.push_str(&format!(" AND m.mode = ?{idx}"));
            params.push(Box::new(m.clone()));
        }
        sql.push_str(" ORDER BY w.updated_at DESC LIMIT 100");
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut count = 0;
        for row in rows {
            let (_id, slug, title, st, updated) = row?;
            println!("{slug:<48}  {st:<10}  {updated}  {title}");
            count += 1;
        }
        if count == 0 {
            println!("(no work-items matching filter)");
        }
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}

fn show(config: &facet::Config, slug: &str) -> Result<()> {
    let ledger = ledger_open(config)?;
    let w = ledger
        .workitem_by_slug(slug)?
        .ok_or_else(|| eyre::eyre!("no work-item with slug {slug}"))?;
    let moments = ledger.moments_for_workitem(w.id)?;
    println!("slug: {}", w.slug);
    println!("title: {}", w.title);
    println!("status: {}", w.status.as_str());
    println!("repos: {}", w.repos.join(", "));
    println!("sessions: {}", w.sessions_count);
    println!("modes: {}", w.modes_present.join(", "));
    println!("moments: {}", moments.len());
    println!("vault path: {}/{}.md", config.vault.prisms_dir, w.slug);
    Ok(())
}

fn render(config: &facet::Config, vault_override: Option<&std::path::Path>, slug: &str) -> Result<()> {
    let ledger = ledger_open(config)?;
    let w = ledger
        .workitem_by_slug(slug)?
        .ok_or_else(|| eyre::eyre!("no work-item with slug {slug}"))?;
    let moments = ledger.moments_for_workitem(w.id)?;
    let vault = vault_root(vault_override)?;
    let path = vault.join(&config.vault.prisms_dir).join(format!("{}.md", w.slug));
    facet::render::render_work_item_note(&path, &w, &moments)?;
    println!("Re-rendered: {}", path.display());
    Ok(())
}

/// `sb facet retry <target>` — `target` is either a session UUID or a
/// work-item slug. UUIDs match `\^[0-9a-f-]+$\` and are 36 chars; anything
/// else is treated as a slug.
fn retry(config: &facet::Config, target: &str) -> Result<()> {
    let ledger = ledger_open(config)?;
    let is_uuid = target.len() == 36 && target.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if is_uuid {
        let n = ledger.with_conn(|c| {
            let n = c
                .execute(
                    "UPDATE sessions SET last_cluster_offset = 0, last_cluster_turn_uuid = NULL, \
                            failure_count = 0, last_failure_reason = NULL, last_failure_stage = NULL \
                     WHERE session_uuid = ?1",
                    rusqlite::params![target],
                )
                .context("rewind session cluster offset")?;
            Ok(n)
        })?;
        if n == 0 {
            eyre::bail!("no session with uuid {target}");
        }
        println!("Rewound cluster offset for session {target}; next tick will re-cluster from byte 0.");
        return Ok(());
    }
    let workitem_id = ledger
        .workitem_by_slug(target)?
        .ok_or_else(|| eyre::eyre!("no work-item with slug {target}"))?
        .id;
    let n = ledger.with_conn(|c| {
        let n = c
            .execute(
                "UPDATE cluster_assignments SET extracted = 0 WHERE workitem_id = ?1",
                rusqlite::params![workitem_id],
            )
            .context("reset cluster_assignments.extracted")?;
        Ok(n)
    })?;
    println!("Flipped {n} cluster_assignments row(s) for workitem {target} to extracted=0; next tick will re-extract.");
    Ok(())
}

/// `sb facet archive <slug>` — mark a work-item archived in the ledger
/// and move its vault note via `rkvr rmrf` (recoverable archive).
fn archive(config: &facet::Config, vault_override: Option<&std::path::Path>, slug: &str) -> Result<()> {
    let ledger = ledger_open(config)?;
    let workitem = ledger
        .workitem_by_slug(slug)?
        .ok_or_else(|| eyre::eyre!("no work-item with slug {slug}"))?;
    ledger.with_conn(|c| {
        c.execute(
            "UPDATE work_items SET status = 'archived', updated_at = ?2 WHERE id = ?1",
            rusqlite::params![workitem.id, chrono::Utc::now().to_rfc3339()],
        )
        .context("update work_items status")?;
        Ok(())
    })?;
    let vault = vault_root(vault_override)?;
    let note_path = vault
        .join(&config.vault.prisms_dir)
        .join(format!("{}.md", workitem.slug));
    if note_path.exists() {
        // Per ~/.claude/refs/safety.md + memory `feedback-rust-deletes-via-rkvr`:
        // Rust code that deletes user-meaningful files (vault notes,
        // artifacts) must shell out to `rkvr rmrf`, not `std::fs::remove_*`.
        let out = std::process::Command::new("rkvr")
            .arg("rmrf")
            .arg(&note_path)
            .output()
            .context("invoke rkvr rmrf")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eyre::bail!("rkvr rmrf failed: {stderr}");
        }
        println!("Archived: ledger status='archived', note moved via rkvr (recoverable via `rkvr rcvr`).");
    } else {
        println!(
            "Archived: ledger status='archived'. No vault note at {} to move.",
            note_path.display()
        );
    }
    Ok(())
}

fn status(config: &facet::Config) -> Result<()> {
    let ledger = ledger_open(config)?;
    let last_tick = ledger
        .meta_get("last-harvest-tick")?
        .unwrap_or_else(|| "(never)".to_string());
    let counts = ledger.with_conn(|c| {
        let active: i64 = c.query_row("SELECT COUNT(*) FROM work_items WHERE status = 'active'", [], |r| {
            r.get(0)
        })?;
        let dormant: i64 = c.query_row("SELECT COUNT(*) FROM work_items WHERE status = 'dormant'", [], |r| {
            r.get(0)
        })?;
        let archived: i64 = c.query_row("SELECT COUNT(*) FROM work_items WHERE status = 'archived'", [], |r| {
            r.get(0)
        })?;
        let pending_extract: i64 = c.query_row(
            "SELECT COUNT(*) FROM cluster_assignments WHERE extracted = 0",
            [],
            |r| r.get(0),
        )?;
        let moments_total: i64 = c.query_row("SELECT COUNT(*) FROM judgment_moments", [], |r| r.get(0))?;
        Ok((active, dormant, archived, pending_extract, moments_total))
    })?;
    println!("facet status:");
    println!("  last-harvest-tick:    {last_tick}");
    println!("  active workitems:     {}", counts.0);
    println!("  dormant workitems:    {}", counts.1);
    println!("  archived workitems:   {}", counts.2);
    println!("  pending extract rows: {}", counts.3);
    println!("  judgment moments:     {}", counts.4);
    Ok(())
}

fn doctor(config: &facet::Config, vault_override: Option<&std::path::Path>) -> Result<()> {
    use colored::Colorize;
    let projects = &config.claude_projects_root;
    if projects.is_dir() {
        println!("{} claude_projects_root present: {}", "OK".green(), projects.display());
    } else {
        println!(
            "{} claude_projects_root missing: {}",
            "WARN".yellow(),
            projects.display()
        );
    }
    let state = vault::paths::facet_state_db();
    println!("ledger db: {}", state.display());
    match vault_root(vault_override) {
        Ok(v) => println!("{} vault: {}", "OK".green(), v.display()),
        Err(e) => println!("{} vault unresolved: {e}", "ERR".red()),
    }
    if config.llm.per_day_budget_usd < config.llm.per_tick_budget_usd {
        println!(
            "{} llm.per-day-budget-usd ({}) < per-tick-budget-usd ({}); per-tick budget will never apply in full",
            "WARN".yellow(),
            config.llm.per_day_budget_usd,
            config.llm.per_tick_budget_usd
        );
    }
    Ok(())
}

fn dedupe(config: &facet::Config, vault_override: Option<&std::path::Path>, dry_run: bool) -> Result<()> {
    let ledger = ledger_open(config)?;
    let plans = facet::dedupe::plan_merges(&ledger)?;
    if plans.is_empty() {
        println!("facet dedupe: no slug-suffix duplicates found.");
        return Ok(());
    }
    println!(
        "facet dedupe: {} merge(s) {}:",
        plans.len(),
        if dry_run { "planned (dry-run)" } else { "executing" }
    );
    for p in &plans {
        println!("  {} -> {}", p.duplicate_slug, p.base_slug);
    }
    if dry_run {
        return Ok(());
    }
    let vault = vault_root(vault_override)?;
    let mut totals = facet::dedupe::MergeReport::default();
    let mut rkvr_failures = 0usize;
    let mut rerender_failures = 0usize;
    for p in &plans {
        let r = facet::dedupe::execute(&ledger, p)?;
        totals.moments_moved += r.moments_moved;
        totals.moments_collided += r.moments_collided;
        totals.cluster_rows_moved += r.cluster_rows_moved;
        totals.cluster_rows_collided += r.cluster_rows_collided;
        totals.session_links_moved += r.session_links_moved;
        totals.session_links_collided += r.session_links_collided;
        totals.repo_links_moved += r.repo_links_moved;
        totals.repo_links_collided += r.repo_links_collided;
        // Archive the duplicate's prism note (recoverable via rkvr rcvr).
        let dup_note = vault
            .join(&config.vault.prisms_dir)
            .join(format!("{}.md", p.duplicate_slug));
        if dup_note.exists() {
            let out = std::process::Command::new("rkvr")
                .arg("rmrf")
                .arg(&dup_note)
                .output()
                .context("invoke rkvr rmrf")?;
            if !out.status.success() {
                rkvr_failures += 1;
            }
        }
        // Re-render the base so the merged moments / sessions / repos are visible.
        if let Some(base) = ledger.workitem_by_id(p.base_id)? {
            let moments = ledger.moments_for_workitem(p.base_id)?;
            let target = vault.join(&config.vault.prisms_dir).join(format!("{}.md", base.slug));
            if let Err(e) = facet::render::render_work_item_note(&target, &base, &moments) {
                eprintln!("rerender failed for {}: {e:#}", base.slug);
                rerender_failures += 1;
            }
        }
    }
    println!(
        "facet dedupe complete:\n  moments moved/collided:        {}/{}\n  cluster rows moved/collided:   {}/{}\n  session links moved/collided:  {}/{}\n  repo links moved/collided:     {}/{}",
        totals.moments_moved,
        totals.moments_collided,
        totals.cluster_rows_moved,
        totals.cluster_rows_collided,
        totals.session_links_moved,
        totals.session_links_collided,
        totals.repo_links_moved,
        totals.repo_links_collided,
    );
    if rkvr_failures > 0 {
        println!("  rkvr archive failures: {rkvr_failures}");
    }
    if rerender_failures > 0 {
        println!("  re-render failures:    {rerender_failures}");
    }
    Ok(())
}
