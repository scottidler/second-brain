use eyre::{Context, Result};
use std::path::Path;

mod migrate;

#[derive(clap::Args)]
pub struct BootstrapArgs {
    /// Skip the fastembed model prefetch (network-light bootstrap on install machines without GPU/disk budget).
    #[arg(long)]
    pub skip_prefetch_model: bool,

    /// Skip registering systemd units. Useful on machines that already have
    /// per-machine drop-ins in place that bootstrap shouldn't overwrite.
    #[arg(long)]
    pub skip_systemd: bool,

    /// Migrate legacy config layout (~/.config/{borg,cortex,obsidian-cortex,oracle,second-brain})
    /// into the unified ~/.config/sb/ layout. Safe to run repeatedly; copies legacy files only when
    /// the new location is empty, refuses on byte differences, never deletes the legacy directory.
    #[arg(long)]
    pub migrate: bool,
}

const BORG_TEMPLATE: &str = include_str!("../../../config/templates/borg.yml.example");
const CORTEX_TEMPLATE: &str = include_str!("../../../config/templates/cortex.yml.example");
const ORACLE_TEMPLATE: &str = include_str!("../../../config/templates/oracle.yml.example");

pub async fn run(args: BootstrapArgs) -> Result<()> {
    // Auto-migrate on first invocation that detects a legacy directory, unless
    // --migrate was passed explicitly (which forces it regardless of detection).
    if args.migrate || migrate::legacy_detected() {
        println!("Detected legacy config layout - migrating into ~/.config/sb/...");
        let report = migrate::migrate_legacy_layout().context("migrate legacy config layout")?;
        for line in &report.lines {
            println!("{line}");
        }
        if report.had_conflicts {
            eyre::bail!(
                "migration refused due to byte differences with existing ~/.config/sb/ files - \
                 resolve manually then rerun"
            );
        }
        println!();
    }

    let targets = [
        ("borg", vault::paths::borg_config(), BORG_TEMPLATE),
        ("cortex", vault::paths::cortex_config(), CORTEX_TEMPLATE),
        ("oracle", vault::paths::oracle_config(), ORACLE_TEMPLATE),
    ];
    for (name, path, template) in &targets {
        write_if_missing(name, path, template)?;
    }

    if !args.skip_systemd {
        println!();
        println!("Installing systemd units...");
        register_systemd_units().await?;
    }

    if !args.skip_prefetch_model {
        println!();
        println!("Prefetching embedding model (this can take ~1-2 minutes on first run)...");
        prefetch_embedding_model()?;
    }

    println!();
    println!("\u{2705} Bootstrap complete. Run `sb status` to see live state.");
    Ok(())
}

fn write_if_missing(name: &str, path: &Path, template: &str) -> Result<()> {
    if path.exists() {
        println!("\u{2139}\u{fe0f}  {name}: already present at {}", path.display());
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("config path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create parent dir for {name}: {}", parent.display()))?;
    std::fs::write(path, template).with_context(|| format!("write {name} template to {}", path.display()))?;
    println!("\u{2705} {name}: wrote template -> {}", path.display());
    Ok(())
}

async fn register_systemd_units() -> Result<()> {
    let borg_config = borg::config::load_config(None).context("load borg config for daemon install")?;
    let borg_install = borg::opts::DaemonOpts {
        install: true,
        uninstall: false,
        reinstall: false,
        start: false,
        stop: false,
        restart: false,
        status: false,
    };
    let borg_outcome = borg::daemon(borg_config, false, borg_install)
        .await
        .context("borg daemon --install")?;
    match &borg_outcome {
        borg::DaemonOutcome::Installed { unit_path } | borg::DaemonOutcome::Reinstalled { unit_path } => {
            println!("Wrote {}", unit_path.display());
            println!("Service installed and started.");
        }
        _ => println!("borg daemon: unexpected outcome from --install ({borg_outcome:?})"),
    }

    let cortex_config = cortex::config::Config::load(None).context("load cortex config for daemon install")?;
    let cwd = std::env::current_dir().context("get CWD")?;
    let cortex_vault = cortex_config.vault_root(Some(&cwd));
    let cortex_install = cortex::opts::DaemonOpts {
        install: true,
        uninstall: false,
        start: false,
        stop: false,
        status: false,
    };
    let outcome = cortex::daemon::run(&cortex_vault, &cortex_config, &cortex_install)
        .await
        .context("cortex daemon --install")?;
    for line in &outcome.lines {
        println!("{line}");
    }

    Ok(())
}

fn prefetch_embedding_model() -> Result<()> {
    // Warm the embedding cache via cortex's dedicated prefetch entry. Returns
    // the resolved model name; bootstrap prints it so the operator sees
    // exactly which model was fetched.
    let resolved = cortex::embed::prefetch(None).context("prefetch embedding model")?;
    println!("Prefetched embedding model {resolved}.");
    Ok(())
}
