use std::path::PathBuf;

use clap::{Args, Subcommand};
use eyre::Result;

use borg::config::Config;
use borg::extension::{self, install};

#[derive(Args)]
pub struct ExtensionCli {
    #[command(subcommand)]
    pub command: ExtensionCommand,
}

#[derive(Subcommand)]
pub enum ExtensionCommand {
    /// Regenerate manifest.json (and ingest-schema.json, when wired) from code.
    Generate,
    /// Regenerate and fail if committed files differ. Drift gate for otto ci.
    Validate,
    /// Generate + sign via AMO. Produces a versioned .xpi.
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
    /// Print the extension version (= workspace version).
    Version,
}

pub fn run(cli: ExtensionCli, config: Config) -> Result<()> {
    let repo_root = extension::repo_root()?;
    match cli.command {
        ExtensionCommand::Generate => {
            let result = extension::generate(&repo_root, &config)?;
            let verb = if result.manifest_changed { "regenerated" } else { "unchanged" };
            println!("manifest {}: {}", verb, result.manifest_path.display());
            Ok(())
        }
        ExtensionCommand::Validate => {
            let result = extension::validate(&repo_root, &config)?;
            if let Some(drift) = result.manifest_drift {
                eprintln!("{drift}");
                eprintln!("Run `sb borg extension generate` and commit the result, then re-run `otto ci`.");
                std::process::exit(2);
            }
            println!("manifest current: no drift");
            Ok(())
        }
        ExtensionCommand::Sign => {
            let result = extension::sign::run(&repo_root, &config)?;
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
            let result = install::run(&repo_root, &config, opts)?;
            if result.skipped_not_installed {
                println!(
                    "extension not installed on this machine; skipping (use without --if-installed to bootstrap)."
                );
            } else {
                if let Some(xpi) = &result.xpi_path {
                    println!("signed: {}", xpi.display());
                }
                if let Some(policy) = &result.policy_path {
                    let verb = if result.policy_changed { "updated" } else { "current" };
                    println!("policy {}: {}", verb, policy.display());
                }
                println!(
                    "extension installed; Firefox will pick up the change automatically per `file://` install_url semantics."
                );
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
        ExtensionCommand::Version => {
            println!("{}", extension::current_version());
            Ok(())
        }
    }
}
