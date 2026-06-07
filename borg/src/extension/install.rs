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

/// Single source of truth for the snap-unsupported message. snap Firefox runs
/// in a sandbox that cannot read a system `policies.json`, so the capture
/// extension's POSTs to the local daemon silently fail. We detect snap and
/// fail loudly rather than mis-install. Shared by `policy_path`,
/// `install_strategy`, and `run` so the operator always sees the same remedy.
const SNAP_UNSUPPORTED: &str = "snap Firefox is unsupported - its sandbox cannot load the capture extension \
     (POSTs to the local daemon silently fail; see \
     docs/postmortems/2026-06-07-firefox-snap-breaks-borg-capture.md). \
     Install Mozilla's /opt Firefox and remove the snap:\n    \
     manifest -C ~/repos/scottidler/dotfiles/manifest.yml -s firefox-opt | bash\n\
     then re-run this command.";

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
    Ok(classify_firefox_path(&resolved_str))
}

/// Pure path-classification: map a canonicalized `which firefox` result to a
/// `FirefoxInstall`. Split out of `detect_firefox()` so the classification can
/// be tested deterministically (the rest of `detect_firefox` shells out to
/// `snap`/`which` and so depends on whatever Firefox the host runs). This is
/// the regression guard against the canonicalize-wrapper trap: a snap box whose
/// `/usr/bin/firefox` wrapper resolves to `/usr/bin/...` must NOT be silently
/// classified here as `AptOrDeb` - that path is reached only when the upstream
/// snap probe already returned false, so the `/snap/` arm below is the
/// belt-and-suspenders second signal.
pub fn classify_firefox_path(resolved: &str) -> FirefoxInstall {
    if resolved.starts_with("/opt/firefox/") {
        return FirefoxInstall::Tarball(PathBuf::from("/opt/firefox"));
    }
    if resolved.starts_with("/snap/") {
        return FirefoxInstall::Snap;
    }
    if resolved.contains("/flatpak/") || resolved.contains("org.mozilla.firefox") {
        return FirefoxInstall::Flatpak;
    }
    if resolved.starts_with("/usr/bin/") || resolved.starts_with("/usr/lib/") {
        return FirefoxInstall::AptOrDeb;
    }
    FirefoxInstall::Unknown
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
        FirefoxInstall::Snap => eyre::bail!(SNAP_UNSUPPORTED),
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
/// sandboxed and cannot, so snap is an unsupported terminal error (handled in
/// `run`/`install_strategy`), not a strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStrategy {
    /// Write a `policies.json` with a `force_installed` entry pointing at the
    /// .xpi. Firefox picks the extension up from the `file://` URL on next
    /// launch. Used for apt/deb, tarball, and flatpak Firefox.
    PolicyFile { path: PathBuf },
}

pub fn install_strategy(install: &FirefoxInstall) -> Result<InstallStrategy> {
    match install {
        FirefoxInstall::Tarball(_) | FirefoxInstall::AptOrDeb | FirefoxInstall::Flatpak => {
            Ok(InstallStrategy::PolicyFile {
                path: policy_path(install)?,
            })
        }
        FirefoxInstall::Snap => eyre::bail!(SNAP_UNSUPPORTED),
        FirefoxInstall::Unknown => eyre::bail!(
            "could not detect Firefox install type; supported: tarball, apt/deb, snap, flatpak. \
             Pass --policy-file to override."
        ),
    }
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

    // Snap detection runs FIRST - before --policy-file resolution. snap's
    // sandbox cannot read ANY system policies.json, so the --policy-file
    // override must not be able to force one onto it (that would silently
    // mis-install, the exact failure this whole change exists to kill).
    // Explicit install -> hard error; --if-installed (deploy hook) -> warn +
    // skip so `otto deploy` does not fail on a not-yet-migrated snap box.
    let detected = detect_firefox()?;
    if matches!(detected, FirefoxInstall::Snap) {
        return snap_run_outcome(opts.if_installed);
    }

    // Resolve the install strategy. --policy-file overrides detection and
    // always uses the PolicyFile strategy (caller knows what they're doing).
    // --if-installed needs the strategy to check whether our extension is
    // already present, EVEN WHEN --no-policy is also set (the otto deploy
    // hook case): --no-policy means "don't WRITE the policy", not "skip the
    // installed-check."
    let strategy: Option<InstallStrategy> = if let Some(override_path) = opts.policy_file.clone() {
        Some(InstallStrategy::PolicyFile { path: override_path })
    } else {
        match detected {
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

    if !latest_link.exists() {
        eyre::bail!(
            "symlink target missing after sign: {}; refusing to install something that points at nothing",
            latest_link.display()
        );
    }

    // Dispatch on strategy. `--no-policy` only applies to the PolicyFile
    // path (system policies.json). `None` strategy means no Firefox detected
    // AND --no-policy passed: a daemon-only host where we sign for archival
    // and stop there.
    match strategy {
        None => Ok(InstallResult {
            xpi_path: Some(sign_result.xpi_path),
            policy_path: None,
            policy_changed: false,
            skipped_not_installed: false,
        }),
        Some(InstallStrategy::PolicyFile { path: target }) => {
            if opts.no_policy {
                return Ok(InstallResult {
                    xpi_path: Some(sign_result.xpi_path),
                    policy_path: None,
                    policy_changed: false,
                    skipped_not_installed: false,
                });
            }
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
    }
}

/// What `run()` does when it detects snap Firefox, factored out so the
/// override-loophole guarantee is unit-testable without the host running snap:
/// explicit install (`if_installed == false`) -> hard error; deploy hook
/// (`if_installed == true`) -> warn + skip so `otto deploy` does not fail on a
/// not-yet-migrated snap box. `run()` calls this *before* resolving
/// `--policy-file`, so the override can never force a policy onto snap.
#[cfg(target_os = "linux")]
fn snap_run_outcome(if_installed: bool) -> Result<InstallResult> {
    if if_installed {
        log::warn!("extension::install: snap Firefox detected, skipping (--if-installed). {SNAP_UNSUPPORTED}");
        return Ok(InstallResult {
            skipped_not_installed: true,
            ..InstallResult::default()
        });
    }
    eyre::bail!(SNAP_UNSUPPORTED)
}

/// Returns true if our extension is already installed according to the given
/// strategy. For PolicyFile, "installed" means the policies.json contains an
/// `ExtensionSettings.<EXTENSION_ID>` entry.
#[cfg(target_os = "linux")]
fn is_already_installed(strategy: &InstallStrategy) -> bool {
    match strategy {
        InstallStrategy::PolicyFile { path } => policy_contains_ours(path),
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
    if let Some(InstallStrategy::PolicyFile { path: target }) = strategy
        && target.exists()
        && policy_contains_ours(&target)
    {
        let existing = read_policy_file(&target)?;
        let stripped = strip_policy(existing);
        let stripped_text = serde_json::to_string_pretty(&stripped).context("serialize stripped policy")? + "\n";
        write_policy_file(&target, &stripped_text)?;
        policy_path_out = Some(target);
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
