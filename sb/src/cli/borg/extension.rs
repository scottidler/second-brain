use std::path::PathBuf;

use clap::{Args, Subcommand};
use eyre::{Context, Result};

use borg::config::Config;
use borg::extension::{self, install};

#[derive(Args)]
pub struct ExtensionCli {
    #[command(subcommand)]
    pub command: ExtensionCommand,
}

#[derive(Subcommand)]
pub enum ExtensionCommand {
    /// Sign via AMO. Stages the full extension into a tempdir and runs web-ext sign.
    Sign,
    /// Sign + atomic symlink swap + drop policies.json (sudo first run).
    Install {
        /// Skip writing policies.json (assumes it's already in place).
        #[arg(long)]
        no_policy: bool,
        /// Write policies.json to this path instead of the auto-detected
        /// Firefox install location (managed-environment escape hatch).
        #[arg(long, value_name = "PATH")]
        policy_file: Option<PathBuf>,
        /// Refresh only if the extension is already installed on this machine.
        /// No-op on daemon-only servers. Used by the otto deploy hook.
        #[arg(long)]
        if_installed: bool,
    },
    /// Remove the policies.json entry (and, with --purge, the signed artifacts).
    /// Does NOT uninstall from a running Firefox - restart Firefox to clear.
    Uninstall {
        #[arg(long)]
        purge: bool,
    },
    /// Materialise the full extension (manifest + schema + static assets +
    /// AMO sidecar) into <DIR> for dev loading via about:debugging.
    Stage {
        /// Target directory. Created if absent; overwrites existing files.
        #[arg(long, value_name = "DIR")]
        to: PathBuf,
    },
    /// Print the resolved extension manifest (or schema with --schema) as
    /// pretty JSON to stdout. Reflects the running binary's version and the
    /// loaded config; no filesystem writes.
    Show {
        /// Print ingest-schema.json instead of manifest.json.
        #[arg(long)]
        schema: bool,
    },
    /// Print the extension version (= sb's CARGO_PKG_VERSION).
    Version,
}

pub fn run(cli: ExtensionCli, config: Config) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let repo_root = extension::repo_root()?;
    match cli.command {
        ExtensionCommand::Sign => {
            let result = extension::sign::run(&repo_root, &config, version)?;
            println!(
                "Signing extension v{} in {}",
                result.version,
                result.extension_dir.display()
            );
            println!("Extension signed successfully: {}", result.xpi_path.display());
            Ok(())
        }
        ExtensionCommand::Install {
            no_policy,
            policy_file,
            if_installed,
        } => {
            let opts = install::InstallOpts {
                no_policy,
                policy_file,
                if_installed,
            };
            let result = install::run(&repo_root, &config, opts, version)?;
            if result.skipped_not_installed {
                println!(
                    "extension not installed on this machine; skipping (use without --if-installed to bootstrap)."
                );
            } else {
                if let Some(xpi) = &result.xpi_path {
                    println!("signed: {}", xpi.display());
                }
                if let Some(target) = &result.policy_path {
                    let is_profile_copy = target.extension().and_then(|s| s.to_str()) == Some("xpi");
                    let label = if is_profile_copy { "profile xpi" } else { "policy" };
                    let verb = if result.policy_changed { "updated" } else { "current" };
                    println!("{label} {verb}: {}", target.display());
                    if is_profile_copy {
                        println!(
                            "extension installed; restart Firefox to load the new .xpi (snap Firefox does not hot-reload extensions)."
                        );
                    } else {
                        println!(
                            "extension installed; Firefox will pick up the change on next launch via `file://` install_url semantics."
                        );
                    }
                }
            }
            Ok(())
        }
        ExtensionCommand::Uninstall { purge } => {
            let result = install::uninstall(install::UninstallOpts { purge })?;
            if let Some(policy) = result.policy_path {
                println!("removed policy entry from {}", policy.display());
            }
            if result.artifacts_removed {
                println!("removed web-ext-artifacts/");
            }
            println!("uninstall complete; restart Firefox to drop the extension from the running profile.");
            Ok(())
        }
        ExtensionCommand::Stage { to } => {
            std::fs::create_dir_all(&to).with_context(|| format!("create stage target {}", to.display()))?;
            let result = extension::stage(&to, version, &config)?;
            println!("staged extension v{version} into {}", result.target_dir.display());
            println!("Load in Firefox via about:debugging -> 'Load Temporary Add-on...' and pick manifest.json");
            Ok(())
        }
        ExtensionCommand::Show { schema } => {
            let body = if schema {
                serde_json::to_string_pretty(&extension::schema::build_schema()?)
                    .context("serialize schema for stdout")?
            } else {
                serde_json::to_string_pretty(&extension::manifest::build_manifest(version, &config))
                    .context("serialize manifest for stdout")?
            };
            println!("{body}");
            Ok(())
        }
        ExtensionCommand::Version => {
            println!("{version}");
            Ok(())
        }
    }
}
