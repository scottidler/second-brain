use clap::{Args, Subcommand};
use std::path::PathBuf;
use std::sync::LazyLock;

use borg::opts;

use super::extension;

use super::tools::get_tool_validation_help;

static HELP_TEXT: LazyLock<String> = LazyLock::new(get_tool_validation_help);

#[derive(Args)]
#[command(after_help = HELP_TEXT.as_str())]
pub struct BorgCli {
    /// Path to config file
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Log level: trace, debug, info, warn, error
    #[arg(short, long)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage the daemon (install, start, stop, status, etc.)
    Daemon(DaemonArgs),
    /// Send a URL to the running daemon for ingestion
    Ingest {
        url: Option<String>,
        /// Read the URL from the clipboard instead of taking it as an argument
        #[arg(long)]
        clipboard: bool,
        /// Ingest this local file instead of a URL
        #[arg(long)]
        file: Option<PathBuf>,
        /// Operator tags to seed the note with (space-separated). They enter
        /// the classifier as author-side candidates, not as final tags
        #[arg(short, long, num_args = 0..)]
        tags: Option<Vec<String>>,
        /// Ingest even when the URL is already in the ledger (a reingest,
        /// which preserves the existing note's location and cortex fields)
        #[arg(long)]
        force: bool,
    },
    /// Quick text capture - create a note from text
    Note {
        text: Option<String>,
        /// Read the note text from the clipboard instead of an argument
        #[arg(long)]
        clipboard: bool,
        /// Operator tags to seed the note with (space-separated). They enter
        /// the classifier as author-side candidates, not as final tags
        #[arg(short, long, num_args = 0..)]
        tags: Option<Vec<String>>,
    },
    /// Install/uninstall a keyboard shortcut to ingest URLs from clipboard
    Hotkey(HotkeyArgs),
    /// Manage the Firefox browser extension (generate, validate, sign, install)
    Extension(extension::ExtensionCli),
    /// Migrate vault frontmatter to current schema. Without `--apply` this is a
    /// dry run (reports what would change); `--apply` is the gate that writes.
    Migrate {
        /// Write the changes. Without it this is a dry run
        #[arg(long)]
        apply: bool,
    },
    /// Audit ledger and vault for misclassified or broken entries
    Audit {
        /// Apply fixes. With no value, fixes every class. With one or more
        /// kinds (space-separated), fixes only those classes. Case-insensitive.
        #[arg(long, num_args = 0.., value_name = "KINDS", ignore_case = true)]
        fix: Option<Vec<borg::audit::FindingKind>>,
    },
    /// Query the receipts log (durable record of every input borg ever saw)
    Log(LogCliArgs),
    /// Show the daemon's ingest queue: `state: idle`, or the current batch
    /// (done, remaining, elapsed, wedged and failed items). Asks the daemon at
    /// `hotkey.host:hotkey.port`, so it answers the same from any machine
    Queue {
        /// yaml or json (default: yaml on a terminal, json when piped)
        #[arg(long, value_enum, ignore_case = true)]
        format: Option<crate::cli::output::Format>,
    },
    /// Block until the ingest batch in flight drains. Exit 0: nothing in
    /// flight, or the batch drained clean. 3: it drained with failures.
    /// 4: an item is wedged. 5: --timeout reached. 1: no answer from the
    /// daemon. Prints only the final snapshot; call it after the last submit
    Wait {
        /// Give up after this long (humantime: 90s, 15m, 1h)
        #[arg(long, default_value = "60m", value_parser = humantime::parse_duration)]
        timeout: std::time::Duration,
        /// yaml or json (default: yaml on a terminal, json when piped)
        #[arg(long, value_enum, ignore_case = true)]
        format: Option<crate::cli::output::Format>,
    },
    /// Reingest existing entries through the current pipeline
    Reingest {
        /// Reingest every ledger entry rather than a filtered subset
        #[arg(long)]
        all: bool,
        /// Only entries of this note type (youtube, article, github, ...)
        #[arg(long, value_name = "TYPE")]
        r#type: Option<String>,
        /// Only entries whose source URL contains this substring
        #[arg(long)]
        source: Option<String>,
        /// Only entries ingested on or before this date (YYYY-MM-DD)
        #[arg(long)]
        before: Option<String>,
        /// Only entries ingested on or after this date (YYYY-MM-DD)
        #[arg(long)]
        after: Option<String>,
        /// List what would be reingested and exit without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Replay the ingestion pipeline for staged traces or vault notes
    Replay(ReplayCliArgs),
    /// Manage staging retention
    Retention(RetentionCliArgs),
    /// Re-ingest every vault note whose body matches the failed-fetch signature
    ReingestFailed {
        /// List the notes that would be reingested and exit without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage the Gate-0 domain blocklist
    Blocklist(BlocklistCliArgs),
    /// Backfill the `ingested:` and `trace-expires:` frontmatter fields on
    /// assisted notes (homogenizes ingested; stamps the retention expiry on
    /// every note that carries a `trace:`).
    BackfillIngested {
        /// List the notes that would be stamped and exit without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Score distillation quality over golden fixtures (design 2026-07-05)
    Eval(EvalArgs),
    /// Harvest dormant Claude Code sessions from clyde into vault notes
    /// (design 2026-07-17). Selects, clusters into threads, distills, and
    /// publishes to the inbox through the normal borg pipeline.
    Harvest(HarvestCliArgs),
    /// Retire the surplus harvest-session-note forks a trace produced before
    /// the trace-keyed-replace fix (design 2026-08-15, Phase 6). Groups by
    /// `trace:`, tombstones every loser (never deletes), and backfills
    /// `harvest-body-hash:` where staging survives. Dry-run by default.
    DedupeSessions {
        /// Write the tombstones (and, with --purge, archive eligible ones).
        /// Without this flag, nothing on disk changes.
        #[arg(long)]
        apply: bool,
        /// Also rkvr-archive any tombstone (new this run, or left over from
        /// an earlier one) with zero live inbound wikilinks. Independent of
        /// --apply: --purge alone previews/archives existing tombstones
        /// without planning new ones.
        #[arg(long)]
        purge: bool,
    },
}

#[derive(Args)]
pub struct HarvestCliArgs {
    /// Scan window override: a relative span (7d, 24h) or a date. Deliberate
    /// backfill - takes precedence over the stored cursor. Omit for steady
    /// state (resume from the watermark cursor; first-ever run falls back to
    /// harvest.initial-since).
    #[arg(long)]
    pub since: Option<String>,
    /// List what would be selected and rejected, then exit WITHOUT writing
    /// anything (no notes, no receipts rows, no reject artifacts, no watermark
    /// advance). Forces dry-run regardless of the harvest.mode config default.
    #[arg(long)]
    pub dry_run: bool,
    /// Force a LIVE run (publish notes, write receipts, advance the watermark)
    /// regardless of the harvest.mode config default. The symmetric counterpart
    /// to --dry-run for a deliberate on-demand backfill without flipping the
    /// timer's harvest.mode. Mutually exclusive with --dry-run.
    #[arg(long, conflicts_with = "dry_run")]
    pub live: bool,
    /// Cap the number of candidate sessions pulled this run (clyde export page
    /// size). Lossless: clyde's paging is gap-free, so the next run resumes
    /// from the cursor.
    #[arg(long)]
    pub limit: Option<usize>,
    /// Dormancy override: treat a session as harvestable once idle this long
    /// (1d, 24h). Forwarded to clyde session export as --dormant-after.
    /// Precedence: this flag > BORG_HARVEST_DORMANT_AFTER env >
    /// harvest.dormant-after config.
    #[arg(long, env = "BORG_HARVEST_DORMANT_AFTER")]
    pub dormant_after: Option<String>,
    /// Re-distill and re-publish in-scope already-published sessions. Part of
    /// the watermark invariant ("I want a fresh distillation"), so it is
    /// contract surface, not a hidden flag.
    #[arg(long)]
    pub force: bool,
    /// Install the nightly systemd user timer (writes sb-harvest.{service,timer}
    /// to ~/.config/systemd/user/) and exit. OnCalendar comes from
    /// harvest.schedule; every other knob stays in borg.yml.
    #[arg(long)]
    pub install: bool,
    /// Remove the harvest systemd user timer units and exit.
    #[arg(long)]
    pub uninstall: bool,
}

#[derive(Args)]
pub struct EvalArgs {
    /// Root of the fixture tree. Default is REPO-RELATIVE
    /// (`config/eval/distill-fixtures`): `sb borg eval` is a developer command
    /// meant to run from the second-brain repo root. Pass an absolute path to
    /// run it elsewhere.
    #[arg(long, default_value = "config/eval/distill-fixtures")]
    pub fixtures: PathBuf,
    /// Hand-labeled calibration file (fixture -> human axis scores). Optional.
    #[arg(long, default_value = "config/eval/distill-calibration.yml")]
    pub calibration: PathBuf,
    /// Judge model name (empty = fabric's default model)
    #[arg(long, default_value = "")]
    pub judge_model: String,
    /// Ignore and overwrite cached judgments
    #[arg(long)]
    pub rebuild_cache: bool,
    /// Write a fillable calibration sheet to this path and skip metrics
    #[arg(long)]
    pub emit_calibration: Option<PathBuf>,
    /// Also write the rendered report to this path
    #[arg(long)]
    pub report: Option<PathBuf>,
}

impl From<&EvalArgs> for borg::eval::EvalOpts {
    fn from(a: &EvalArgs) -> Self {
        Self {
            fixtures_dir: vault::paths::expand_tilde(&a.fixtures),
            calibration_path: vault::paths::expand_tilde(&a.calibration),
            judge_model: a.judge_model.clone(),
            rebuild_cache: a.rebuild_cache,
            emit_calibration: a.emit_calibration.as_ref().map(vault::paths::expand_tilde),
        }
    }
}

#[derive(Args)]
pub struct HotkeyArgs {
    /// Install the desktop hotkey that ingests the clipboard URL
    #[arg(long)]
    pub install: bool,
    /// Remove the installed hotkey
    #[arg(long)]
    pub uninstall: bool,
    /// Key binding to register (desktop-environment syntax)
    #[arg(long, default_value = "<Ctrl><Shift>b")]
    pub key: String,
}
impl From<HotkeyArgs> for opts::HotkeyOpts {
    fn from(a: HotkeyArgs) -> Self {
        Self {
            install: a.install,
            uninstall: a.uninstall,
            key: a.key,
        }
    }
}

#[derive(Args)]
pub struct DaemonArgs {
    /// Write the systemd user unit for the borg daemon
    #[arg(long)]
    pub install: bool,
    /// Remove the systemd user unit
    #[arg(long)]
    pub uninstall: bool,
    /// Uninstall then install the systemd user unit
    #[arg(long)]
    pub reinstall: bool,
    /// Run the daemon in the foreground (the no-flag default)
    #[arg(long)]
    pub start: bool,
    /// Stop the running daemon
    #[arg(long)]
    pub stop: bool,
    /// Restart the running daemon
    #[arg(long)]
    pub restart: bool,
    /// Show the daemon's systemd status
    #[arg(long)]
    pub status: bool,
}
impl From<DaemonArgs> for opts::DaemonOpts {
    fn from(a: DaemonArgs) -> Self {
        Self {
            install: a.install,
            uninstall: a.uninstall,
            reinstall: a.reinstall,
            start: a.start,
            stop: a.stop,
            restart: a.restart,
            status: a.status,
        }
    }
}

#[derive(Args)]
pub struct LogCliArgs {
    /// Filter by receipt status (received | succeeded | failed | crashed |
    /// rejected). `rejected` is the harvest selection gate declining a
    /// below-bar candidate (distinct from a broken ingest).
    #[arg(long)]
    pub status: Option<String>,
    /// Filter by method (http | telegram | discord | ntfy | cli | clipboard |
    /// harvest).
    #[arg(long)]
    pub method: Option<String>,
    /// Filter failed/rejected rows by failure_stage (e.g. fetch-failed,
    /// publish-failed, selection - the harvest selection gate).
    #[arg(long)]
    pub stage: Option<String>,
    /// Lower bound on received_at (inclusive). Accepts a relative duration
    /// (5m, 2h, 7d), an ISO-8601 datetime (2026-06-04T05:18:59Z), or a date
    /// (2026-06-04).
    #[arg(long)]
    pub since: Option<String>,
    /// SQL LIKE pattern matched against raw_input (e.g. `%youtube.com%`).
    #[arg(long)]
    pub source: Option<String>,
    /// Show only degraded publishes (notes written from a distill fallback).
    #[arg(long)]
    pub degraded: bool,
    /// Cap the number of rows returned.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Show a single trace's full detail instead of the list view.
    #[arg(long)]
    pub trace: Option<String>,
}

#[derive(Args)]
pub struct BlocklistCliArgs {
    #[command(subcommand)]
    pub action: BlocklistAction,
}
#[derive(Subcommand)]
pub enum BlocklistAction {
    /// List every blocklisted domain
    List,
    /// Remove a single domain from the blocklist
    Remove { domain: String },
    /// Remove every entry from the blocklist
    Clear,
}

#[derive(Args)]
pub struct ReplayCliArgs {
    pub trace_id: Option<String>,
    /// Resume the pipeline at this stage instead of the beginning. Session
    /// traces only; any other kind rejects a non-zero value
    #[arg(long, default_value_t = 0)]
    pub from_stage: u8,
    /// Replay every trace received within this window (a relative span like
    /// 7d or 24h, ISO-8601, or a bare date)
    #[arg(long)]
    pub since: Option<String>,
    /// Replay only the traces that were rejected at the door
    #[arg(long)]
    pub rejected: bool,
    /// Re-fetch a pre-staging note by reading its frontmatter, for notes that
    /// predate the staging store. Requires --note
    #[arg(long)]
    pub bootstrap_from_vault: bool,
    /// The vault note to bootstrap from (used with --bootstrap-from-vault)
    #[arg(long)]
    pub note: Option<PathBuf>,
    /// List what would be replayed and exit without writing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct RetentionCliArgs {
    #[command(subcommand)]
    pub action: RetentionAction,
}
#[derive(Subcommand)]
pub enum RetentionAction {
    /// Sweep aged-off trace directories and raw-input sidecars
    Sweep {
        /// List what would be swept and exit without deleting
        #[arg(long)]
        dry_run: bool,
        /// Sweep ONLY the vault's raw-input sidecars, leaving staging alone.
        #[arg(long)]
        sidecars_only: bool,
    },
    /// Report trace counts and disk usage
    Status,
}
