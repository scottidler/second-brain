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

/// Outcome of a `sb cortex sweep` invocation. Captures the data each branch
/// produced; sb formats it for the terminal.
#[derive(Debug, Default)]
pub struct SweepReport {
    /// Populated when `--cold` was set: scan / surfaced / pinned-excluded counts.
    pub cold: Option<ColdStats>,
    /// Populated when `--migrate` was set: how many notes were (would be) rewritten,
    /// and whether this was a dry run.
    pub migration: Option<MigrationOutcome>,
    /// Populated when proposals were scanned (default mode or `--proposals`):
    /// the proposals found and, on apply, where they were written.
    pub proposals: Option<ProposalsOutcome>,
}

#[derive(Debug)]
pub struct MigrationOutcome {
    pub count: usize,
    pub dry_run: bool,
}

#[derive(Debug)]
pub struct ProposalsOutcome {
    pub proposals: Vec<Proposal>,
    /// `Some(path)` when proposals were written to disk; `None` when `--dry-run`
    /// (or no proposals were produced).
    pub written_to: Option<String>,
}

/// Top-level orchestrator for `sb cortex sweep`. Validates flag combinations,
/// branches between cold-sweep, tag migration, and proposal-scan modes, and
/// returns a typed report; sb formats the output.
pub fn run(vault_root: &Path, config: &Config, opts: &SweepOpts) -> Result<SweepReport> {
    log::info!("starting sweep command (vault_root={})", vault_root.display());

    if opts.cold && (opts.migrate || opts.proposals) {
        eyre::bail!("--cold cannot be combined with --migrate or --proposals");
    }

    if opts.cold {
        let stats = cold(vault_root, config)?;
        return Ok(SweepReport {
            cold: Some(stats),
            ..SweepReport::default()
        });
    }

    let notes = scan_vault(vault_root, &config.vault)?;
    let mut report = SweepReport::default();

    if opts.migrate {
        let count = migrate(vault_root, &notes, &config.sweep, opts.dry_run)?;
        report.migration = Some(MigrationOutcome {
            count,
            dry_run: opts.dry_run,
        });
    }

    if opts.proposals || !opts.migrate {
        let proposals = scan_proposals(&notes, &config.sweep)?;
        let written_to = if !proposals.is_empty() && !opts.dry_run {
            write_proposals(&config.sweep, proposals.clone())?;
            Some(config.sweep.proposals_path.clone())
        } else {
            None
        };
        report.proposals = Some(ProposalsOutcome { proposals, written_to });
    }

    Ok(report)
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
/// Rewrites each note's tags using the canonical mapping.
/// Returns the number of notes modified.
pub fn migrate(vault_root: &Path, notes: &[Note], config: &SweepConfig, dry_run: bool) -> Result<usize> {
    let canonical_file =
        CanonicalTagsFile::load(Path::new(&config.canonical_path)).wrap_err("failed to load canonical tags")?;
    let mapping =
        canonical::load_tag_mapping(Path::new(&config.mapping_path)).wrap_err("failed to load tag mapping")?;
    let canonical_set = canonical_file.all_tags();
    let max_per_note = canonical_file.max_per_note;

    let mut modified_count = 0;

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
            } else {
                let full_path = vault_root.join(&note.path);
                rewrite_note_tags(&full_path, &new_tags)?;
                log::info!(
                    "rewrote {}: {} -> {} tags",
                    note.path.display(),
                    tags.len(),
                    new_tags.len()
                );
            }
            modified_count += 1;
        }
    }

    Ok(modified_count)
}

/// Scan notes for non-canonical tags and generate proposals.
pub fn scan_proposals(notes: &[Note], config: &SweepConfig) -> Result<Vec<Proposal>> {
    let canonical_file =
        CanonicalTagsFile::load(Path::new(&config.canonical_path)).wrap_err("failed to load canonical tags")?;
    let mapping =
        canonical::load_tag_mapping(Path::new(&config.mapping_path)).wrap_err("failed to load tag mapping")?;
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
    let path = shellexpand::tilde(&config.proposals_path).to_string();
    let mut existing = load_proposals(&path).unwrap_or(ProposalsFile { proposals: Vec::new() });

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
    std::fs::write(&path, yaml).wrap_err("failed to write proposals file")?;
    Ok(())
}

fn load_proposals(path: &str) -> Result<ProposalsFile> {
    let content = std::fs::read_to_string(path).wrap_err("failed to read proposals file")?;
    let file: ProposalsFile = serde_yaml::from_str(&content).wrap_err("failed to parse proposals YAML")?;
    Ok(file)
}

fn rewrite_note_tags(path: &Path, new_tags: &[String]) -> Result<()> {
    let content = std::fs::read_to_string(path).wrap_err("failed to read note")?;
    if let Some(new_content) = replace_tags_in_frontmatter(&content, new_tags) {
        std::fs::write(path, new_content).wrap_err("failed to write note")?;
    }
    Ok(())
}

/// Entry point invoked by the cortex daemon's `cold_interval` tick.
/// Thin wrapper over `run_cold` that matches the
/// `crate::embed::daemon_tick` shape, so the daemon's `select!` arm is
/// a one-liner. Errors are propagated; the daemon translates them to
/// log lines so the runtime keeps ticking.
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
    let now = chrono::Utc::now().timestamp();
    let older_than = now - (cold.older_than_days as i64) * 86_400;
    let query = ColdQuery {
        older_than,
        limit: cold.limit,
    };

    let rows = index.cold_notes(&query).wrap_err("cold_notes query failed")?;
    let pinned_excluded = index
        .count_pinned_excluded(older_than)
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
                let date_str = chrono::DateTime::<chrono::Utc>::from_timestamp(row.modified_at, 0)
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let title = if row.title.is_empty() { "(untitled)".to_string() } else { row.title.clone() };
                out.push_str(&format!(
                    "- [ ] `{path}` - \"{title}\" - last modified {date_str}\n",
                    path = row.path,
                ));
            }
            out.push('\n');
        }
    }

    // Footer cross-references the dashboard view so a reviewer can hop
    // back to the broader vault overview after triaging.
    out.push_str("---\n\n");
    out.push_str("See also: [[borg-dashboard]] for vault activity overview.\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::NoteBuilder;

    fn make_config(dir: &Path) -> SweepConfig {
        let canonical_path = dir.join("canonical-tags.yml");
        let mapping_path = dir.join("tag-mapping.yml");
        let proposals_path = dir.join("tag-proposals.yml");

        std::fs::write(
            &canonical_path,
            "max-per-note: 3\nmax-canonical: 300\ntags:\n  ai:\n    - ai\n    - claude\n    - llm\n  tech:\n    - rust\n    - python\n",
        )
        .expect("write canonical");
        std::fs::write(
            &mapping_path,
            "ai-agents: ai\nai-coding: ai\nclaudecodeai: null\nrustlang: rust\n",
        )
        .expect("write mapping");
        std::fs::write(&proposals_path, "proposals: []\n").expect("write proposals");

        SweepConfig {
            canonical_path: canonical_path.to_string_lossy().to_string(),
            mapping_path: mapping_path.to_string_lossy().to_string(),
            proposals_path: proposals_path.to_string_lossy().to_string(),
            sweep_interval: "1h".to_string(),
            proposal_threshold: 2,
            cold: crate::config::ColdConfig::default(),
        }
    }

    fn make_cold_note(path: &str, title: &str, domain: &str, modified_at: i64) -> ColdNote {
        ColdNote {
            path: path.to_string(),
            title: title.to_string(),
            domain: domain.to_string(),
            modified_at,
        }
    }

    /// Fixed input used by both the snapshot assertion and the
    /// (ignored) regeneration test. Keeping the construction in a
    /// shared helper guarantees the regen path and the comparison
    /// path see identical input.
    fn snapshot_fixture_input() -> (Vec<ColdNote>, ColdStats, u32, chrono::DateTime<chrono::Utc>) {
        let rows = vec![
            // 2025-08-12T00:00:00Z = 1_754_956_800
            make_cold_note("notes/ai/old-paper.md", "Old Paper", "ai", 1_754_956_800),
            make_cold_note("notes/ai/forgotten.md", "Forgotten Thread", "ai", 1_754_956_800),
            make_cold_note("notes/diy/unused-jig.md", "Unused Jig", "diy", 1_754_956_800),
        ];
        let stats = ColdStats {
            scanned: 1_345,
            surfaced: 3,
            pinned_excluded: 7,
        };
        let now = chrono::DateTime::<chrono::Utc>::from_timestamp(1_747_569_600, 0).expect("fixed now");
        (rows, stats, 180, now)
    }

    const SNAPSHOT_FIXTURE_PATH: &str = "src/sweep/fixtures/cold-notes-expected.md";

    /// Byte-exact equality between `render_cold_report_at` output and
    /// the checked-in fixture. If this fails after an intentional format
    /// change, run `cargo test -p cortex regenerate_cold_report_snapshot
    /// -- --ignored` and review the diff before re-committing.
    #[test]
    fn cold_report_matches_snapshot_fixture() {
        let (rows, stats, days, now) = snapshot_fixture_input();
        let rendered = render_cold_report_at(&rows, &stats, days, now);
        let expected = std::fs::read_to_string(SNAPSHOT_FIXTURE_PATH).unwrap_or_else(|e| {
            panic!(
                "missing snapshot fixture at {SNAPSHOT_FIXTURE_PATH}: {e}. \
                 Regenerate with `cargo test -p cortex regenerate_cold_report_snapshot -- --ignored`."
            )
        });
        assert_eq!(
            rendered, expected,
            "cold-report snapshot drift; regenerate with --ignored after reviewing the diff",
        );
    }

    /// Regenerate the snapshot fixture. Run with `cargo test -p cortex
    /// regenerate_cold_report_snapshot -- --ignored` after an
    /// intentional format change, then inspect the diff and re-commit.
    #[test]
    #[ignore = "writes a checked-in fixture; opt in via --ignored"]
    fn regenerate_cold_report_snapshot() {
        let (rows, stats, days, now) = snapshot_fixture_input();
        let rendered = render_cold_report_at(&rows, &stats, days, now);
        std::fs::write(SNAPSHOT_FIXTURE_PATH, &rendered).expect("write snapshot fixture");
        eprintln!("wrote snapshot fixture to {SNAPSHOT_FIXTURE_PATH}");
    }

    #[test]
    fn render_cold_report_groups_by_domain_and_includes_metadata() {
        let rows = vec![
            // 2025-08-12T00:00:00Z = 1_754_956_800
            make_cold_note("notes/ai/a.md", "A Paper", "ai", 1_754_956_800),
            make_cold_note("notes/ai/b.md", "B Thing", "ai", 1_754_956_800),
            make_cold_note("notes/diy/c.md", "C Hack", "diy", 1_754_956_800),
        ];
        let stats = ColdStats {
            scanned: 100,
            surfaced: 3,
            pinned_excluded: 7,
        };
        let out = render_cold_report(&rows, &stats, 180);

        assert!(out.starts_with("---\n"), "frontmatter present");
        assert!(out.contains("older-than-days: 180"));
        assert!(out.contains("total-surfaced: 3"));
        assert!(out.contains("pinned-excluded: 7"));
        assert!(out.contains("pinned: true"), "report file marks itself pinned");
        assert!(out.contains("## ai (2)"));
        assert!(out.contains("## diy (1)"));
        assert!(out.contains("- [ ] `notes/ai/a.md`"));
        assert!(out.contains("\"A Paper\""));
        assert!(out.contains("last modified 2025-08-12"));
    }

    #[test]
    fn render_cold_report_empty_writes_placeholder() {
        let stats = ColdStats {
            scanned: 100,
            surfaced: 0,
            pinned_excluded: 0,
        };
        let out = render_cold_report(&[], &stats, 180);
        assert!(out.contains("No cold notes at the current threshold."));
    }

    #[test]
    fn render_cold_report_groups_empty_domain_as_no_domain() {
        let rows = vec![make_cold_note("notes/loose.md", "Loose", "", 1_754_956_800)];
        let stats = ColdStats {
            scanned: 1,
            surfaced: 1,
            pinned_excluded: 0,
        };
        let out = render_cold_report(&rows, &stats, 180);
        assert!(out.contains("## (no domain) (1)"));
    }

    /// `test_daemon_cold_tick_fires` per the design doc. Goes through
    /// `sweep::daemon_cold_tick` - the exact entry point the daemon's
    /// `select!` arm invokes - so a regression in (a) the daemon ->
    /// sweep wiring, (b) `config.oracle_db_path()` resolution, or (c)
    /// the run_cold body would all break this test. We intentionally do
    /// NOT drive `start_watching` in a spawned task: the tokio
    /// `interval` firing is tokio's responsibility, and the surrounding
    /// daemon orchestration (watcher, initial sweep, intel scheduling)
    /// pulls in heavy machinery for a path that's already covered by
    /// the surrounding unit tests. The chronology the design doc
    /// specifies ("a few seconds") is collapsed to a single synchronous
    /// invocation here.
    ///
    /// Linux-only because we redirect `dirs::data_local_dir()` via
    /// `XDG_DATA_HOME`. macOS resolves data_local_dir via system APIs
    /// that ignore env vars (see the `dirs` crate platform notes).
    #[cfg(target_os = "linux")]
    #[serial_test::serial(xdg_data_home)]
    #[test]
    fn test_daemon_cold_tick_fires() {
        use std::path::PathBuf;
        use vault::frontmatter::Frontmatter;
        use vault::note::Note;
        use vault::search::SearchIndex;

        let xdg_tmp = tempfile::tempdir().expect("xdg tmpdir");
        let vault_tmp = tempfile::tempdir().expect("vault tmpdir");

        // Redirect `dirs::data_local_dir()` so config.oracle_db_path()
        // resolves under our tempdir instead of the real user store.
        // safety: behind serial_test::serial(xdg_data_home), no
        // concurrent test mutates XDG_DATA_HOME.
        let prior = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: tests serialized by the `xdg_data_home` lock; no
        // concurrent reader exists, so the mutation is sound here.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", xdg_tmp.path());
        }

        let result = std::panic::catch_unwind(|| {
            // Resolve the DB path the same way production does.
            let config = crate::config::Config::default();
            let db_path = config.oracle_db_path();
            assert!(
                db_path.starts_with(xdg_tmp.path()),
                "XDG_DATA_HOME redirect did not take: db_path={}",
                db_path.display(),
            );
            std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("mkdir db parent");

            // Pre-seed the DB with a cold note (no signals, old mtime).
            let index = SearchIndex::open(&db_path).expect("open db");
            let fm = Frontmatter {
                title: Some("Stale Note".to_string()),
                note_type: Some("article".to_string()),
                origin: Some("assisted".to_string()),
                domain: Some("ai".to_string()),
                ..Frontmatter::default()
            };
            let note = Note {
                path: PathBuf::from("notes/ai/stale.md"),
                frontmatter: fm,
                body: "## Summary\n\nS.\n".to_string(),
                raw: String::new(),
            };
            index.index_one(&note, 1_000).expect("seed cold note");
            drop(index);

            // Invoke the same function the daemon's select! arm calls.
            let stats = daemon_cold_tick(vault_tmp.path(), &config).expect("daemon_cold_tick");
            assert_eq!(stats.surfaced, 1, "the cold note must surface");

            let report = vault_tmp.path().join("system").join("views").join("cold-notes.md");
            assert!(report.exists(), "report file must appear at {}", report.display());
            let body = std::fs::read_to_string(&report).expect("read report");
            assert!(body.contains("`notes/ai/stale.md`"), "report must list the cold note");
        });

        // SAFETY: same serialization guarantee as the set_var above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    #[test]
    fn cold_with_index_counts_pinned_excluded() {
        use std::path::PathBuf;
        use vault::frontmatter::Frontmatter;
        use vault::note::Note;
        use vault::search::SearchIndex;

        let index = SearchIndex::open_memory().expect("open");
        // Old, otherwise-cold, pinned: should be counted as
        // pinned_excluded, NOT surfaced.
        let fm_pinned = Frontmatter {
            title: Some("Pinned".to_string()),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            domain: Some("ai".to_string()),
            pinned: Some(true),
            ..Frontmatter::default()
        };
        let pinned_note = Note {
            path: PathBuf::from("notes/ai/pinned.md"),
            frontmatter: fm_pinned,
            body: "## Summary\n\nP.\n".to_string(),
            raw: String::new(),
        };
        index.index_one(&pinned_note, 1_000).expect("index pinned");

        // Old, unpinned, cold: should surface.
        let fm_cold = Frontmatter {
            title: Some("Cold".to_string()),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            domain: Some("ai".to_string()),
            ..Frontmatter::default()
        };
        let cold_note = Note {
            path: PathBuf::from("notes/ai/cold.md"),
            frontmatter: fm_cold,
            body: "## Summary\n\nC.\n".to_string(),
            raw: String::new(),
        };
        index.index_one(&cold_note, 1_000).expect("index cold");

        let vault_root = tempfile::tempdir().expect("tmpdir");
        let cold_config = crate::config::ColdConfig {
            older_than_days: 30,
            limit: 100,
        };
        let stats = cold_with_index(vault_root.path(), &index, &cold_config).expect("run_cold");

        assert_eq!(stats.scanned, 2, "two indexed notes");
        assert_eq!(stats.surfaced, 1, "only the unpinned one surfaces");
        assert_eq!(stats.pinned_excluded, 1, "the pinned one counts in the floor stat");

        let report_path = vault_root.path().join("system").join("views").join("cold-notes.md");
        let body = std::fs::read_to_string(&report_path).expect("read");
        assert!(body.contains("`notes/ai/cold.md`"));
        assert!(!body.contains("`notes/ai/pinned.md`"), "pinned must not surface");
        assert!(body.contains("pinned-excluded: 1"));
    }

    #[test]
    fn cold_with_index_writes_report_atomically() {
        // The same fixture pattern the daemon test would otherwise need
        // (driving the daemon for a few seconds and asserting the file
        // appears). Going through `cold_with_index` directly skips
        // the tokio interval but exercises every other step the daemon
        // tick runs, so a regression in the SQL, render, or atomic
        // write will surface here.
        use std::path::PathBuf;
        use vault::frontmatter::Frontmatter;
        use vault::note::Note;
        use vault::search::SearchIndex;

        let index = SearchIndex::open_memory().expect("open");
        let fm_cold = Frontmatter {
            title: Some("Old Paper".to_string()),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            domain: Some("ai".to_string()),
            ..Frontmatter::default()
        };
        let cold_note = Note {
            path: PathBuf::from("notes/ai/old.md"),
            frontmatter: fm_cold,
            body: "## Summary\n\nO.\n".to_string(),
            raw: String::new(),
        };
        index.index_one(&cold_note, 1_000).expect("index");

        let vault_root = tempfile::tempdir().expect("tmpdir");
        let cold_config = crate::config::ColdConfig {
            older_than_days: 30,
            limit: 100,
        };
        let stats = cold_with_index(vault_root.path(), &index, &cold_config).expect("run_cold");

        assert_eq!(stats.scanned, 1);
        assert_eq!(stats.surfaced, 1);
        assert_eq!(stats.pinned_excluded, 0);

        let report_path = vault_root.path().join("system").join("views").join("cold-notes.md");
        assert!(
            report_path.exists(),
            "report file should exist at {}",
            report_path.display()
        );
        let body = std::fs::read_to_string(&report_path).expect("read report");
        assert!(body.contains("## ai (1)"));
        assert!(body.contains("`notes/ai/old.md`"));
        assert!(body.contains("\"Old Paper\""));
        // Atomic write: temp file should not survive the rename.
        let tmp_path = report_path.with_extension("md.tmp");
        assert!(!tmp_path.exists(), "temp file should not linger");
    }

    #[test]
    fn render_cold_report_handles_missing_title() {
        let rows = vec![make_cold_note("notes/a.md", "", "ai", 1_754_956_800)];
        let stats = ColdStats {
            scanned: 1,
            surfaced: 1,
            pinned_excluded: 0,
        };
        let out = render_cold_report(&rows, &stats, 180);
        assert!(out.contains("\"(untitled)\""));
    }

    #[test]
    fn test_scan_proposals_finds_non_canonical() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let config = make_config(dir.path());

        let notes = vec![
            NoteBuilder::new("notes/a.md").tags(&["unknown-tag", "ai"]).build(),
            NoteBuilder::new("notes/b.md").tags(&["unknown-tag", "rust"]).build(),
            NoteBuilder::new("notes/c.md").tags(&["other-tag", "python"]).build(),
        ];

        let proposals = scan_proposals(&notes, &config).expect("scan");
        // "unknown-tag" appears on 2 notes, meets threshold of 2
        assert!(proposals.iter().any(|p| p.tag == "unknown-tag"));
        // "other-tag" appears on 1 note, below threshold
        assert!(!proposals.iter().any(|p| p.tag == "other-tag"));
    }

    #[test]
    fn test_scan_proposals_mapped_tags_not_proposed() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let config = make_config(dir.path());

        let notes = vec![
            NoteBuilder::new("notes/a.md").tags(&["ai-agents", "rustlang"]).build(),
            NoteBuilder::new("notes/b.md").tags(&["ai-agents", "python"]).build(),
        ];

        let proposals = scan_proposals(&notes, &config).expect("scan");
        // ai-agents maps to "ai" in the mapping file, so it should NOT be proposed
        assert!(proposals.is_empty());
    }
}
