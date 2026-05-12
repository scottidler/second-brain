use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::LazyLock;

static HELP_TEXT: LazyLock<String> = LazyLock::new(get_tool_validation_help);

#[derive(Parser)]
#[command(
    name = "borg",
    about = "Obsidian ingestion daemon - receives URLs and produces summarized markdown notes",
    version = env!("GIT_DESCRIBE"),
    after_help = HELP_TEXT.as_str()
)]
pub struct Cli {
    /// Path to config file
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage the daemon (install, start, stop, status, etc.)
    Daemon(DaemonOpts),
    /// Send a URL to the running daemon for ingestion
    Ingest {
        /// URL to ingest (omit when using --clipboard or --file)
        url: Option<String>,
        /// Read URL from system clipboard
        #[arg(long)]
        clipboard: bool,
        /// Ingest a local file (image, pdf, etc.)
        #[arg(long)]
        file: Option<PathBuf>,
        /// Comma-separated tags
        #[arg(short, long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
        /// Force re-ingestion even if URL was already processed
        #[arg(long)]
        force: bool,
    },
    /// Quick text capture - create a note from text
    Note {
        /// Text to capture (omit when using --clipboard)
        text: Option<String>,
        /// Read text from system clipboard
        #[arg(long)]
        clipboard: bool,
        /// Comma-separated tags
        #[arg(short, long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
    },
    /// Install/uninstall a keyboard shortcut to ingest URLs from clipboard
    Hotkey(HotkeyOpts),
    /// Sign the browser extension for Firefox (AMO)
    Sign,
    /// Migrate vault frontmatter to current schema
    Migrate {
        /// Preview changes without writing (default)
        #[arg(long)]
        dry_run: bool,
        /// Apply changes to vault files
        #[arg(long)]
        apply: bool,
    },
    /// Audit ledger and vault for misclassified or broken entries.
    /// With --invariant, walk intake/ledger/dlq and report orphans -
    /// trace_ids in the intake log that have no resolution.
    Audit {
        /// Fix misclassified types in vault notes
        #[arg(long)]
        fix: bool,
        /// Run the intake -> ledger / dlq invariant audit instead of the
        /// classification audit. Writes `system/views/borg-orphans.md`.
        #[arg(long)]
        invariant: bool,
        /// Maximum seconds an intake row can lack a ledger/dlq resolution
        /// before it is reported as an orphan.
        #[arg(long, default_value_t = 1800)]
        bound_secs: u64,
    },
    /// Inspect the intake log (every input borg received).
    Intake(IntakeCliArgs),
    /// Inspect and replay the dead letter queue.
    Dlq(DlqCliArgs),
    /// Reingest existing entries through the current pipeline
    Reingest {
        /// Reingest all completed entries
        #[arg(long)]
        all: bool,
        /// Filter by content type (youtube, article, github, social, reddit)
        #[arg(long, value_name = "TYPE")]
        r#type: Option<String>,
        /// Filter by domain (ai, tech, football, etc.)
        #[arg(long)]
        domain: Option<String>,
        /// Reingest a specific URL
        #[arg(long)]
        source: Option<String>,
        /// Filter entries ingested before date (YYYY-MM-DD)
        #[arg(long)]
        before: Option<String>,
        /// Filter entries ingested after date (YYYY-MM-DD)
        #[arg(long)]
        after: Option<String>,
        /// Preview without reingesting
        #[arg(long)]
        dry_run: bool,
    },
    /// Replay the ingestion pipeline for staged traces or vault notes
    Replay(ReplayCliArgs),
    /// Manage staging retention
    Retention(RetentionCliArgs),
    /// Re-ingest every vault note whose body matches the failed-fetch
    /// signature (the migration path for the 28 bad XDA-style notes).
    ReingestFailed {
        /// Preview matches without re-ingesting
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage the Gate-0 domain blocklist (list / remove / clear)
    Blocklist(BlocklistCliArgs),
    /// Backfill the `ingested:` frontmatter field on assisted notes that
    /// were written before the intake-log + DLQ design shipped.
    BackfillIngested {
        /// Preview the planned writes without modifying any files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage the Borg Dashboard view (refresh to the latest template)
    Dashboard(DashboardCliArgs),
}

#[derive(Parser, Debug)]
pub struct DashboardCliArgs {
    #[command(subcommand)]
    pub action: DashboardAction,
}

#[derive(Subcommand, Debug)]
pub enum DashboardAction {
    /// Rewrite the dashboard with the current canonical template (used to
    /// upgrade a dashboard that pre-dates a query-schema change).
    Refresh,
}

#[derive(Parser, Debug)]
pub struct IntakeCliArgs {
    #[command(subcommand)]
    pub action: IntakeAction,
}

#[derive(Subcommand, Debug)]
pub enum IntakeAction {
    /// List recent intake rows.
    List {
        /// Filter by ingest method (telegram, http, ntfy, discord, cli).
        #[arg(long)]
        method: Option<String>,
        /// Only rows on or after this date (YYYY-MM-DD).
        #[arg(long)]
        since: Option<String>,
        /// Maximum number of rows.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show the intake row + raw-input sidecar for a single trace.
    Show { trace_id: String },
}

#[derive(Parser, Debug)]
pub struct DlqCliArgs {
    #[command(subcommand)]
    pub action: DlqAction,
}

#[derive(Subcommand, Debug)]
pub enum DlqAction {
    /// List DLQ rows.
    List {
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        stage: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show one DLQ row + its intake row + the raw-input sidecar.
    Show { trace_id: String },
    /// Mark a DLQ row as resolved (or another status). When invoked with
    /// `--resolved`, move every row whose status is already resolved or
    /// abandoned to `borg-dlq-archive.md`.
    Archive {
        /// Trace id to update. Omit when using `--resolved`.
        trace_id: Option<String>,
        /// Status to write (resolved by default).
        #[arg(long, default_value = "resolved")]
        status: String,
        /// Move every resolved/abandoned row to the archive file.
        #[arg(long)]
        resolved: bool,
    },
    /// Replay a DLQ entry: generates a new trace_id with `replay_of`
    /// pointing at the original and re-injects the input.
    Replay { trace_id: String },
}

#[derive(Parser, Debug)]
pub struct BlocklistCliArgs {
    #[command(subcommand)]
    pub action: BlocklistAction,
}

#[derive(Subcommand, Debug)]
pub enum BlocklistAction {
    /// List every blocklisted domain with its retriable-after timestamp
    List,
    /// Remove a single domain from the blocklist
    Remove { domain: String },
    /// Remove every entry from the blocklist
    Clear,
}

#[derive(Parser, Debug)]
pub struct ReplayCliArgs {
    /// Specific trace ID to replay
    pub trace_id: Option<String>,
    /// Start replay at this stage (keep artifacts from earlier stages)
    #[arg(long, default_value_t = 0)]
    pub from_stage: u8,
    /// Replay all traces from the last N duration (e.g. "7d", "24h")
    #[arg(long)]
    pub since: Option<String>,
    /// Only replay rejected traces
    #[arg(long)]
    pub rejected: bool,
    /// Seed replay from a vault note's frontmatter (for pre-staging notes)
    #[arg(long)]
    pub bootstrap_from_vault: bool,
    /// Path to a specific vault note (required with --bootstrap-from-vault)
    #[arg(long)]
    pub note: Option<PathBuf>,
    /// Dry-run: print actions without executing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Parser, Debug)]
pub struct RetentionCliArgs {
    #[command(subcommand)]
    pub action: RetentionAction,
}

#[derive(Subcommand, Debug)]
pub enum RetentionAction {
    /// Sweep aged-off trace directories
    Sweep {
        #[arg(long)]
        dry_run: bool,
    },
    /// Report trace counts and disk usage
    Status,
}

#[derive(Parser, Debug)]
pub struct HotkeyOpts {
    /// Install the keyboard shortcut
    #[arg(long)]
    pub install: bool,

    /// Uninstall the keyboard shortcut
    #[arg(long)]
    pub uninstall: bool,

    /// Daemon host to send URLs to (default: localhost)
    #[arg(long, default_value = "localhost")]
    pub host: String,

    /// Daemon port (default: 8181)
    #[arg(long, default_value_t = 8181)]
    pub port: u16,

    /// Key binding in GNOME format (default: <Ctrl><Shift>b)
    #[arg(long, default_value = "<Ctrl><Shift>b")]
    pub key: String,
}

#[derive(Parser, Debug)]
pub struct DaemonOpts {
    /// Install system service (idempotent - safe to run repeatedly)
    #[arg(long)]
    pub install: bool,

    /// Uninstall system service
    #[arg(long)]
    pub uninstall: bool,

    /// Reinstall system service (full teardown then install)
    #[arg(long)]
    pub reinstall: bool,

    /// Start daemon (used by systemd ExecStart)
    #[arg(long)]
    pub start: bool,

    /// Stop daemon
    #[arg(long)]
    pub stop: bool,

    /// Restart daemon
    #[arg(long)]
    pub restart: bool,

    /// Show daemon status
    #[arg(long)]
    pub status: bool,
}

fn get_tool_validation_help() -> String {
    #[allow(clippy::type_complexity)]
    let tools: &[(&str, &str, &str, &[(&str, &str, &str)])] = &[
        ("yt-dlp", "--version", "2023.0.0", &[("ffmpeg", "-version", "")]),
        ("fabric", "--version", "1.0.0", &[]),
        ("markitdown", "--version", "", &[]),
    ];

    // Collect all tool statuses first to compute column widths
    struct ToolEntry {
        icon: String,
        name: String,
        version: String,
        prefix: String, // text before the icon (e.g. "  " or "  └── ")
    }
    let mut entries: Vec<ToolEntry> = Vec::new();
    for (tool, version_arg, min_version, deps) in tools {
        let status = check_tool_version(tool, version_arg, min_version);
        entries.push(ToolEntry {
            icon: status.status_icon,
            name: tool.to_string(),
            version: status.version,
            prefix: "  ".to_string(),
        });
        for (i, (dep, dep_ver_arg, dep_min_ver)) in deps.iter().enumerate() {
            let dep_status = check_tool_version(dep, dep_ver_arg, dep_min_ver);
            let connector = if i == deps.len() - 1 { "└──" } else { "├──" };
            entries.push(ToolEntry {
                icon: dep_status.status_icon,
                name: dep.to_string(),
                version: dep_status.version,
                prefix: format!("  {connector} "),
            });
        }
    }

    // Compute the display width from prefix start through end of name for each entry
    // prefix + icon(2 display cols) + " " + name
    let max_left_len = entries
        .iter()
        .map(|e| e.prefix.chars().count() + 2 + 1 + e.name.len())
        .max()
        .unwrap_or(0);
    let max_ver_len = entries.iter().map(|e| e.version.len()).max().unwrap_or(0);

    let mut help = String::from("REQUIRED TOOLS:\n");
    for entry in &entries {
        let left_len = entry.prefix.chars().count() + 2 + 1 + entry.name.len();
        let padding = max_left_len - left_len;
        help.push_str(&format!(
            "{}{} {}{}  {:>width$}\n",
            entry.prefix,
            entry.icon,
            entry.name,
            " ".repeat(padding),
            entry.version,
            width = max_ver_len,
        ));
    }

    help.push_str("\nLogs are written to: ~/.local/share/borg/logs/borg.log");
    help
}

struct ToolStatus {
    version: String,
    status_icon: String,
}

fn check_tool_version(tool: &str, version_arg: &str, min_version: &str) -> ToolStatus {
    match ProcessCommand::new(tool).arg(version_arg).output() {
        Ok(output) if output.status.success() => {
            let version_output = String::from_utf8_lossy(&output.stdout);
            let version = extract_version(&version_output);

            let meets_requirement = if min_version.is_empty() {
                true // No version requirement, just check existence
            } else {
                version_compare(&version, min_version)
            };

            ToolStatus {
                version: if version.is_empty() || version == "unknown" {
                    "installed".to_string()
                } else {
                    version
                },
                status_icon: if meets_requirement { "✅" } else { "⚠️" }.to_string(),
            }
        }
        _ => ToolStatus {
            version: "not found".to_string(),
            status_icon: "❌".to_string(),
        },
    }
}

fn extract_version(output: &str) -> String {
    // Try to find a version-like pattern (digits.digits...) in the first line
    if let Some(line) = output.lines().next() {
        for word in line.split_whitespace() {
            let trimmed = word.trim_start_matches('v');
            if trimmed.contains('.') && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return trimmed.to_string();
            }
        }
        // If the whole line is a version (like yt-dlp outputs)
        let trimmed = line.trim();
        if trimmed.contains('.') && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return trimmed.to_string();
        }
    }
    "unknown".to_string()
}

fn version_compare(version: &str, min_version: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').map(|part| part.parse().unwrap_or(0)).collect() };

    let v1 = parse(version);
    let v2 = parse(min_version);

    for (a, b) in v1.iter().zip(v2.iter()) {
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }

    v1.len() >= v2.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_no_subcommand_defaults_to_none() {
        let cli = Cli::try_parse_from(["borg"]).expect("parse");
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_daemon_install() {
        let cli = Cli::try_parse_from(["borg", "daemon", "--install"]).expect("parse");
        match cli.command {
            Some(Command::Daemon(opts)) => assert!(opts.install),
            _ => panic!("expected Daemon"),
        }
    }

    #[test]
    fn test_daemon_start() {
        let cli = Cli::try_parse_from(["borg", "daemon", "--start"]).expect("parse");
        match cli.command {
            Some(Command::Daemon(opts)) => assert!(opts.start),
            _ => panic!("expected Daemon"),
        }
    }

    #[test]
    fn test_daemon_status() {
        let cli = Cli::try_parse_from(["borg", "daemon", "--status"]).expect("parse");
        match cli.command {
            Some(Command::Daemon(opts)) => assert!(opts.status),
            _ => panic!("expected Daemon"),
        }
    }

    #[test]
    fn test_daemon_reinstall() {
        let cli = Cli::try_parse_from(["borg", "daemon", "--reinstall"]).expect("parse");
        match cli.command {
            Some(Command::Daemon(opts)) => assert!(opts.reinstall),
            _ => panic!("expected Daemon"),
        }
    }

    #[test]
    fn test_ingest_subcommand() {
        let cli = Cli::try_parse_from(["borg", "ingest", "https://example.com"]).expect("parse");
        match cli.command {
            Some(Command::Ingest {
                url,
                clipboard,
                file,
                tags,
                force,
            }) => {
                assert_eq!(url, Some("https://example.com".to_string()));
                assert!(!clipboard);
                assert!(file.is_none());
                assert!(tags.is_none());
                assert!(!force);
            }
            _ => panic!("expected Ingest"),
        }
    }

    #[test]
    fn test_ingest_with_file() {
        let cli = Cli::try_parse_from(["borg", "ingest", "--file", "/tmp/photo.png"]).expect("parse");
        match cli.command {
            Some(Command::Ingest { url, file, .. }) => {
                assert!(url.is_none());
                assert_eq!(file, Some(PathBuf::from("/tmp/photo.png")));
            }
            _ => panic!("expected Ingest"),
        }
    }

    #[test]
    fn test_ingest_with_file_and_tags() {
        let cli = Cli::try_parse_from([
            "borg",
            "ingest",
            "--file",
            "/tmp/diagram.jpg",
            "-t",
            "diagram,whiteboard",
        ])
        .expect("parse");
        match cli.command {
            Some(Command::Ingest { file, tags, .. }) => {
                assert_eq!(file, Some(PathBuf::from("/tmp/diagram.jpg")));
                assert_eq!(tags, Some(vec!["diagram".to_string(), "whiteboard".to_string()]));
            }
            _ => panic!("expected Ingest"),
        }
    }

    #[test]
    fn test_ingest_with_tags() {
        let cli = Cli::try_parse_from(["borg", "ingest", "https://example.com", "-t", "ai,rust"]).expect("parse");
        match cli.command {
            Some(Command::Ingest { url, tags, .. }) => {
                assert_eq!(url, Some("https://example.com".to_string()));
                assert_eq!(tags, Some(vec!["ai".to_string(), "rust".to_string()]));
            }
            _ => panic!("expected Ingest"),
        }
    }

    #[test]
    fn test_hotkey_install() {
        let cli = Cli::try_parse_from(["borg", "hotkey", "--install"]).expect("parse");
        match cli.command {
            Some(Command::Hotkey(opts)) => {
                assert!(opts.install);
                assert!(!opts.uninstall);
                assert_eq!(opts.host, "localhost");
                assert_eq!(opts.port, 8181);
                assert_eq!(opts.key, "<Ctrl><Shift>b");
            }
            _ => panic!("expected Hotkey"),
        }
    }

    #[test]
    fn test_hotkey_uninstall() {
        let cli = Cli::try_parse_from(["borg", "hotkey", "--uninstall"]).expect("parse");
        match cli.command {
            Some(Command::Hotkey(opts)) => assert!(opts.uninstall),
            _ => panic!("expected Hotkey"),
        }
    }

    #[test]
    fn test_hotkey_custom_host_and_port() {
        let cli = Cli::try_parse_from(["borg", "hotkey", "--install", "--host", "desk.lan", "--port", "9090"])
            .expect("parse");
        match cli.command {
            Some(Command::Hotkey(opts)) => {
                assert!(opts.install);
                assert_eq!(opts.host, "desk.lan");
                assert_eq!(opts.port, 9090);
            }
            _ => panic!("expected Hotkey"),
        }
    }

    #[test]
    fn test_hotkey_custom_key() {
        let cli = Cli::try_parse_from(["borg", "hotkey", "--install", "--key", "<Super>b"]).expect("parse");
        match cli.command {
            Some(Command::Hotkey(opts)) => {
                assert_eq!(opts.key, "<Super>b");
            }
            _ => panic!("expected Hotkey"),
        }
    }

    #[test]
    fn test_note_subcommand() {
        let cli = Cli::try_parse_from(["borg", "note", "Met James at the Rust meetup"]).expect("parse");
        match cli.command {
            Some(Command::Note { text, clipboard, tags }) => {
                assert_eq!(text, Some("Met James at the Rust meetup".to_string()));
                assert!(!clipboard);
                assert!(tags.is_none());
            }
            _ => panic!("expected Note"),
        }
    }

    #[test]
    fn test_note_with_tags() {
        let cli = Cli::try_parse_from(["borg", "note", "define: garrulous", "-t", "vocab,english"]).expect("parse");
        match cli.command {
            Some(Command::Note { text, tags, .. }) => {
                assert_eq!(text, Some("define: garrulous".to_string()));
                assert_eq!(tags, Some(vec!["vocab".to_string(), "english".to_string()]));
            }
            _ => panic!("expected Note"),
        }
    }

    #[test]
    fn test_note_clipboard() {
        let cli = Cli::try_parse_from(["borg", "note", "--clipboard"]).expect("parse");
        match cli.command {
            Some(Command::Note { text, clipboard, .. }) => {
                assert!(text.is_none());
                assert!(clipboard);
            }
            _ => panic!("expected Note"),
        }
    }

    #[test]
    fn test_global_options_with_subcommand() {
        let cli = Cli::try_parse_from(["borg", "-v", "-l", "debug", "daemon", "--start"]).expect("parse");
        assert!(cli.verbose);
        assert_eq!(cli.log_level, Some("debug".to_string()));
        match cli.command {
            Some(Command::Daemon(opts)) => assert!(opts.start),
            _ => panic!("expected Daemon"),
        }
    }

    #[test]
    fn test_reingest_all() {
        let cli = Cli::try_parse_from(["borg", "reingest", "--all"]).expect("parse");
        match cli.command {
            Some(Command::Reingest { all, dry_run, .. }) => {
                assert!(all);
                assert!(!dry_run);
            }
            _ => panic!("expected Reingest"),
        }
    }

    #[test]
    fn test_reingest_dry_run_with_type() {
        let cli = Cli::try_parse_from(["borg", "reingest", "--all", "--type", "youtube", "--dry-run"]).expect("parse");
        match cli.command {
            Some(Command::Reingest {
                all, r#type, dry_run, ..
            }) => {
                assert!(all);
                assert_eq!(r#type, Some("youtube".to_string()));
                assert!(dry_run);
            }
            _ => panic!("expected Reingest"),
        }
    }

    #[test]
    fn test_reingest_source() {
        let cli = Cli::try_parse_from(["borg", "reingest", "--source", "https://example.com"]).expect("parse");
        match cli.command {
            Some(Command::Reingest { source, all, .. }) => {
                assert!(!all);
                assert_eq!(source, Some("https://example.com".to_string()));
            }
            _ => panic!("expected Reingest"),
        }
    }

    #[test]
    fn test_reingest_domain_and_date() {
        let cli = Cli::try_parse_from(["borg", "reingest", "--all", "--domain", "ai", "--after", "2026-03-01"])
            .expect("parse");
        match cli.command {
            Some(Command::Reingest { all, domain, after, .. }) => {
                assert!(all);
                assert_eq!(domain, Some("ai".to_string()));
                assert_eq!(after, Some("2026-03-01".to_string()));
            }
            _ => panic!("expected Reingest"),
        }
    }
}
