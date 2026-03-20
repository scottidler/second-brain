use clap::Parser;
use eyre::{Context, Result};
use borg::cli::{Cli, Command};
use borg::config::Config;
use borg::logging;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config: Config =
        borg::config::load_config(cli.config.as_ref()).context("Failed to load configuration")?;

    let log_level = logging::resolve_log_level(cli.log_level.as_deref(), config.log_level.as_deref());
    logging::setup_logging(&log_level).context("Failed to setup logging")?;

    log::debug!("Resolved log level: {log_level}");
    log::debug!("Config: {:?}", config);

    if cli.verbose {
        println!("{}", colored::Colorize::yellow("Verbose mode enabled"));
    }

    match cli.command {
        None => {
            Cli::parse_from(["borg", "--help"]);
            Ok(())
        }
        Some(Command::Daemon(opts)) => borg::run_daemon(config, cli.verbose, opts).await,
        Some(Command::Ingest {
            url,
            clipboard,
            file,
            tags,
            force,
        }) => {
            if let Some(file_path) = file {
                borg::run_file_ingest(config, file_path, tags, force).await
            } else {
                let resolved_url = borg::resolve_ingest_url(url, clipboard)?;
                let method = if clipboard {
                    borg::types::IngestMethod::Clipboard
                } else {
                    borg::types::IngestMethod::Cli
                };
                borg::run_ingest(config, resolved_url, tags, force, clipboard, method).await
            }
        }
        Some(Command::Note { text, clipboard, tags }) => {
            let resolved_text = borg::resolve_note_text(text, clipboard)?;
            borg::run_note(config, resolved_text, tags).await
        }
        Some(Command::Hotkey(opts)) => borg::run_hotkey(opts, &config).await,
        Some(Command::Sign) => borg::run_sign(&config).await,
        Some(Command::Migrate { dry_run: _, apply }) => borg::migrate::run_migrate(&config, apply).await,
        Some(Command::Audit { fix }) => borg::audit::run_audit(&config, fix).await,
        Some(Command::Reingest {
            all,
            r#type,
            domain,
            source,
            before,
            after,
            dry_run,
        }) => borg::run_reingest(config, all, r#type, domain, source, before, after, dry_run).await,
    }
}
