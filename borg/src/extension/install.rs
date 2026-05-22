use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use eyre::{Context, Result};
use serde_json::{Value, json};

use crate::config::Config;
use crate::extension::{self, sign};

const EXTENSION_ID: &str = "obsidian-borg@scottidler";
const LATEST_XPI_NAME: &str = "obsidian-borg-latest.xpi";

#[derive(Debug, Default)]
pub struct InstallOpts {
    pub no_policy: bool,
    pub policy_file: Option<PathBuf>,
    pub if_installed: bool,
}

#[derive(Debug, Default)]
pub struct InstallResult {
    pub xpi_path: Option<PathBuf>,
    pub policy_path: Option<PathBuf>,
    pub policy_changed: bool,
    pub skipped_not_installed: bool,
}

#[derive(Debug, Default)]
pub struct UninstallOpts {
    pub purge: bool,
}

#[derive(Debug, Default)]
pub struct UninstallResult {
    pub policy_path: Option<PathBuf>,
    pub artifacts_removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirefoxInstall {
    Tarball(PathBuf),
    AptOrDeb,
    Snap,
    Flatpak,
    Unknown,
}

pub fn detect_firefox() -> Result<FirefoxInstall> {
    log::debug!("extension::install::detect_firefox");
    let Some(resolved) = which_firefox()? else {
        return Ok(FirefoxInstall::Unknown);
    };
    let resolved_str = resolved.to_string_lossy();
    log::debug!("extension::install::detect_firefox: resolved={resolved_str}");
    if resolved_str.starts_with("/opt/firefox/") {
        return Ok(FirefoxInstall::Tarball(PathBuf::from("/opt/firefox")));
    }
    if resolved_str.starts_with("/snap/") {
        return Ok(FirefoxInstall::Snap);
    }
    if resolved_str.contains("/flatpak/") || resolved_str.contains("org.mozilla.firefox") {
        return Ok(FirefoxInstall::Flatpak);
    }
    if resolved_str.starts_with("/usr/bin/") || resolved_str.starts_with("/usr/lib/") {
        return Ok(FirefoxInstall::AptOrDeb);
    }
    Ok(FirefoxInstall::Unknown)
}

fn which_firefox() -> Result<Option<PathBuf>> {
    let which_out = Command::new("which")
        .arg("firefox")
        .output()
        .context("failed to run `which firefox`")?;
    if !which_out.status.success() {
        return Ok(None);
    }
    let path = String::from_utf8_lossy(&which_out.stdout).trim().to_string();
    if path.is_empty() {
        return Ok(None);
    }
    let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(&path));
    Ok(Some(canonical))
}

pub fn policy_path(install: &FirefoxInstall) -> Result<PathBuf> {
    match install {
        FirefoxInstall::Tarball(root) => Ok(root.join("distribution").join("policies.json")),
        FirefoxInstall::AptOrDeb => Ok(PathBuf::from("/etc/firefox/policies/policies.json")),
        FirefoxInstall::Snap => eyre::bail!(
            "snap-installed Firefox cannot load `file://` install_url - use the Mozilla tarball or apt Firefox, \
             or pass --policy-file to override"
        ),
        FirefoxInstall::Flatpak => {
            let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("cannot resolve $HOME"))?;
            Ok(home.join(".var/app/org.mozilla.firefox/.mozilla/firefox/policies/policies.json"))
        }
        FirefoxInstall::Unknown => eyre::bail!(
            "could not detect Firefox install type; supported: tarball, apt/deb, flatpak. \
             Pass --policy-file to override."
        ),
    }
}

fn requires_sudo(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/etc/") || s.starts_with("/opt/")
}

fn install_url_for(repo_root: &Path) -> String {
    let xpi = extension::extension_dir(repo_root)
        .join("web-ext-artifacts")
        .join(LATEST_XPI_NAME);
    format!("file://{}", xpi.display())
}

fn build_policy_entry(install_url: &str) -> Value {
    json!({
        "installation_mode": "force_installed",
        "install_url": install_url,
        "updates_disabled": false,
        "default_area": "navbar"
    })
}

pub fn merge_policy(existing: Value, install_url: &str) -> Value {
    let mut root = match existing {
        Value::Object(_) => existing,
        _ => json!({}),
    };
    let root_obj = root.as_object_mut().expect("root is object");
    let policies = root_obj.entry("policies".to_string()).or_insert_with(|| json!({}));
    if !policies.is_object() {
        *policies = json!({});
    }
    let policies_obj = policies.as_object_mut().expect("policies is object");
    let ext_settings = policies_obj
        .entry("ExtensionSettings".to_string())
        .or_insert_with(|| json!({}));
    if !ext_settings.is_object() {
        *ext_settings = json!({});
    }
    let ext_settings_obj = ext_settings.as_object_mut().expect("ExtensionSettings is object");
    ext_settings_obj.insert(EXTENSION_ID.to_string(), build_policy_entry(install_url));
    root
}

pub fn strip_policy(existing: Value) -> Value {
    let Value::Object(mut root_obj) = existing else {
        return existing;
    };
    if let Some(policies) = root_obj.get_mut("policies").and_then(|v| v.as_object_mut())
        && let Some(ext_settings) = policies.get_mut("ExtensionSettings").and_then(|v| v.as_object_mut())
    {
        ext_settings.remove(EXTENSION_ID);
        if ext_settings.is_empty() {
            policies.remove("ExtensionSettings");
        }
    }
    Value::Object(root_obj)
}

fn read_policy_file(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => {
            serde_json::from_str(&content).with_context(|| format!("parse {}", path.display()))
        }
        _ => Ok(json!({})),
    }
}

fn policy_contains_ours(path: &Path) -> bool {
    let Ok(existing) = read_policy_file(path) else {
        return false;
    };
    existing
        .get("policies")
        .and_then(|p| p.get("ExtensionSettings"))
        .and_then(|e| e.get(EXTENSION_ID))
        .is_some()
}

fn write_policy_file(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("policy path has no parent: {}", path.display()))?;
    if requires_sudo(path) {
        log::info!("writing {} via sudo tee (atomic rename)", path.display());
        // Stage to a sibling tmp file then atomic-rename, both via sudo.
        let tmp = parent.join(format!(".policies.json.{}.tmp", std::process::id()));
        let mut child = Command::new("sudo")
            .arg("tee")
            .arg(&tmp)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("spawn `sudo tee` for policy write")?;
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().ok_or_else(|| eyre::eyre!("sudo tee stdin"))?;
            stdin.write_all(content.as_bytes())?;
        }
        let status = child.wait().context("wait on sudo tee")?;
        if !status.success() {
            eyre::bail!("sudo tee {} failed with exit {status}", tmp.display());
        }
        let mv_status = Command::new("sudo")
            .arg("mv")
            .arg(&tmp)
            .arg(path)
            .status()
            .context("spawn `sudo mv` for policy rename")?;
        if !mv_status.success() {
            eyre::bail!("sudo mv {} -> {} failed", tmp.display(), path.display());
        }
    } else {
        if !parent.exists() {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
        let tmp = parent.join(format!(".policies.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    }
    Ok(())
}

fn atomic_symlink_swap(versioned_xpi: &Path, latest_link: &Path) -> Result<()> {
    log::debug!(
        "extension::install: symlink {} -> {}",
        latest_link.display(),
        versioned_xpi.display()
    );
    let target_basename = versioned_xpi
        .file_name()
        .ok_or_else(|| eyre::eyre!("versioned xpi has no filename: {}", versioned_xpi.display()))?;
    let parent = latest_link
        .parent()
        .ok_or_else(|| eyre::eyre!("latest link has no parent: {}", latest_link.display()))?;
    let tmp = parent.join(format!(".{LATEST_XPI_NAME}.{}.tmp", std::process::id()));
    if tmp.exists() || tmp.symlink_metadata().is_ok() {
        std::fs::remove_file(&tmp).ok();
    }
    symlink(target_basename, &tmp).with_context(|| format!("symlink {}", tmp.display()))?;
    std::fs::rename(&tmp, latest_link)
        .with_context(|| format!("atomic-rename {} -> {}", tmp.display(), latest_link.display()))?;
    Ok(())
}

pub fn run(repo_root: &Path, config: &Config, opts: InstallOpts) -> Result<InstallResult> {
    log::debug!(
        "extension::install::run: no_policy={} if_installed={} policy_file={:?}",
        opts.no_policy,
        opts.if_installed,
        opts.policy_file
    );

    let policy_target = if let Some(override_path) = opts.policy_file.clone() {
        Some(override_path)
    } else if opts.no_policy && opts.if_installed {
        None
    } else {
        // We need the path either to write to it OR to check it (if_installed).
        match detect_firefox()? {
            ff @ FirefoxInstall::Unknown if opts.if_installed => {
                log::debug!("extension::install: --if-installed and no Firefox detected -> skip");
                let _ = ff;
                return Ok(InstallResult {
                    skipped_not_installed: true,
                    ..InstallResult::default()
                });
            }
            ff => Some(policy_path(&ff)?),
        }
    };

    if opts.if_installed {
        let Some(target) = policy_target.as_ref() else {
            return Ok(InstallResult {
                skipped_not_installed: true,
                ..InstallResult::default()
            });
        };
        if !policy_contains_ours(target) {
            log::debug!(
                "extension::install: --if-installed and {} has no obsidian-borg entry -> skip",
                target.display()
            );
            return Ok(InstallResult {
                skipped_not_installed: true,
                ..InstallResult::default()
            });
        }
    }

    let sign_result = sign::run(repo_root, config).context("sign extension")?;
    let dir = extension::extension_dir(repo_root);
    let artifacts_dir = dir.join("web-ext-artifacts");
    let latest_link = artifacts_dir.join(LATEST_XPI_NAME);
    atomic_symlink_swap(&sign_result.xpi_path, &latest_link)?;

    if opts.no_policy {
        return Ok(InstallResult {
            xpi_path: Some(sign_result.xpi_path),
            policy_path: None,
            policy_changed: false,
            skipped_not_installed: false,
        });
    }

    if !latest_link.exists() {
        eyre::bail!(
            "symlink target missing after sign: {}; refusing to write a policy that points at nothing",
            latest_link.display()
        );
    }

    let target = policy_target.expect("policy_target must be set when no_policy=false");
    let install_url = install_url_for(repo_root);
    let existing = read_policy_file(&target).unwrap_or_else(|_| json!({}));
    let merged = merge_policy(existing.clone(), &install_url);
    let merged_text = serde_json::to_string_pretty(&merged).context("serialize merged policy")? + "\n";
    let existing_text = serde_json::to_string_pretty(&existing).unwrap_or_default() + "\n";
    let policy_changed = merged_text != existing_text;
    if policy_changed {
        write_policy_file(&target, &merged_text)?;
    }

    Ok(InstallResult {
        xpi_path: Some(sign_result.xpi_path),
        policy_path: Some(target),
        policy_changed,
        skipped_not_installed: false,
    })
}

pub fn uninstall(opts: UninstallOpts) -> Result<UninstallResult> {
    log::debug!("extension::install::uninstall: purge={}", opts.purge);
    let detected = detect_firefox().ok();
    let policy_target = detected.as_ref().and_then(|i| policy_path(i).ok());

    let mut policy_path_out = None;
    if let Some(target) = policy_target.as_ref()
        && target.exists()
        && policy_contains_ours(target)
    {
        let existing = read_policy_file(target)?;
        let stripped = strip_policy(existing);
        let stripped_text = serde_json::to_string_pretty(&stripped).context("serialize stripped policy")? + "\n";
        write_policy_file(target, &stripped_text)?;
        policy_path_out = Some(target.clone());
    }

    let mut artifacts_removed = false;
    if opts.purge {
        let repo_root = extension::repo_root()?;
        let artifacts_dir = extension::extension_dir(&repo_root).join("web-ext-artifacts");
        if artifacts_dir.exists() {
            std::fs::remove_dir_all(&artifacts_dir).with_context(|| format!("remove {}", artifacts_dir.display()))?;
            artifacts_removed = true;
        }
    }

    Ok(UninstallResult {
        policy_path: policy_path_out,
        artifacts_removed,
    })
}

#[cfg(test)]
mod tests;
