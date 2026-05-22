use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
use std::os::unix::fs::symlink;
#[cfg(target_os = "linux")]
use std::process::Stdio;

use eyre::{Context, Result};
use serde_json::{Value, json};

use crate::config::Config;
#[cfg(target_os = "linux")]
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
    // Ask the snap daemon directly. Snap-installed Firefox surfaces
    // /usr/bin/firefox as a shell-script wrapper (not a symlink), so
    // path-based canonicalize cannot follow it into /snap/. `snap list
    // firefox` is the only reliable signal.
    if is_snap_firefox() {
        log::debug!("extension::install::detect_firefox: snap detected via `snap list firefox`");
        return Ok(FirefoxInstall::Snap);
    }
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

fn is_snap_firefox() -> bool {
    let Ok(output) = Command::new("snap").arg("list").arg("firefox").output() else {
        return false;
    };
    output.status.success() && !output.stdout.is_empty()
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
            "snap-installed Firefox cannot load a system policies.json (it runs inside snap confinement). \
             Use `install_strategy()` instead, which targets the snap profile's extensions/ directory."
        ),
        FirefoxInstall::Flatpak => {
            let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("cannot resolve $HOME"))?;
            Ok(home.join(".var/app/org.mozilla.firefox/.mozilla/firefox/policies/policies.json"))
        }
        FirefoxInstall::Unknown => eyre::bail!(
            "could not detect Firefox install type; supported: tarball, apt/deb, snap, flatpak. \
             Pass --policy-file to override."
        ),
    }
}

/// How to deliver the signed .xpi to a given Firefox install. System Firefox
/// (apt/deb, tarball, flatpak) reads enterprise policies; snap Firefox is
/// sandboxed and cannot, so we copy the .xpi straight into the user's snap
/// profile extensions directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStrategy {
    /// Write a `policies.json` with a `force_installed` entry pointing at the
    /// .xpi. Firefox picks the extension up from the `file://` URL on next
    /// launch. Used for apt/deb, tarball, and flatpak Firefox.
    PolicyFile { path: PathBuf },
    /// Copy the .xpi directly into the snap profile's extensions/ directory
    /// at `<profile>/extensions/<EXTENSION_ID>.xpi`. Snap Firefox reads
    /// extensions from its confined profile; the system /etc/firefox/policies
    /// file is invisible inside the sandbox.
    ProfileExtension { xpi_path: PathBuf },
}

pub fn install_strategy(install: &FirefoxInstall) -> Result<InstallStrategy> {
    match install {
        FirefoxInstall::Tarball(_) | FirefoxInstall::AptOrDeb | FirefoxInstall::Flatpak => {
            Ok(InstallStrategy::PolicyFile {
                path: policy_path(install)?,
            })
        }
        FirefoxInstall::Snap => {
            let profile = snap_active_profile_dir()?;
            Ok(InstallStrategy::ProfileExtension {
                xpi_path: profile.join("extensions").join(format!("{EXTENSION_ID}.xpi")),
            })
        }
        FirefoxInstall::Unknown => eyre::bail!(
            "could not detect Firefox install type; supported: tarball, apt/deb, snap, flatpak. \
             Pass --policy-file to override."
        ),
    }
}

/// Locate the active snap-Firefox profile directory by parsing
/// `~/snap/firefox/common/.mozilla/firefox/profiles.ini`. Snap Firefox
/// confines its profile under `~/snap/firefox/common/.mozilla/firefox/`,
/// distinct from the unconfined `~/.mozilla/firefox/` used by apt/deb
/// Firefox.
fn snap_active_profile_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("cannot resolve $HOME"))?;
    let snap_firefox = home.join("snap/firefox/common/.mozilla/firefox");
    let profiles_ini = snap_firefox.join("profiles.ini");
    if !profiles_ini.exists() {
        eyre::bail!(
            "snap Firefox profiles.ini not found at {} - launch Firefox at least once before installing the extension",
            profiles_ini.display()
        );
    }
    let contents =
        std::fs::read_to_string(&profiles_ini).with_context(|| format!("read {}", profiles_ini.display()))?;
    let profile_path = parse_default_profile_path(&contents)
        .ok_or_else(|| eyre::eyre!("no profile found in {}", profiles_ini.display()))?;
    Ok(snap_firefox.join(profile_path))
}

/// Parse a Firefox `profiles.ini` and return the active profile's path.
/// Prefers a `Profile` section with `Default=1`; falls back to the first
/// `Profile` section if none is marked default.
pub fn parse_default_profile_path(contents: &str) -> Option<String> {
    let mut current_section: Option<String> = None;
    let mut current_is_default = false;
    let mut current_path: Option<String> = None;
    let mut first_path: Option<String> = None;
    let mut default_path: Option<String> = None;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if let Some(rest) = line.strip_prefix('[')
            && let Some(name) = rest.strip_suffix(']')
        {
            // Flush previous section before opening a new one.
            if current_is_default && default_path.is_none() {
                default_path = current_path.clone();
            }
            current_section = Some(name.to_string());
            current_is_default = false;
            current_path = None;
            continue;
        }
        if current_section.as_deref().is_some_and(|s| s.starts_with("Profile"))
            && let Some((key, value)) = line.split_once('=')
        {
            match key.trim() {
                "Default" => {
                    if value.trim() == "1" {
                        current_is_default = true;
                    }
                }
                "Path" => {
                    let v = value.trim().to_string();
                    if first_path.is_none() {
                        first_path = Some(v.clone());
                    }
                    current_path = Some(v);
                }
                _ => {}
            }
        }
    }
    // Flush the final section.
    if current_is_default && default_path.is_none() {
        default_path = current_path;
    }
    default_path.or(first_path)
}

#[cfg(target_os = "linux")]
fn requires_sudo(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/etc/") || s.starts_with("/opt/")
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn write_policy_file(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("policy path has no parent: {}", path.display()))?;
    if requires_sudo(path) {
        log::info!("writing {} via sudo tee (atomic rename)", path.display());
        // Ensure the parent directory exists. On a fresh apt/deb Firefox
        // install, /etc/firefox/policies/ may not exist yet; the non-sudo
        // branch below already handles this via std::fs::create_dir_all,
        // and the sudo branch needs the same affordance via `sudo mkdir -p`.
        if !parent.exists() {
            let mkdir_status = Command::new("sudo")
                .arg("mkdir")
                .arg("-p")
                .arg(parent)
                .status()
                .context("spawn `sudo mkdir -p` for policy dir")?;
            if !mkdir_status.success() {
                eyre::bail!("sudo mkdir -p {} failed with exit {mkdir_status}", parent.display());
            }
        }
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
pub fn run(repo_root: &Path, config: &Config, opts: InstallOpts, version: &str) -> Result<InstallResult> {
    log::debug!(
        "extension::install::run: no_policy={} if_installed={} policy_file={:?}",
        opts.no_policy,
        opts.if_installed,
        opts.policy_file
    );

    // Resolve the install strategy. --policy-file overrides detection and
    // always uses the PolicyFile strategy (caller knows what they're doing).
    // --if-installed needs the strategy to check whether our extension is
    // already present, EVEN WHEN --no-policy is also set (the otto deploy
    // hook case): --no-policy means "don't WRITE the policy", not "skip the
    // installed-check."
    let strategy: Option<InstallStrategy> = if let Some(override_path) = opts.policy_file.clone() {
        Some(InstallStrategy::PolicyFile { path: override_path })
    } else {
        match detect_firefox()? {
            FirefoxInstall::Unknown => {
                if opts.if_installed {
                    log::debug!("extension::install: --if-installed and no Firefox detected -> skip");
                    return Ok(InstallResult {
                        skipped_not_installed: true,
                        ..InstallResult::default()
                    });
                }
                if opts.no_policy {
                    None
                } else {
                    eyre::bail!(
                        "could not detect Firefox install type; supported: tarball, apt/deb, snap, flatpak. \
                         Pass --policy-file to override."
                    );
                }
            }
            ff => Some(install_strategy(&ff)?),
        }
    };

    if opts.if_installed {
        let Some(strategy_ref) = strategy.as_ref() else {
            log::debug!("extension::install: --if-installed but no install strategy resolvable -> skip");
            return Ok(InstallResult {
                skipped_not_installed: true,
                ..InstallResult::default()
            });
        };
        if !is_already_installed(strategy_ref) {
            log::debug!(
                "extension::install: --if-installed and strategy {:?} has no obsidian-borg entry -> skip",
                strategy_ref
            );
            return Ok(InstallResult {
                skipped_not_installed: true,
                ..InstallResult::default()
            });
        }
    }

    let sign_result = sign::run(repo_root, config, version).context("sign extension")?;
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
            "symlink target missing after sign: {}; refusing to install something that points at nothing",
            latest_link.display()
        );
    }

    let strategy = strategy.expect("strategy must be set when no_policy=false");
    match strategy {
        InstallStrategy::PolicyFile { path: target } => {
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
        InstallStrategy::ProfileExtension { xpi_path } => {
            let parent = xpi_path
                .parent()
                .ok_or_else(|| eyre::eyre!("profile extension path has no parent: {}", xpi_path.display()))?;
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create profile extensions dir {}", parent.display()))?;
            // Copy fresh; std::fs::copy overwrites atomically on the same
            // filesystem. The signed .xpi from sign::run is the source.
            std::fs::copy(&sign_result.xpi_path, &xpi_path)
                .with_context(|| format!("copy {} -> {}", sign_result.xpi_path.display(), xpi_path.display()))?;
            log::info!(
                "extension::install: copied signed .xpi into snap profile at {}",
                xpi_path.display()
            );
            Ok(InstallResult {
                xpi_path: Some(sign_result.xpi_path),
                policy_path: Some(xpi_path),
                policy_changed: true,
                skipped_not_installed: false,
            })
        }
    }
}

/// Returns true if our extension is already installed according to the given
/// strategy. For PolicyFile, "installed" means the policies.json contains an
/// `ExtensionSettings.<EXTENSION_ID>` entry. For ProfileExtension, "installed"
/// means the .xpi exists in the profile.
#[cfg(target_os = "linux")]
fn is_already_installed(strategy: &InstallStrategy) -> bool {
    match strategy {
        InstallStrategy::PolicyFile { path } => policy_contains_ours(path),
        InstallStrategy::ProfileExtension { xpi_path } => xpi_path.exists(),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn run(_repo_root: &Path, _config: &Config, _opts: InstallOpts, _version: &str) -> Result<InstallResult> {
    eyre::bail!("install verb is Linux-only; macOS/Windows users use `sb borg extension sign` + manual .xpi install")
}

#[cfg(target_os = "linux")]
pub fn uninstall(opts: UninstallOpts) -> Result<UninstallResult> {
    log::debug!("extension::install::uninstall: purge={}", opts.purge);
    let strategy = detect_firefox().ok().and_then(|i| install_strategy(&i).ok());

    let mut policy_path_out = None;
    if let Some(strategy) = strategy {
        match strategy {
            InstallStrategy::PolicyFile { path: target } => {
                if target.exists() && policy_contains_ours(&target) {
                    let existing = read_policy_file(&target)?;
                    let stripped = strip_policy(existing);
                    let stripped_text =
                        serde_json::to_string_pretty(&stripped).context("serialize stripped policy")? + "\n";
                    write_policy_file(&target, &stripped_text)?;
                    policy_path_out = Some(target);
                }
            }
            InstallStrategy::ProfileExtension { xpi_path } => {
                if xpi_path.exists() {
                    std::fs::remove_file(&xpi_path)
                        .with_context(|| format!("remove profile extension {}", xpi_path.display()))?;
                    policy_path_out = Some(xpi_path);
                }
            }
        }
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

#[cfg(not(target_os = "linux"))]
pub fn uninstall(_opts: UninstallOpts) -> Result<UninstallResult> {
    eyre::bail!("uninstall verb is Linux-only; macOS/Windows users remove web-ext-artifacts/ manually")
}

#[cfg(test)]
mod tests;
