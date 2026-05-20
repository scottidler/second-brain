use eyre::{Context, Result};
use std::path::Path;

#[derive(clap::Args)]
pub struct BootstrapArgs {
    /// Skip the fastembed model prefetch (network-light bootstrap on install machines without GPU/disk budget).
    #[arg(long)]
    pub skip_prefetch_model: bool,

    /// Skip registering systemd units. Useful on machines that already have
    /// per-machine drop-ins in place that bootstrap shouldn't overwrite.
    #[arg(long)]
    pub skip_systemd: bool,
}

const BORG_TEMPLATE: &str = include_str!("../../../config/templates/borg.yml.example");
const CORTEX_TEMPLATE: &str = include_str!("../../../config/templates/cortex.yml.example");
const ORACLE_TEMPLATE: &str = include_str!("../../../config/templates/oracle.yml.example");

pub async fn run(args: BootstrapArgs) -> Result<()> {
    let config_root = dirs::config_dir().ok_or_else(|| eyre::eyre!("dirs::config_dir() returned None"))?;

    let targets = [
        ("borg", config_root.join("borg").join("borg.yml"), BORG_TEMPLATE),
        (
            "cortex",
            config_root.join("obsidian-cortex").join("obsidian-cortex.yml"),
            CORTEX_TEMPLATE,
        ),
        ("oracle", config_root.join("oracle").join("oracle.yml"), ORACLE_TEMPLATE),
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
    borg::daemon(borg_config, false, borg_install)
        .await
        .context("borg daemon --install")?;

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
    // Reuse cortex's embed pipeline with prefetch_model = true to warm the embedding
    // cache. Vault root doesn't matter for prefetch; we pass CWD.
    let cwd = std::env::current_dir().context("get CWD")?;
    let config = cortex::config::Config::load(None).context("load cortex config (defaults are fine)")?;
    let vault_root = config.vault_root(Some(&cwd));
    let opts = cortex::opts::EmbedOpts {
        backfill: false,
        kind: None,
        model: None,
        batch_size: cortex::embed::DEFAULT_BATCH_SIZE,
        prefetch_model: true,
        use_mock: false,
    };
    cortex::embed::run(&vault_root, &config, &opts).map(|_| ())
}
