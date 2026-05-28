use eyre::{Context, Result};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;
use vault::paths::CliConfig;

use crate::cli::{Cli, Cmd};

/// Init the right logger for the parsed subcommand.
///
/// All logs land under `~/.local/share/sb/<name>.log`:
/// - Borg verbs   -> sb/borg.log
/// - Cortex verbs -> sb/cortex.log
/// - Oracle verbs -> sb/oracle.log (tracing-subscriber for `oracle serve` so MCP stdio stays clean; env_logger for the rest)
/// - status / doctor / bootstrap -> sb/{status,doctor,bootstrap}.log
///
/// Inspection verbs (`sb status`, `sb doctor`, `sb borg daemon --status`,
/// `sb borg log`, etc.) default to stderr-only. To opt one of them in to
/// file logging, add it to `~/.config/sb/cli.yml`:
///
/// ```yaml
/// logging:
///   status: true
///   borg-daemon-status: true
/// ```
pub fn init_for(cli: &Cli) -> Result<()> {
    let cli_cfg = CliConfig::load();
    let (name, level) = name_and_level(cli, &cli_cfg);
    let path = log_path(&name);

    // The `oracle serve` MCP server uses tracing, not log; route it to the
    // tracing-subscriber file writer. Everything else goes through env_logger.
    if matches!(&cli.cmd, Cmd::Oracle(c) if matches!(c.command, crate::cli::oracle::Commands::Serve)) {
        return init_tracing_to_file(&path, &level);
    }
    if routes_to_stderr_only(cli, &cli_cfg) {
        return vault::logging::setup_logging_stderr(&level);
    }
    vault::logging::setup_logging(&path, &level)
}

/// True when this invocation should log to stderr only, not to the
/// `<subsystem>.log` file. Inspection verbs are stderr-only by default;
/// `~/.config/sb/cli.yml::logging.<path>.<verb>: true` flips that verb
/// back into file logging. Work verbs (daemon --start, ingest, sweep,
/// etc.) always write to the file regardless of `cli.yml`.
fn routes_to_stderr_only(cli: &Cli, cli_cfg: &CliConfig) -> bool {
    match verb_logs_to_file(cli, cli_cfg) {
        VerbLogging::AlwaysFile => false,
        VerbLogging::Inspection { opted_in } => !opted_in,
    }
}

enum VerbLogging {
    AlwaysFile,
    Inspection { opted_in: bool },
}

/// Maps the parsed CLI invocation to a path through the nested
/// `cli.yml::logging` tree, then asks the config whether that path is
/// opted in. Returns `AlwaysFile` for work verbs (daemon --start, ingest,
/// classify, sweep, embed, etc.) that always write to the subsystem log
/// regardless of `cli.yml`.
fn verb_logs_to_file(cli: &Cli, cli_cfg: &CliConfig) -> VerbLogging {
    use crate::cli::{borg, cortex, oracle};
    let inspect = |path: &[&str]| VerbLogging::Inspection {
        opted_in: cli_cfg.logging.opted_in(path),
    };
    let daemon_flag = |d: &borg::DaemonArgs| -> &'static str {
        if d.install {
            "install"
        } else if d.uninstall {
            "uninstall"
        } else if d.reinstall {
            "reinstall"
        } else if d.stop {
            "stop"
        } else if d.restart {
            "restart"
        } else {
            "status"
        }
    };
    let cortex_daemon_flag = |d: &cortex::DaemonArgs| -> &'static str {
        if d.install {
            "install"
        } else if d.uninstall {
            "uninstall"
        } else if d.stop {
            "stop"
        } else {
            "status"
        }
    };
    match &cli.cmd {
        Cmd::Status(_) => inspect(&["status"]),
        Cmd::Doctor(_) => inspect(&["doctor"]),
        Cmd::Bootstrap(_) => inspect(&["bootstrap"]),
        Cmd::Borg(c) => match c.command.as_ref() {
            None => VerbLogging::AlwaysFile,
            Some(borg::Command::Daemon(d)) if d.start => VerbLogging::AlwaysFile,
            Some(borg::Command::Daemon(d)) => inspect(&["borg", "daemon", daemon_flag(d)]),
            Some(borg::Command::Log(_)) => inspect(&["borg", "log"]),
            Some(borg::Command::Dlq(_)) => inspect(&["borg", "dlq"]),
            Some(borg::Command::Intake(_)) => inspect(&["borg", "intake"]),
            Some(borg::Command::Blocklist(_)) => inspect(&["borg", "blocklist"]),
            Some(borg::Command::Retention(_)) => inspect(&["borg", "retention"]),
            Some(borg::Command::Dashboard(_)) => inspect(&["borg", "dashboard"]),
            Some(borg::Command::Extension(_)) => inspect(&["borg", "extension"]),
            Some(borg::Command::Hotkey(_)) => inspect(&["borg", "hotkey"]),
            _ => VerbLogging::AlwaysFile,
        },
        Cmd::Cortex(c) => match &c.command {
            cortex::Command::Daemon(d) if d.start => VerbLogging::AlwaysFile,
            cortex::Command::Daemon(d) => inspect(&["cortex", "daemon", cortex_daemon_flag(d)]),
            _ => VerbLogging::AlwaysFile,
        },
        Cmd::Oracle(c) => match c.command {
            oracle::Commands::Stats => inspect(&["oracle", "stats"]),
            oracle::Commands::Call { .. } => inspect(&["oracle", "call"]),
            _ => VerbLogging::AlwaysFile,
        },
        Cmd::Glean(_) => VerbLogging::AlwaysFile,
    }
}

fn name_and_level(cli: &Cli, cli_cfg: &CliConfig) -> (String, String) {
    let cli_yaml_level = cli_cfg.logging.level.as_deref();
    match &cli.cmd {
        Cmd::Borg(c) => (
            "borg".into(),
            resolve_level(
                cli.log_level.as_deref(),
                c.log_level.as_deref(),
                cli_yaml_level,
                cli.verbose,
            ),
        ),
        Cmd::Cortex(c) => (
            "cortex".into(),
            resolve_level(
                cli.log_level.as_deref(),
                c.log_level.as_deref(),
                cli_yaml_level,
                cli.verbose,
            ),
        ),
        Cmd::Oracle(_) => (
            "oracle".into(),
            resolve_level(cli.log_level.as_deref(), None, cli_yaml_level, cli.verbose),
        ),
        Cmd::Status(_) => (
            "status".into(),
            resolve_level(cli.log_level.as_deref(), None, cli_yaml_level, cli.verbose),
        ),
        Cmd::Doctor(_) => (
            "doctor".into(),
            resolve_level(cli.log_level.as_deref(), None, cli_yaml_level, cli.verbose),
        ),
        Cmd::Bootstrap(_) => (
            "bootstrap".into(),
            resolve_level(cli.log_level.as_deref(), None, cli_yaml_level, cli.verbose),
        ),
        Cmd::Glean(_) => (
            "glean".into(),
            resolve_level(cli.log_level.as_deref(), None, cli_yaml_level, cli.verbose),
        ),
    }
}

/// Path of the log file for the given subsystem/command name.
/// All logs land under `~/.local/share/sb/<name>.log`. Used by both
/// the logger initializer and after_help text builders that want to
/// show the path in `--help` output.
pub fn log_path(name: &str) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sb")
        .join(format!("{name}.log"))
}

/// Resolution order: CLI flag (root or sub) > `--verbose` > `cli.yml::logging.level` > "info".
fn resolve_level(root: Option<&str>, sub: Option<&str>, cli_yaml: Option<&str>, verbose: bool) -> String {
    if let Some(v) = root.or(sub) {
        return v.to_string();
    }
    if verbose {
        return "debug".into();
    }
    cli_yaml.map(str::to_string).unwrap_or_else(|| "info".into())
}

fn init_tracing_to_file(log_path: &std::path::Path, level: &str) -> Result<()> {
    let filter = EnvFilter::new(level);

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create log directory")?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .context("Failed to open log file")?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();

    Ok(())
}
