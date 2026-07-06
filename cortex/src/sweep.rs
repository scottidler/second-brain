use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use vault::canonical::{self, CanonicalTagsFile};
use vault::search::{ColdNote, ColdQuery, SearchIndex};

use crate::config::{Config, SweepConfig};
use crate::opts::SweepOpts;
use crate::tags::replace_tags_in_frontmatter;
use crate::vault::{Note, scan_vault};

/// Outcome of a `sb cortex sweep` invocation. `mode` is the primary action
/// the user requested; `proposals` is orthogonal and populated whenever a
/// proposal scan ran (default mode, `--proposals`, or `--migrate --proposals`).
#[derive(Debug)]
pub struct SweepReport {
    pub mode: SweepMode,
    pub proposals: Option<Vec<Proposal>>,
    pub proposals_path: Option<String>,
}

/// Mirrors the Reingest/Backfill disambiguation rule: dry-run and apply
/// produce distinct enum variants so sb never has to consult input opts
/// to format output.
#[derive(Debug)]
pub enum SweepMode {
    WouldMigrate {
        count: usize,
    },
    Migrated {
        count: usize,
    },
    Proposals,
    Cold {
        scanned: u64,
        surfaced: u64,
        pinned_excluded: u64,
    },
}

/// Top-level orchestrator for `sb cortex sweep`. Validates flag combinations,
/// branches between cold-sweep, tag migration, and proposal-scan modes, and
/// returns a typed report; sb formats the output.
pub fn run(vault_root: &Path, config: &Config, opts: &SweepOpts) -> Result<SweepReport> {
    crate::startup::validate_canonical_assets()?;
    log::info!("starting sweep command (vault_root={})", vault_root.display());

    if opts.cold && (opts.migrate || opts.proposals) {
        eyre::bail!("--cold cannot be combined with --migrate or --proposals");
    }

    if opts.cold {
        let stats = cold(vault_root, config)?;
        return Ok(SweepReport {
            mode: SweepMode::Cold {
                scanned: stats.scanned,
                surfaced: stats.surfaced,
                pinned_excluded: stats.pinned_excluded,
            },
            proposals: None,
            proposals_path: None,
        });
    }

    let notes = scan_vault(vault_root, &config.vault)?;

    let migrate_count = if opts.migrate {
        Some(migrate(vault_root, &notes, &config.sweep, opts.dry_run)?.len())
    } else {
        None
    };

    let proposals_data = if opts.proposals || !opts.migrate {
        let proposals = scan_proposals(&notes, &config.sweep)?;
        let path = if !proposals.is_empty() && !opts.dry_run {
            write_proposals(&config.sweep, proposals.clone())?;
            Some(config.sweep.proposals_path.display().to_string())
        } else {
            None
        };
        Some((proposals, path))
    } else {
        None
    };

    let mode = match migrate_count {
        Some(count) if opts.dry_run => SweepMode::WouldMigrate { count },
        Some(count) => SweepMode::Migrated { count },
        None => SweepMode::Proposals,
    };

    let (proposals, proposals_path) = match proposals_data {
        Some((p, path)) => (Some(p), path),
        None => (None, None),
    };

    Ok(SweepReport {
        mode,
        proposals,
        proposals_path,
    })
}

/// Stats from a single cold-sweep run. Surfaced in the cortex log and
/// embedded in the report's frontmatter so a reviewer can tell at a
/// glance how the floor is doing.
#[derive(Debug, Clone, Copy)]
pub struct ColdStats {
    pub scanned: u64,
    pub surfaced: u64,
    pub pinned_excluded: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Proposal {
    pub tag: String,
    pub frequency: usize,
    pub suggested_canonical: Option<String>,
    pub action: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalsFile {
    pub proposals: Vec<Proposal>,
}

/// Run tag sweep migration on all notes in the vault.
///
/// Rewrites each note's tags using the canonical mapping. In `dry_run`,
/// returns the notes that WOULD be rewritten (a prediction - nothing is
/// written). Otherwise returns only the notes `rewrite_note_tags` actually
/// wrote: `new_tags != tags` alone is not sufficient - `replace_tags_in_frontmatter`
/// can return `None` (e.g. malformed/missing frontmatter) and leave the file
/// untouched, so counting on the tag-diff alone would fingerprint a path
/// whose bytes never changed, tripping the daemon's oscillation detector on
/// phantom churn.
pub fn migrate(vault_root: &Path, notes: &[Note], config: &SweepConfig, dry_run: bool) -> Result<Vec<String>> {
    crate::startup::validate_canonical_assets()?;
    let canonical_file = CanonicalTagsFile::load(&config.canonical_path).wrap_err("failed to load canonical tags")?;
    let mapping = canonical::load_tag_mapping(&config.mapping_path).wrap_err("failed to load tag mapping")?;
    let canonical_set = canonical_file.all_tags();
    let max_per_note = canonical_file.max_per_note;

    // Real changed-path list (would-rewrite in dry-run, actually-rewritten
    // otherwise) so the daemon's oscillation fingerprint compares consecutive
    // sweeps by file.
    let mut modified = Vec::new();

    for note in notes {
        let tags = note.frontmatter.tags.clone().unwrap_or_default();

        if tags.is_empty() {
            continue;
        }

        let new_tags = canonical::filter_and_cap(&tags, &canonical_set, &mapping, max_per_note);

        if new_tags != tags {
            if dry_run {
                let dropped: Vec<_> = tags.iter().filter(|t| !new_tags.contains(t)).collect();
                log::info!(
                    "would rewrite {}: {} -> {} tags (drop: {:?})",
                    note.path.display(),
                    tags.len(),
                    new_tags.len(),
                    dropped
                );
                modified.push(note.path.to_string_lossy().to_string());
            } else {
                let full_path = vault_root.join(&note.path);
                if rewrite_note_tags(&full_path, &new_tags)? {
                    log::info!(
                        "rewrote {}: {} -> {} tags",
                        note.path.display(),
                        tags.len(),
                        new_tags.len()
                    );
                    modified.push(note.path.to_string_lossy().to_string());
                } else {
                    log::warn!(
                        "skipping tag rewrite for {}: frontmatter block not found or unparseable",
                        note.path.display()
                    );
                }
            }
        }
    }

    Ok(modified)
}

/// Scan notes for non-canonical tags and generate proposals.
pub fn scan_proposals(notes: &[Note], config: &SweepConfig) -> Result<Vec<Proposal>> {
    crate::startup::validate_canonical_assets()?;
    let canonical_file = CanonicalTagsFile::load(&config.canonical_path).wrap_err("failed to load canonical tags")?;
    let mapping = canonical::load_tag_mapping(&config.mapping_path).wrap_err("failed to load tag mapping")?;
    let canonical_set = canonical_file.all_tags();

    // Count non-canonical tags across all notes
    let mut non_canonical: HashMap<String, Vec<String>> = HashMap::new();

    for note in notes {
        let tags = note.frontmatter.tags.clone().unwrap_or_default();

        for tag in &tags {
            let matches = canonical::match_to_canonical(tag, &canonical_set, &mapping);
            if matches.is_empty() {
                non_canonical
                    .entry(tag.clone())
                    .or_default()
                    .push(note.path.to_string_lossy().to_string());
            }
        }
    }

    // Filter to tags meeting proposal threshold
    let threshold = config.proposal_threshold;
    let proposals: Vec<Proposal> = non_canonical
        .into_iter()
        .filter(|(_, notes)| notes.len() >= threshold)
        .map(|(tag, notes)| Proposal {
            frequency: notes.len(),
            suggested_canonical: None,
            action: "review".to_string(),
            notes,
            tag,
        })
        .collect();

    Ok(proposals)
}

/// Write proposals to the proposals file, merging with existing.
pub fn write_proposals(config: &SweepConfig, new_proposals: Vec<Proposal>) -> Result<()> {
    // proposals_path is a PathBuf already tilde-expanded at config-load time
    // (deserialize_tilde_pathbuf), so no shellexpand here.
    let path = &config.proposals_path;
    let mut existing = load_proposals(path).unwrap_or(ProposalsFile { proposals: Vec::new() });

    // Merge: update frequency for existing tags, add new ones
    for proposal in new_proposals {
        if let Some(existing_proposal) = existing.proposals.iter_mut().find(|p| p.tag == proposal.tag) {
            existing_proposal.frequency = proposal.frequency;
            existing_proposal.notes = proposal.notes;
        } else {
            existing.proposals.push(proposal);
        }
    }

    let yaml = serde_yaml::to_string(&existing).wrap_err("failed to serialize proposals")?;
    std::fs::write(path, yaml).wrap_err("failed to write proposals file")?;
    Ok(())
}

fn load_proposals(path: &Path) -> Result<ProposalsFile> {
    let content = std::fs::read_to_string(path).wrap_err("failed to read proposals file")?;
    let file: ProposalsFile = serde_yaml::from_str(&content).wrap_err("failed to parse proposals YAML")?;
    Ok(file)
}

/// Rewrite `path`'s frontmatter `tags:` entry to `new_tags`. Returns `true`
/// only when a write actually landed - `false` when `replace_tags_in_frontmatter`
/// found no rewriteable frontmatter block (the caller must not count this as
/// a modified file).
fn rewrite_note_tags(path: &Path, new_tags: &[String]) -> Result<bool> {
    let content = std::fs::read_to_string(path).wrap_err("failed to read note")?;
    match replace_tags_in_frontmatter(&content, new_tags) {
        Some(new_content) => {
            vault::note::write_atomic(path, new_content.as_bytes()).wrap_err("failed to write note")?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Entry point invoked by the cortex daemon's `cold_interval` tick.
/// Thin wrapper over `cold` so the daemon's `select!` arm is a one-liner.
/// Errors are propagated; the daemon translates them to log lines so
/// the runtime keeps ticking.
pub fn daemon_cold_tick(vault_root: &Path, config: &Config) -> Result<ColdStats> {
    cold(vault_root, config)
}

/// Generate the cold-note review report.
///
/// Opens the oracle search DB, runs the cold query, groups by domain,
/// renders a markdown checklist, writes it atomically to
/// `<vault_root>/system/views/cold-notes.md`. Cortex writes nothing to
/// the `notes` table; the inbound counts it reads are whatever oracle's
/// periodic recompute most recently materialized.
pub fn cold(vault_root: &Path, config: &Config) -> Result<ColdStats> {
    log::debug!(
        "run_cold: vault_root={} older_than_days={} limit={}",
        vault_root.display(),
        config.sweep.cold.older_than_days,
        config.sweep.cold.limit,
    );

    let db_path = config.oracle_db_path();
    let index = SearchIndex::open(&db_path).wrap_err_with(|| {
        format!(
            "failed to open oracle search DB at {} - has oracle ever indexed this vault?",
            db_path.display()
        )
    })?;

    cold_with_index(vault_root, &index, &config.sweep.cold)
}

/// Inner cold-sweep entrypoint that takes an already-open `SearchIndex`.
/// Lets tests synthesize an in-memory DB and exercise the full render +
/// write path without going through `config.oracle_db_path()`. The
/// daemon tick uses the outer `run_cold` so it stays a one-line `select!`
/// arm matching the embed-tick shape.
pub fn cold_with_index(vault_root: &Path, index: &SearchIndex, cold: &crate::config::ColdConfig) -> Result<ColdStats> {
    // Floor is a content-date string: a note is cold when its `date:`
    // frontmatter is lexically older than this. Subtracting from today's
    // calendar date keeps the comparison in the same `YYYY-MM-DD` space as
    // the normalized `date` column.
    let before_date = (chrono::Utc::now().date_naive() - chrono::Duration::days(cold.older_than_days as i64))
        .format("%Y-%m-%d")
        .to_string();
    let query = ColdQuery {
        before_date: before_date.clone(),
        limit: cold.limit,
    };

    let rows = index.cold_notes(&query).wrap_err("cold_notes query failed")?;
    let pinned_excluded = index
        .count_pinned_excluded(&before_date)
        .wrap_err("count_pinned_excluded failed")?;
    let scanned = index.count_notes().wrap_err("count_notes failed")?;

    if scanned == 0 {
        // First-run-no-oracle case (or genuinely empty vault): write the
        // placeholder report and surface a hint so an operator who ran
        // `cortex sweep --cold` before oracle ever indexed knows what
        // happened.
        log::warn!("run_cold: notes table is empty - oracle reindex has not yet run, or vault is brand new");
    }

    let stats = ColdStats {
        scanned,
        surfaced: rows.len() as u64,
        pinned_excluded,
    };

    let report = render_cold_report(&rows, &stats, cold.older_than_days);
    let out_path = vault_root.join("system").join("views").join("cold-notes.md");
    atomic_write(&out_path, &report).wrap_err_with(|| format!("failed to write {}", out_path.display()))?;

    log::info!(
        "run_cold: scanned={} surfaced={} pinned_excluded={} report={}",
        stats.scanned,
        stats.surfaced,
        stats.pinned_excluded,
        out_path.display(),
    );
    Ok(stats)
}

fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

/// Render the cold-note report with the current wall-clock timestamp.
/// Thin wrapper over `render_cold_report_at` so production code stays
/// terse; tests use the `_at` variant directly for byte-exact snapshot
/// comparison against a checked-in fixture.
pub fn render_cold_report(rows: &[ColdNote], stats: &ColdStats, older_than_days: u32) -> String {
    let now = chrono::Utc::now();
    render_cold_report_at(rows, stats, older_than_days, now)
}

/// Render the cold-note report with an explicit `now` for the
/// `generated-at` frontmatter field. Splitting the timestamp out as a
/// parameter is what makes the snapshot fixture in
/// `cortex/src/sweep/fixtures/cold-notes-expected.md` reproducible.
pub fn render_cold_report_at(
    rows: &[ColdNote],
    stats: &ColdStats,
    older_than_days: u32,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let generated_at = now.format("%Y-%m-%dT%H:%M:%SZ");

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("generated-at: {generated_at}\n"));
    out.push_str("generator: cortex sweep --cold\n");
    out.push_str(&format!("older-than-days: {older_than_days}\n"));
    out.push_str(&format!("total-surfaced: {}\n", stats.surfaced));
    out.push_str(&format!("pinned-excluded: {}\n", stats.pinned_excluded));
    // The report itself carries pinned: true so it can never qualify
    // as cold once oracle reindexes it as a system-view note.
    out.push_str("pinned: true\n");
    out.push_str("---\n\n");

    out.push_str("# Cold Notes\n\n");
    out.push_str(&format!(
        "Notes older than **{older_than_days} days** with no reads, no inbound links, and not pinned. \
         Decide per row: archive, delete, leave, promote.\n\n"
    ));
    out.push_str(
        "This file is regenerated weekly by `cortex sweep --cold`. Do not edit \
         manually; pin a note (`pinned: true` in its frontmatter) to remove it \
         from this report.\n\n",
    );

    if rows.is_empty() {
        out.push_str("No cold notes at the current threshold.\n\n");
    } else {
        // Group rows by domain, preserving stable order for snapshot tests.
        let mut groups: BTreeMap<String, Vec<&ColdNote>> = BTreeMap::new();
        for row in rows {
            let key = if row.domain.is_empty() {
                "(no domain)".to_string()
            } else {
                row.domain.clone()
            };
            groups.entry(key).or_default().push(row);
        }

        for (domain, domain_rows) in &groups {
            out.push_str(&format!("## {domain} ({count})\n\n", count = domain_rows.len()));
            for row in domain_rows {
                let title = if row.title.is_empty() { "(untitled)".to_string() } else { row.title.clone() };
                out.push_str(&format!(
                    "- [ ] `{path}` - \"{title}\" - dated {date}\n",
                    path = row.path,
                    date = row.date,
                ));
            }
            out.push('\n');
        }
    }

    // Footer cross-references the live ingest-activity view so a reviewer can
    // hop to the broader vault overview after triaging. The old
    // `borg-dashboard` markdown was retired; the current view is the
    // live-updating `borg-ledger.base`.
    out.push_str("---\n\n");
    out.push_str("See also: [[borg-ledger]] for vault ingest activity.\n");

    out
}

#[cfg(test)]
mod tests;
