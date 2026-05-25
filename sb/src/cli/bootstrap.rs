use eyre::{Context, Result};
use std::path::Path;

mod migrate;

#[cfg(test)]
mod tests;

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

    /// Sign + install the Firefox capture extension after standard bootstrap.
    /// First run on a machine: requires sudo for the policy-file write.
    /// Subsequent runs are unattended; pair with `otto deploy` for auto-refresh.
    #[arg(long)]
    pub extension: bool,

    /// Refresh shared YAMLs (canonical-tags, tag-mapping, tag-proposals) and
    /// the patterns directory from the binary's embedded copies, overwriting
    /// any operator edits. Per-host templates (borg.yml, cortex.yml, oracle.yml)
    /// are still write-if-missing under --force - those hold per-host config.
    #[arg(long)]
    pub force: bool,
}

pub(crate) const BORG_TEMPLATE: &str = include_str!("../../../config/templates/borg.yml.example");
pub(crate) const CORTEX_TEMPLATE: &str = include_str!("../../../config/templates/cortex.yml.example");
pub(crate) const ORACLE_TEMPLATE: &str = include_str!("../../../config/templates/oracle.yml.example");

pub(crate) const CANONICAL_TAGS_YML: &str = include_str!("../../../config/canonical-tags.yml");
pub(crate) const TAG_MAPPING_YML: &str = include_str!("../../../config/tag-mapping.yml");
pub(crate) const TAG_PROPOSALS_YML: &str = include_str!("../../../config/tag-proposals.yml");

/// Custom fabric patterns shipped with sb. Embedded byte-for-byte; the
/// explicit list (rather than `include_dir!`) makes adding a pattern a
/// deliberate code change reviewable in a PR.
///
/// Public consumer's install verb writes these to
/// `vault::paths::patterns_dir()`; doctor's drift check reads them back.
pub(crate) const PATTERNS: &[(&str, &str)] = &[
    ("condense.md", include_str!("../../../borg/patterns/condense.md")),
    (
        "distill-article.md",
        include_str!("../../../borg/patterns/distill-article.md"),
    ),
    (
        "distill-image.md",
        include_str!("../../../borg/patterns/distill-image.md"),
    ),
    (
        "distill-repo.md",
        include_str!("../../../borg/patterns/distill-repo.md"),
    ),
    (
        "distill-thread.md",
        include_str!("../../../borg/patterns/distill-thread.md"),
    ),
    (
        "distill-video.md",
        include_str!("../../../borg/patterns/distill-video.md"),
    ),
    (
        "distill-video-chunk.md",
        include_str!("../../../borg/patterns/distill-video-chunk.md"),
    ),
    (
        "distill-video-reduce.md",
        include_str!("../../../borg/patterns/distill-video-reduce.md"),
    ),
    (
        "distill-voicenote.md",
        include_str!("../../../borg/patterns/distill-voicenote.md"),
    ),
    (
        "distill-voicenote-chunk.md",
        include_str!("../../../borg/patterns/distill-voicenote-chunk.md"),
    ),
    (
        "distill-voicenote-reduce.md",
        include_str!("../../../borg/patterns/distill-voicenote-reduce.md"),
    ),
    (
        "obsidian-classify.md",
        include_str!("../../../borg/patterns/obsidian-classify.md"),
    ),
    (
        "obsidian-note.md",
        include_str!("../../../borg/patterns/obsidian-note.md"),
    ),
    (
        "obsidian-youtube-slides.md",
        include_str!("../../../borg/patterns/obsidian-youtube-slides.md"),
    ),
];

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

    extract_canonical_assets(args.force)?;

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

    if args.extension {
        println!();
        println!("Installing Firefox capture extension (sudo required for first policy write)...");
        install_extension()?;
    }

    println!();
    println!("\u{2705} Bootstrap complete. Run `sb status` to see live state.");
    Ok(())
}

fn install_extension() -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let repo_root = borg::extension::repo_root().context("locate repo root for extension install")?;
    let config = borg::config::load_config(None).context("load borg config for extension install")?;
    let opts = borg::extension::install::InstallOpts::default();
    let result = borg::extension::install::run(&repo_root, &config, opts, version).context("extension install")?;
    if let Some(xpi) = &result.xpi_path {
        println!("signed: {}", xpi.display());
    }
    if let Some(policy) = &result.policy_path {
        let verb = if result.policy_changed { "updated" } else { "current" };
        println!("policy {}: {}", verb, policy.display());
    }
    println!(
        "Firefox capture extension installed; the daily-use loop is now `bump && otto deploy` \
         (auto-refresh via the otto deploy hook)."
    );
    Ok(())
}

/// Extract every embedded canonical asset to `~/.config/sb/`. Templates
/// (borg.yml, cortex.yml, oracle.yml) are always write-if-missing because
/// they hold per-host config. Shared YAMLs and patterns honor `force`:
/// write-if-missing by default, always-write under `--force`.
///
/// Pulled out of `run` so unit tests can exercise the extraction in
/// isolation against an `XDG_CONFIG_HOME` tempdir.
pub(crate) fn extract_canonical_assets(force: bool) -> Result<()> {
    let targets = [
        ("borg", vault::paths::borg_config(), BORG_TEMPLATE),
        ("cortex", vault::paths::cortex_config(), CORTEX_TEMPLATE),
        ("oracle", vault::paths::oracle_config(), ORACLE_TEMPLATE),
    ];
    for (name, path, template) in &targets {
        write_if_missing(name, path, template)?;
    }

    let shared = [
        ("canonical-tags", vault::paths::canonical_tags(), CANONICAL_TAGS_YML),
        ("tag-mapping", vault::paths::tag_mapping(), TAG_MAPPING_YML),
        ("tag-proposals", vault::paths::tag_proposals(), TAG_PROPOSALS_YML),
    ];
    for (name, path, contents) in &shared {
        if force {
            write_always(name, path, contents)?;
        } else {
            write_if_missing(name, path, contents)?;
        }
    }

    let patterns_dir = vault::paths::patterns_dir();
    std::fs::create_dir_all(&patterns_dir)
        .with_context(|| format!("create patterns dir: {}", patterns_dir.display()))?;
    for (filename, contents) in PATTERNS {
        let path = patterns_dir.join(filename);
        if force {
            write_always(filename, &path, contents)?;
        } else {
            write_if_missing(filename, &path, contents)?;
        }
    }

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

fn write_always(name: &str, path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("config path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create parent dir for {name}: {}", parent.display()))?;
    std::fs::write(path, contents).with_context(|| format!("write {name} to {}", path.display()))?;
    println!("\u{2705} {name}: refreshed from embedded copy -> {}", path.display());
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
    // The unified resolver is strict: a marker-less CWD returns Err. For
    // bootstrap's install path we tolerate that and fall back to the CWD
    // we already have - the install only writes the systemd unit; the
    // daemon itself will re-resolve via `--vault` (set in the unit) at start time.
    let cortex_vault = cortex_config.vault_root(Some(&cwd)).unwrap_or_else(|_| cwd.clone());
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
