//! Shared health checks consumed by `sb status` (informational rendering)
//! and `sb doctor` (severity-tagged findings).

use std::path::Path;
use std::process::Command;

use borg::config::{SignalConfig, TelegramConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    Info,
    Warn,
    Error,
}

impl Severity {
    pub fn icon(self) -> &'static str {
        match self {
            Severity::Ok => "\u{2705}",
            Severity::Info => "\u{1f4ac}",
            Severity::Warn => "\u{26a0}\u{fe0f} ",
            Severity::Error => "\u{274c}",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
    pub suggested_fix: Option<String>,
}

impl Finding {
    pub fn ok(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Ok,
            message: msg.into(),
            suggested_fix: None,
        }
    }
    pub fn info(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            message: msg.into(),
            suggested_fix: None,
        }
    }
    pub fn warn(msg: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            message: msg.into(),
            suggested_fix: Some(fix.into()),
        }
    }
    pub fn error(msg: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: msg.into(),
            suggested_fix: Some(fix.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: &'static str,
    pub findings: Vec<Finding>,
}

pub fn all_sections() -> Vec<Section> {
    vec![
        Section {
            name: "systemd",
            findings: systemd_findings(),
        },
        Section {
            name: "config",
            findings: config_findings(),
        },
        Section {
            name: "shared config",
            findings: shared_config_findings(),
        },
        Section {
            name: "patterns",
            findings: pattern_findings(),
        },
        Section {
            name: "external binaries",
            findings: external_binaries_findings(),
        },
        Section {
            name: "embedding cache",
            findings: embedding_findings(),
        },
        Section {
            name: "borg",
            findings: borg_findings(),
        },
        Section {
            name: "firefox",
            findings: firefox_findings(),
        },
        Section {
            name: "telegram",
            findings: telegram_findings(),
        },
        Section {
            name: "signal",
            findings: signal_findings(),
        },
        Section {
            name: "vault",
            findings: vault_findings(),
        },
        Section {
            name: "data dir",
            findings: data_dir_findings(),
        },
    ]
}

fn systemd_findings() -> Vec<Finding> {
    let mut findings = Vec::new();
    for unit in &["borg.service", "cortex.service"] {
        match systemctl_show(unit) {
            Ok(state) => {
                if state.active_state == "active" {
                    findings.push(Finding::ok(format!(
                        "{unit}: active (PID {pid}, RSS {rss})",
                        unit = unit,
                        pid = state.main_pid,
                        rss = human_bytes(state.memory_current),
                    )));
                } else if state.active_state == "inactive" {
                    findings.push(Finding::warn(
                        format!("{unit}: inactive"),
                        format!("systemctl --user start {unit}"),
                    ));
                } else {
                    findings.push(Finding::error(
                        format!("{unit}: {}", state.active_state),
                        format!("systemctl --user status {unit}"),
                    ));
                }
            }
            Err(e) => findings.push(Finding::error(
                format!("{unit}: query failed ({e})"),
                format!("systemctl --user status {unit}"),
            )),
        }
    }
    findings
}

fn config_findings() -> Vec<Finding> {
    // Parse-status check: try to deserialize each config file as the typed
    // subsystem Config struct so YAML syntax errors AND schema errors (wrong
    // field type, unknown required field, etc.) both surface before the
    // daemon hits them at startup. Using `serde_yaml::Value` here would
    // only catch syntax, missing the design's intent.
    let borg_path = vault::paths::borg_config();
    let cortex_path = vault::paths::cortex_config();
    let oracle_path = vault::paths::oracle_config();

    let mut findings = vec![
        parse_typed::<borg::config::Config>("borg", &borg_path),
        parse_typed::<cortex::config::Config>("cortex", &cortex_path),
        parse_typed::<oracle::Config>("oracle", &oracle_path),
    ];

    // Surface unset vault.root-path values per Phase 1b. A missing root_path is
    // valid but every subsequent operation must fall back to --vault or the
    // marker-gated CWD - flag it so the user knows.
    if let Ok(cfg) = borg::config::load_config::<borg::config::Config>(None)
        && cfg.vault.root_path.is_none()
    {
        findings.push(Finding::warn(
            format!("borg: vault.root-path not set in {}", borg_path.display()),
            format!("set `vault.root-path` in {}", borg_path.display()),
        ));
    }
    if let Ok(cfg) = cortex::config::Config::load(None)
        && cfg.vault.root_path.is_none()
    {
        findings.push(Finding::warn(
            format!("cortex: vault.root-path not set in {}", cortex_path.display()),
            format!("set `vault.root-path` in {}", cortex_path.display()),
        ));
    }
    if let Ok(cfg) = oracle::Config::load(None)
        && cfg.vault.root_path.is_none()
    {
        findings.push(Finding::warn(
            format!("oracle: vault.root-path not set in {}", oracle_path.display()),
            format!("set `vault.root-path` in {}", oracle_path.display()),
        ));
    }

    findings
}

fn parse_typed<T: serde::de::DeserializeOwned>(name: &str, path: &std::path::Path) -> Finding {
    if !path.exists() {
        return Finding::warn(
            format!("{name}: missing ({})", path.display()),
            "sb bootstrap".to_string(),
        );
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return Finding::error(
                format!("{name}: {} unreadable: {e}", path.display()),
                "check permissions on the file".to_string(),
            );
        }
    };
    match serde_yaml::from_str::<T>(&content) {
        Ok(_) => Finding::ok(format!("{name}: {} (parses as typed Config)", path.display())),
        Err(e) => Finding::error(
            format!("{name}: {} parse failed: {e}", path.display()),
            format!(
                "open {} and fix the YAML (or sb bootstrap to restore the template)",
                path.display()
            ),
        ),
    }
}

/// Drift detection for the three shared YAML files. Source of truth is
/// now the binary's `include_str!` constants in `cli::bootstrap`, so the
/// check works on any machine without a clone of the repo. Mismatch is
/// Info, not Warn: operators are encouraged to tune these files.
fn shared_config_findings() -> Vec<Finding> {
    let installed_dir = vault::paths::config_root();
    let mut findings = Vec::new();
    for (filename, expected) in &[
        ("canonical-tags.yml", crate::cli::bootstrap::CANONICAL_TAGS_YML),
        ("tag-mapping.yml", crate::cli::bootstrap::TAG_MAPPING_YML),
        ("tag-proposals.yml", crate::cli::bootstrap::TAG_PROPOSALS_YML),
    ] {
        let path = installed_dir.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(actual) if actual.as_str() == *expected => {
                findings.push(Finding::ok(format!("{filename}: matches binary")));
            }
            Ok(_) => {
                findings.push(Finding::info(format!(
                    "{filename}: differs from binary (operator edit or stale binary?)"
                )));
            }
            Err(_) => {
                findings.push(Finding::error(
                    format!("{filename}: missing at {}", path.display()),
                    "sb bootstrap",
                ));
            }
        }
    }
    findings
}

const FABRIC_INSTALL_HINT: &str = "go install github.com/danielmiessler/fabric/cmd/fabric@latest";
/// Wall-clock bound for the live fabric probe. A real `summarize` on a 4-byte
/// input returns in a couple of seconds; this is the network-hang ceiling.
const FABRIC_PROBE_TIMEOUT_SECS: u64 = 30;
/// The cortex daemon's classify sweep runs every 300 s
/// (`cortex.yml classify.interval-secs`), so a note that has sat in `inbox/`
/// past 48 h has been offered to the classifier hundreds of times already.
/// It is either stuck (a classifier error) or low-confidence: `mark_needs_review`
/// (`cortex/src/classify.rs:963`) deliberately leaves a no-signal or
/// low-confidence note unclassified rather than guessing, so silence past this
/// window means a human needs to look, not that the daemon will eventually
/// catch up. Doctor thresholds are consts, not config (no `doctor:` section).
const INBOX_STALE_SECS: u64 = 48 * 3600;
/// `data dir` section thresholds. Doctor has no config file (see the const
/// above); these are the same kind of hardcoded ceiling.
const DATA_DIR_LOGS_WARN_BYTES: u64 = 512 * 1024 * 1024;
const DATA_DIR_TOTAL_WARN_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Remedy for both places the pre-R1 oracle data dir surfaces: the `data dir`
/// section's Warn and the Error the `SearchIndex::open` guard produces.
const LEGACY_ORACLE_REMEDY: &str = "runbook R1: stop cortex, mv -T ~/.local/share/oracle ~/.local/share/sb/oracle";
const SIGNAL_RS_INSTALL_HINT: &str =
    "cargo install --git https://github.com/scottidler/signal-rs --bin signal-rs --tag v0.2.1";

/// External-binary detection. Surface absence with the exact install
/// command. Signal-rs check is gated on `config.signal.is_some()` so a
/// Telegram-only install isn't pestered.
fn external_binaries_findings() -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(fabric_cli_findings());
    findings.extend(fabric_max_tokens_findings());
    findings.extend(fabric_default_patterns_findings());
    findings.extend(fabric_live_probe_findings());
    if let Ok(config) = borg::config::load_config::<borg::config::Config>(None)
        && config.signal.is_some()
    {
        findings.extend(signal_rs_cli_findings());
    }
    findings
}

fn fabric_cli_findings() -> Vec<Finding> {
    match Command::new("fabric").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let label = if version.is_empty() { "(no version reported)" } else { version.as_str() };
            vec![Finding::ok(format!("fabric CLI installed: {label}"))]
        }
        _ => vec![Finding::error("fabric CLI not found on PATH", FABRIC_INSTALL_HINT)],
    }
}

/// Verifies the installed fabric supports `--maxTokens`.
///
/// Borg passes `--maxTokens` on every pattern call (`fabric.max-tokens` in
/// borg.yml) because upstream fabric pins Anthropic requests to a hardcoded
/// 4096, and thinking tokens are billed against that same budget, so
/// `distill-video` gets truncated mid-YAML while fabric still exits 0. The
/// flag exists only on a fabric carrying danielmiessler/Fabric#2207.
///
/// This check exists because the failure is otherwise SILENT-ish and easy to
/// reintroduce: mise pins `github:danielmiessler/fabric` and its shims dir
/// precedes `~/.local/bin` in the daemon's PATH, so a `mise install` re-sync
/// puts an unpatched fabric back in front. Without this, the next symptom
/// would be broken video notes, not a failed check.
fn fabric_max_tokens_findings() -> Vec<Finding> {
    let Ok(out) = Command::new("fabric").arg("--help").output() else {
        // The CLI-missing case is already covered by fabric_cli_findings.
        return Vec::new();
    };
    // --help writes to stdout on some builds and stderr on others; check both.
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if text.contains("--maxTokens") {
        return vec![Finding::ok(
            "fabric supports --maxTokens (output ceiling is raisable)".to_string(),
        )];
    }
    vec![Finding::error(
        "installed fabric has no --maxTokens flag; every Anthropic call is capped at 4096 \
         and large distillations will truncate mid-document while fabric exits 0",
        "install the patched build: cd ~/repos/scottidler/Fabric && \
         go build -ldflags=\"-s -w\" -o ~/.local/bin/fabric ./cmd/fabric \
         (and check `which fabric`: mise shims precede ~/.local/bin on PATH)",
    )]
}

/// Verifies Daniel Miessler's default fabric patterns are present.
/// Uses substring containment rather than full parsing so an upstream
/// `fabric -l` format change does not break this check.
fn fabric_default_patterns_findings() -> Vec<Finding> {
    let Ok(out) = Command::new("fabric").arg("-l").output() else {
        // The CLI-missing case is already covered by fabric_cli_findings.
        return Vec::new();
    };
    if !out.status.success() {
        return vec![Finding::warn(
            "fabric -l exited non-zero (CLI installed, pattern list unavailable)",
            "rerun `fabric -y --update-patterns` to (re)provision Daniel Miessler's default patterns",
        )];
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let required = ["extract_wisdom", "summarize", "create_tags"];
    let missing: Vec<&str> = required.iter().copied().filter(|name| !stdout.contains(name)).collect();
    if missing.is_empty() {
        vec![Finding::ok(
            "fabric default patterns present (extract_wisdom, summarize, create_tags)",
        )]
    } else {
        vec![Finding::error(
            format!("missing default fabric patterns: {}", missing.join(", ")),
            "fabric -y --update-patterns",
        )]
    }
}

/// Live end-to-end fabric probe: run the `summarize` pattern on a trivial input
/// through the exact `vault::fabric::run_pattern` path borg uses at publish
/// time, with `model=""` so fabric resolves its own configured `DEFAULT_MODEL`.
/// The install/pattern checks above are static (a present binary, a listed
/// pattern); only a live call catches the failure that silently degrades every
/// ingest - a retired/misconfigured model (`404 not_found_error`), a bad API
/// key, or no egress to the provider. `fabric -l` lists models the live API will
/// 404 on, so the static list cannot substitute for this. Warn (not error) on
/// failure: a transient network blip should not turn doctor red, and the
/// degraded-count check is the companion signal for actual impact.
fn fabric_live_probe_findings() -> Vec<Finding> {
    // Skip when the binary is absent - fabric_cli_findings already errored, and
    // a probe would just restate it.
    if !vault::fabric::is_available("fabric") {
        return Vec::new();
    }
    // Thread the configured credential var NAME (borg's `llm.api-key`, which the
    // borg Config mirrors into `fabric.api-key`) so the probe exercises the same
    // ANTHROPIC_API_KEY-on-the-child translation the live ingest path uses. Fall
    // back to the fabric-native name when config is unavailable.
    let api_key_env = borg::config::load_config::<borg::config::Config>(None)
        .map(|cfg| cfg.fabric.api_key)
        .unwrap_or_else(|_| "ANTHROPIC_API_KEY".to_string());
    match vault::fabric::run_pattern(
        "summarize",
        "ping",
        "fabric",
        &api_key_env,
        "",
        0,
        FABRIC_PROBE_TIMEOUT_SECS,
    ) {
        Ok(_) => vec![Finding::ok(
            "fabric live probe (summarize) succeeded against the configured model".to_string(),
        )],
        Err(e) => vec![Finding::warn(
            format!("fabric live probe failed: {e}"),
            "check DEFAULT_MODEL in ~/.config/fabric/.env against a live `fabric --listmodels` probe (the static list can name retired models), the API key, and provider egress"
                .to_string(),
        )],
    }
}

fn signal_rs_cli_findings() -> Vec<Finding> {
    match Command::new("signal-rs").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let label = if version.is_empty() { "(no version reported)" } else { version.as_str() };
            vec![Finding::ok(format!("signal-rs CLI installed: {label}"))]
        }
        _ => vec![Finding::error(
            "signal-rs CLI not found on PATH",
            SIGNAL_RS_INSTALL_HINT,
        )],
    }
}

/// Drift detection for the bundled fabric patterns (see `bootstrap::PATTERNS`).
/// Mismatch is Warn here
/// (not Info): patterns are version-sensitive LLM prompts, so drift from
/// binary usually means "the binary moved forward but the operator
/// didn't refresh." `sb bootstrap --force` is the explicit fix.
fn pattern_findings() -> Vec<Finding> {
    let installed_dir = vault::paths::patterns_dir();
    let mut findings = Vec::new();
    let mut drift = 0usize;
    let mut missing = 0usize;
    for (filename, expected) in crate::cli::bootstrap::PATTERNS {
        let path = installed_dir.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(actual) if actual.as_str() == *expected => {}
            Ok(_) => drift += 1,
            Err(_) => missing += 1,
        }
    }
    let total = crate::cli::bootstrap::PATTERNS.len();
    if missing > 0 {
        findings.push(Finding::error(
            format!("{missing} of {total} patterns missing"),
            "sb bootstrap",
        ));
    }
    if drift > 0 {
        findings.push(Finding::warn(
            format!("{drift} of {total} patterns drifted from binary"),
            "sb bootstrap --force",
        ));
    }
    if drift == 0 && missing == 0 {
        findings.push(Finding::ok(format!("{total} patterns match binary")));
    }

    // The cortex classify Tier-2 pattern is referenced BY NAME in cortex.yml
    // (`actions.classify.fabric-pattern`) and resolved at runtime. A name that
    // doesn't resolve to a file silently disables Tier-2 LLM classification
    // (this exact bug shipped: the old default `cortex_classify` matched no
    // file). Verify the configured name resolves to an installed pattern.
    if let Ok(cfg) = cortex::config::Config::load(None) {
        let name = &cfg.actions.classify.fabric_pattern;
        let resolved = installed_dir.join(name);
        let resolved_md = installed_dir.join(format!("{name}.md"));
        if resolved.exists() || resolved_md.exists() {
            findings.push(Finding::ok(format!("classify pattern '{name}' resolves")));
        } else {
            findings.push(Finding::error(
                format!("classify pattern '{name}' resolves to no file in {}", installed_dir.display()),
                "set actions.classify.fabric-pattern in cortex.yml to an installed pattern (e.g. obsidian-classify), or sb bootstrap",
            ));
        }
    }
    findings
}

fn embedding_findings() -> Vec<Finding> {
    // The active embedding backend (candle vs fastembed) is a compile-time
    // feature on the vault crate. Both keep their downloaded weights in
    // different XDG locations. Probe both: any populated cache means the
    // model is available for the next inference call.
    let mut findings = Vec::new();
    let Some(cache_root) = dirs::cache_dir() else {
        findings.push(Finding::error(
            "dirs::cache_dir() returned None",
            "check XDG_CACHE_HOME",
        ));
        return findings;
    };

    // candle backend: hf-hub stores at ~/.cache/huggingface/hub/models--<owner>--<name>/
    let hf_hub = cache_root.join("huggingface/hub");
    let candle_models = list_model_dirs(&hf_hub, "models--");

    // fastembed backend: ~/.cache/fastembed/
    let fastembed = cache_root.join("fastembed");
    let fastembed_present = fastembed.exists() && has_any_subdir(&fastembed);

    if !candle_models.is_empty() {
        findings.push(Finding::ok(format!(
            "candle cache: {} model(s) under {}",
            candle_models.len(),
            hf_hub.display()
        )));
    }
    if fastembed_present {
        findings.push(Finding::ok(format!("fastembed cache: {}", fastembed.display())));
    }
    if candle_models.is_empty() && !fastembed_present {
        findings.push(Finding::warn(
            format!(
                "no embedding model cache found (checked {}, {})",
                hf_hub.display(),
                fastembed.display()
            ),
            "sb bootstrap (or sb cortex embed --prefetch-model)".to_string(),
        ));
    }
    findings
}

fn list_model_dirs(root: &std::path::Path, prefix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(prefix) && e.path().is_dir() { Some(name) } else { None }
        })
        .collect()
}

fn has_any_subdir(root: &std::path::Path) -> bool {
    let Ok(mut entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.any(|e| e.is_ok_and(|entry| entry.path().is_dir()))
}

struct SystemdState {
    active_state: String,
    main_pid: String,
    memory_current: u64,
}

fn systemctl_show(unit: &str) -> Result<SystemdState, String> {
    let output = Command::new("systemctl")
        .args(["--user", "show", "-p", "ActiveState,MemoryCurrent,MainPID", unit])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut state = SystemdState {
        active_state: "unknown".into(),
        main_pid: "0".into(),
        memory_current: 0,
    };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ActiveState=") {
            state.active_state = v.to_string();
        } else if let Some(v) = line.strip_prefix("MainPID=") {
            state.main_pid = v.to_string();
        } else if let Some(v) = line.strip_prefix("MemoryCurrent=") {
            state.memory_current = v.parse().unwrap_or(0);
        }
    }
    Ok(state)
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

fn borg_findings() -> Vec<Finding> {
    // Config load is itself a health signal; receipts health comes from the
    // DB (the same computation behind GET /health/audit).
    if let Err(e) = borg::config::load_config::<borg::config::Config>(None) {
        return vec![Finding::error(
            format!("could not load borg config: {e}"),
            format!("ensure {} exists (sb bootstrap)", vault::paths::borg_config().display()),
        )];
    }

    let mut findings = Vec::new();
    // The actionable silent-drop signal: crashes the watchdog declared in the
    // last 24h. Lifetime counts are in the receipts_summary line below.
    match borg::triage::audit_health_stats() {
        Ok(h) => {
            if h.crashed_24h > 0 {
                findings.push(Finding::warn(
                    format!(
                        "{} input(s) crashed in the last 24h (watchdog declared them lost)",
                        h.crashed_24h
                    ),
                    "sb borg log --stage crashed --since 24h".to_string(),
                ));
            } else {
                findings.push(Finding::ok("no crashes in the last 24h".to_string()));
            }
            if h.failed_24h > 0 {
                findings.push(Finding::info(format!("{} failure(s) in the last 24h", h.failed_24h)));
            }
            // The silent-quality signal: notes that landed but via a distill
            // fallback (e.g. a fabric API error). They read as `succeeded`, so
            // they never show up in the failed/crashed counts above - this is
            // the one place doctor makes degraded ingestion loud.
            if h.degraded_24h > 0 {
                findings.push(Finding::warn(
                    format!(
                        "{} note(s) published degraded in the last 24h (distill fallback - impoverished body)",
                        h.degraded_24h
                    ),
                    "sb borg log --degraded --since 24h, then `sb borg replay <trace>` once the cause is fixed"
                        .to_string(),
                ));
            }
        }
        Err(e) => findings.push(Finding::warn(
            format!("borg receipts health query failed: {e}"),
            "sb borg log --status failed (manual investigation)".to_string(),
        )),
    }
    // Harvest drift guard (harvest-completion Phase 6): distinguishes "harvest
    // has never run yet" (no warning - nothing installed/soaked) from "the
    // timer runs but a FUTURE clyde contract drift silently produced zero
    // session receipts for days" - the frozen CI fixtures can never catch a
    // drift that only shows up against the live catalog.
    match borg::triage::harvest_drift_stats() {
        Ok(d) if d.should_warn() => {
            findings.push(Finding::warn(
                format!(
                    "harvest timer has run before but produced zero session receipts in the last {}d - possible clyde contract drift",
                    borg::triage::HARVEST_DRIFT_WINDOW_DAYS
                ),
                "sb borg harvest --dry-run --since 60d to check the live contract; \
                 sb borg log --method harvest --since 7d for recent history"
                    .to_string(),
            ));
        }
        Ok(_) => {}
        Err(e) => findings.push(Finding::info(format!("harvest drift check failed: {e}"))),
    }
    // Receipts DB summary: open read-only, group by status, group failures
    // by stage. Reported as info (the file may not exist yet on a fresh
    // install, which is fine).
    match receipts_summary() {
        Ok(line) => findings.push(Finding::ok(line)),
        Err(e) => findings.push(Finding::info(format!("receipts: {e}"))),
    }
    findings
}

/// Firefox-install health for the capture extension. snap Firefox is the one
/// state we actively warn about: its sandbox cannot load the extension, so a
/// box still on snap silently captures nothing. Reuses borg's public
/// `detect_firefox()` (the enum-returning API) rather than the private
/// `is_snap_firefox()` probe - no new public surface, same detection we keep.
fn firefox_findings() -> Vec<Finding> {
    match borg::extension::install::detect_firefox() {
        Ok(install) => vec![firefox_finding(&install)],
        Err(e) => vec![Finding::info(format!("firefox detection skipped: {e}"))],
    }
}

/// Pure mapping from a detected install to a finding, so the snap-warns /
/// others-ok decision is unit-testable without the host actually running any
/// particular Firefox.
fn firefox_finding(install: &borg::extension::install::FirefoxInstall) -> Finding {
    use borg::extension::install::FirefoxInstall;
    match install {
        FirefoxInstall::Snap => Finding::warn(
            "snap Firefox present - its sandbox breaks the capture extension",
            "migrate to Mozilla /opt Firefox: manifest -C ~/repos/scottidler/dotfiles/manifest.yml -s firefox-opt",
        ),
        FirefoxInstall::Tarball(_) => Finding::ok("firefox: Mozilla /opt tarball (capture-capable)"),
        FirefoxInstall::AptOrDeb => Finding::ok("firefox: apt/deb (capture-capable)"),
        FirefoxInstall::Flatpak => Finding::ok("firefox: flatpak (capture-capable)"),
        FirefoxInstall::Unknown => Finding::info("firefox: not detected on this host"),
    }
}

/// One-line summary of the receipts DB for `sb status`.
fn receipts_summary() -> Result<String, String> {
    let path = vault::receipts::receipts_db_path().map_err(|e| e.to_string())?;
    if !path.exists() {
        return Err(format!("DB not yet created at {}", path.display()));
    }
    let conn = borg::receipts::open_at(&path).map_err(|e| e.to_string())?;
    let (received, succeeded, failed, rejected) = borg::receipts::count_by_status(&conn).map_err(|e| e.to_string())?;
    let by_stage = borg::receipts::count_failed_by_stage(&conn).map_err(|e| e.to_string())?;
    let stage_summary = if by_stage.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = by_stage.iter().map(|(s, n)| format!("{s}={n}")).collect();
        format!(" [{}]", parts.join(", "))
    };
    let rejected_summary = if rejected > 0 { format!(", {rejected} rejected") } else { String::new() };
    Ok(format!(
        "receipts: {received} received, {succeeded} succeeded, {failed} failed{rejected_summary}{stage_summary}"
    ))
}

fn vault_findings() -> Vec<Finding> {
    // The schema-doc drift check runs FIRST and unconditionally: it reads four
    // markdown files, not the oracle index, so it must survive every early
    // return below (the legacy-oracle-DB guard among them).
    let mut findings = schema_docs_findings();

    // Open the oracle SQLite index and pull two readings: total note count
    // (and schema gaps) and embedding coverage. Read-only.
    let config = match oracle::Config::load(None) {
        Ok(c) => c,
        Err(e) => {
            findings.push(Finding::error(
                format!("could not load oracle config: {e}"),
                format!(
                    "ensure {} exists (sb bootstrap)",
                    vault::paths::oracle_config().display()
                ),
            ));
            return findings;
        }
    };
    let db = match vault::search::SearchIndex::open(&config.db_path()) {
        Ok(db) => db,
        // The fail-closed legacy-oracle-DB guard is an Error, not a Warn:
        // `sb oracle index` would not fix it, the runbook move would, and
        // until it runs every opener (cortex daemon included) refuses.
        Err(e) if e.downcast_ref::<vault::search::SearchError>().is_some() => {
            findings.push(Finding::error(format!("{e}"), LEGACY_ORACLE_REMEDY.to_string()));
            return findings;
        }
        Err(e) => {
            findings.push(Finding::warn(
                format!("oracle SQLite index not openable: {e}"),
                "sb oracle index".to_string(),
            ));
            return findings;
        }
    };

    match db.stats() {
        Ok(stats) => {
            findings.push(Finding::ok(format!(
                "{} note(s) indexed across {} domain(s)",
                stats.total_notes,
                stats.by_domain.len()
            )));
        }
        Err(e) => findings.push(Finding::warn(
            format!("vault stats query failed: {e}"),
            "sb oracle stats".to_string(),
        )),
    }

    findings.extend(frontmatter_policy_findings());
    findings.push(inbox_stale_finding(&db));

    match db.embedding_coverage() {
        Ok(cov) => {
            if cov.total_notes == 0 {
                findings.push(Finding::info("no notes in index yet (sb oracle index)".to_string()));
            } else if cov.embedded_notes == 0 {
                findings.push(Finding::warn(
                    format!("embedding coverage: 0 / {} notes (0%)", cov.total_notes),
                    "sb cortex embed --backfill".to_string(),
                ));
            } else {
                let pct = cov.percent();
                let line = format!(
                    "embedding coverage: {} / {} notes ({:.1}%)",
                    cov.embedded_notes, cov.total_notes, pct
                );
                if pct < 50.0 {
                    findings.push(Finding::warn(line, "sb cortex embed --backfill".to_string()));
                } else {
                    findings.push(Finding::ok(line));
                }
            }
        }
        Err(e) => findings.push(Finding::warn(
            format!("embedding coverage query failed: {e}"),
            "sb cortex embed --backfill".to_string(),
        )),
    }

    findings
}

/// Doctor's frontmatter-policy signal, sourced from cortex's own lint
/// policy rather than a second copy of it. Enters through `cortex::lint`
/// (not `scan_vault` + `lint_frontmatter` directly) so the exclude/include
/// filter cortex's CLI applies (`lintable_notes` in `cortex::lib`) is the
/// same set doctor counts over - a direct `scan_vault` call would count the
/// larger ignore-only set instead.
/// Are the four generated `system/schemas/*-values.md` files still what the
/// binary renders? Same compare `sb cortex schema --check` runs, so doctor and
/// the verb can never disagree. Mirrors `shared_config_findings`.
fn schema_docs_findings() -> Vec<Finding> {
    let cortex_config = match cortex::config::Config::load(None) {
        Ok(c) => c,
        Err(e) => {
            return vec![Finding::warn(
                format!("could not load cortex config for the schema-doc check: {e}"),
                "sb bootstrap".to_string(),
            )];
        }
    };
    let vault_root = match cortex_config.vault_root(None) {
        Ok(p) => p,
        Err(e) => {
            return vec![Finding::warn(
                format!("could not resolve vault root for the schema-doc check: {e}"),
                "set `vault.root-path` in cortex.yml".to_string(),
            )];
        }
    };
    match cortex::schema_docs::render_all(&vault_root, false) {
        Ok(report) if report.drifted() => vec![Finding::warn(
            format!(
                "system/schemas/*-values.md drifted from binary ({})",
                report.drifted_paths().join(", ")
            ),
            "sb cortex schema --render".to_string(),
        )],
        Ok(_) => vec![Finding::ok("system/schemas/*-values.md: matches binary".to_string())],
        Err(e) => vec![Finding::warn(
            format!("schema-doc drift check failed: {e}"),
            "sb cortex schema --check".to_string(),
        )],
    }
}

fn frontmatter_policy_findings() -> Vec<Finding> {
    let cortex_config = match cortex::config::Config::load(None) {
        Ok(c) => c,
        Err(e) => {
            return vec![Finding::warn(
                format!("could not load cortex config: {e}"),
                "sb bootstrap".to_string(),
            )];
        }
    };
    let vault_root = match cortex_config.vault_root(None) {
        Ok(p) => p,
        Err(e) => {
            return vec![Finding::warn(
                format!("could not resolve vault root for cortex lint: {e}"),
                "set `vault.root-path` in cortex.yml".to_string(),
            )];
        }
    };
    let opts = cortex::opts::LintOpts {
        apply: false,
        format: cortex::opts::LintFormat::Human,
        rule: vec!["frontmatter".to_string()],
        path: None,
    };
    let report = match cortex::lint(&vault_root, &cortex_config, &opts) {
        Ok((report, _apply)) => report,
        Err(e) => {
            return vec![Finding::warn(
                format!("cortex lint (frontmatter) failed: {e}"),
                "sb cortex lint --rule frontmatter".to_string(),
            )];
        }
    };

    let mut findings = Vec::new();
    let required = report.count_by_rule_prefix("frontmatter.required.");
    let domain = required.get("domain").copied().unwrap_or(0);
    let origin = required.get("origin").copied().unwrap_or(0);
    let tags = required.get("tags").copied().unwrap_or(0);
    findings.push(Finding::info(format!(
        "frontmatter gaps (cortex lint policy): domain={domain}, origin={origin}, tags={tags}"
    )));

    let enum_violations: u64 = report.count_by_rule_prefix("frontmatter.enum.").values().sum();
    if enum_violations > 0 {
        findings.push(Finding::warn(
            format!("{enum_violations} frontmatter enum violations"),
            "sb cortex lint --rule frontmatter".to_string(),
        ));
    }

    findings
}

/// Warn when the oldest inbox note has sat unclassified past `INBOX_STALE_SECS`.
/// See `INBOX_STALE_SECS` for the rationale. `inbox_oldest` already excludes
/// `inbox/.claude/...` dotfiles (tooling, not unclassified content).
fn inbox_stale_finding(db: &vault::search::SearchIndex) -> Finding {
    match db.inbox_oldest() {
        Ok(Some((path, modified_at))) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let age_secs = (now - modified_at).max(0) as u64;
            if age_secs > INBOX_STALE_SECS {
                Finding::warn(
                    format!("oldest inbox note {path} is {}h old", age_secs / 3600),
                    "sb cortex classify, or assign a domain by hand".to_string(),
                )
            } else {
                Finding::ok(format!("oldest inbox note {path} is {}h old", age_secs / 3600))
            }
        }
        Ok(None) => Finding::ok("inbox is empty".to_string()),
        Err(e) => Finding::warn(format!("inbox_oldest query failed: {e}"), "sb oracle index".to_string()),
    }
}

/// Filenames tolerated directly inside the oracle data dir. Anything else
/// there (a hand-copied backup, a stray `.db-journal`, etc.) is dead weight
/// riding along in the same directory `SearchIndex::open` manages.
const ORACLE_DIR_ALLOWED_FILES: &[&str] = &["oracle.db", "oracle.db-wal", "oracle.db-shm", "eval-cache.db"];

/// Disk-footprint section for sb's own data directories. Info-only unless a
/// size crosses a threshold - this is a "where did my disk go" signal, not a
/// health check, so nothing here can fail `sb doctor`.
///
/// One exception to "info-only": a `Finding::warn` for a legacy
/// `~/.local/share/oracle/` directory left behind by an unfinished migration
/// (runbook R1).
fn data_dir_findings() -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut total_bytes: u64 = 0;

    let stages_dir = vault::paths::borg_stages_dir();
    let stages_bytes = vault::paths::dir_size(&stages_dir);
    total_bytes += stages_bytes;
    findings.push(Finding::info(format!(
        "stages: {} ({})",
        human_bytes(stages_bytes),
        stages_dir.display()
    )));

    let (receipts_bytes, receipts_path) = match vault::receipts::receipts_db_path() {
        Ok(path) => (vault::paths::dir_size(&path), Some(path)),
        Err(_) => (0, None),
    };
    total_bytes += receipts_bytes;
    findings.push(Finding::info(match &receipts_path {
        Some(path) => format!("receipts.db: {} ({})", human_bytes(receipts_bytes), path.display()),
        None => "receipts.db: could not resolve path (HOME/XDG_DATA_HOME unset)".to_string(),
    }));

    // Logs land flat under `xdg_data_dir()/sb/*.log*` (`.log`, `.log.1`, ...
    // from `FileRotate`), not nested per subsystem - see `sb/src/logger.rs`.
    let logs_dir = vault::paths::xdg_data_dir().map(|d| d.join("sb"));
    let logs_bytes = logs_dir
        .as_ref()
        .map(|dir| sum_matching_files(dir, |name| name.contains(".log")))
        .unwrap_or(0);
    total_bytes += logs_bytes;
    findings.push(Finding::info(format!("logs: {}", human_bytes(logs_bytes))));
    if logs_bytes > DATA_DIR_LOGS_WARN_BYTES {
        findings.push(Finding::warn(
            format!(
                "logs total {} exceeds {}",
                human_bytes(logs_bytes),
                human_bytes(DATA_DIR_LOGS_WARN_BYTES)
            ),
            "prune or archive old *.log.N files under ~/.local/share/sb/".to_string(),
        ));
    }

    // `oracle_db_path()`'s parent: `~/.local/share/sb/oracle/`.
    let oracle_dir = vault::paths::oracle_db_path().parent().map(Path::to_path_buf);
    let oracle_bytes = oracle_dir.as_ref().map(|dir| vault::paths::dir_size(dir)).unwrap_or(0);
    total_bytes += oracle_bytes;
    findings.push(Finding::info(format!("oracle: {}", human_bytes(oracle_bytes))));

    if let Some(dir) = &oracle_dir
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
            if is_file && !ORACLE_DIR_ALLOWED_FILES.contains(&name.as_ref()) {
                findings.push(Finding::warn(
                    format!("stray backup in oracle dir: {name}"),
                    format!("remove or relocate {}", entry.path().display()),
                ));
            }
        }
    }

    // The pre-R1 data dir. Present means the operator move has not run yet;
    // the guard in `SearchIndex::open` is what makes that state safe, this is
    // what makes it visible.
    let legacy_dir = vault::paths::legacy_oracle_dir();
    if legacy_dir.join("oracle.db").exists() {
        findings.push(Finding::warn(
            format!(
                "legacy oracle data dir present at {} ({}); see runbook R1",
                legacy_dir.display(),
                human_bytes(vault::paths::dir_size(&legacy_dir)),
            ),
            LEGACY_ORACLE_REMEDY.to_string(),
        ));
    }

    if total_bytes > DATA_DIR_TOTAL_WARN_BYTES {
        findings.push(Finding::warn(
            format!(
                "sb data dir total {} exceeds {}",
                human_bytes(total_bytes),
                human_bytes(DATA_DIR_TOTAL_WARN_BYTES)
            ),
            "review stages/receipts.db/logs/oracle sizes above".to_string(),
        ));
    }

    findings
}

/// Sum the sizes of top-level files (not recursing into subdirectories) whose
/// filename matches `pred`. Used for the `sb/*.log*` glob - rotated logs sit
/// flat alongside the live file, not nested under a `logs/` subdirectory.
fn sum_matching_files(dir: &Path, pred: impl Fn(&str) -> bool) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|entry| pred(&entry.file_name().to_string_lossy()))
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// Telegram doctor section. Was missing pre-Phase 5 despite Telegram being
/// borg's daily driver; shipped alongside the Signal section so both
/// transports report config + auth + host parity.
fn telegram_findings() -> Vec<Finding> {
    let config = match borg::config::load_config::<borg::config::Config>(None) {
        Ok(c) => c,
        Err(e) => {
            return vec![Finding::error(
                format!("could not load borg config: {e}"),
                "ensure ~/.config/sb/borg.yml exists (sb bootstrap)",
            )];
        }
    };
    let Some(tg) = config.telegram.as_ref() else {
        return vec![Finding::info(
            "telegram not configured (no `telegram:` block in borg.yml)",
        )];
    };
    telegram_findings_for(tg)
}

fn telegram_findings_for(tg: &TelegramConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    if let Some(host) = tg.host.as_deref()
        && !host.is_empty()
    {
        let current = current_hostname();
        if !current.eq_ignore_ascii_case(host) {
            findings.push(Finding::info(format!(
                "telegram.host={host:?}, hostname={current:?} (this machine does not run Telegram ingest)"
            )));
            return findings;
        }
    }
    if tg.bot_token.trim().is_empty() {
        findings.push(Finding::error(
            "telegram.bot-token is empty",
            "set telegram.bot-token in borg.yml (env var name or path containing the token)",
        ));
        return findings;
    }
    match borg::config::resolve_secret(&tg.bot_token) {
        Ok(token) if !token.is_empty() => {
            findings.push(Finding::ok(format!(
                "telegram.bot-token resolves ({} chars)",
                token.len()
            )));
            // Live get_me() probe, run on an isolated thread inside borg so
            // sb's doctor carries no teloxide dependency.
            match borg::probe_telegram(&token) {
                Ok(username) => findings.push(Finding::ok(format!("telegram.get_me() succeeded: @{username}"))),
                Err(e) => findings.push(Finding::error(
                    format!("telegram.get_me() failed: {e}"),
                    "verify the bot token, network egress to api.telegram.org, and that the bot is not deauthorized",
                )),
            }
        }
        Ok(_) => findings.push(Finding::error(
            format!("telegram.bot-token {:?} resolved to an empty string", tg.bot_token),
            "set the env var (or file) referenced by telegram.bot-token to a non-empty value",
        )),
        Err(e) => findings.push(Finding::error(
            format!("telegram.bot-token {:?} not resolvable: {e}", tg.bot_token),
            "set the env var (or file path) referenced by telegram.bot-token",
        )),
    }
    // Empty allowlist is fail-closed (deny-all) as of the 2026-06-09
    // remediation. Warn so the operator knows ingest will reject every chat
    // rather than silently accepting everyone (the old fail-open behavior).
    if tg.allowed_chat_ids.is_empty() {
        findings.push(Finding::warn(
            "telegram.allowed-chat-ids is empty: ALL chats are denied (fail-closed)",
            "add your chat id(s) to telegram.allowed-chat-ids in borg.yml to enable ingest",
        ));
    }
    findings
}

/// Signal doctor section. Host-gated like the supervisor (`is_local_host`):
/// when the configured host differs from this machine the section
/// short-circuits at the host comparison and skips the state_dir / link
/// checks - the laptop is not expected to have a linked state on disk.
fn signal_findings() -> Vec<Finding> {
    let config = match borg::config::load_config::<borg::config::Config>(None) {
        Ok(c) => c,
        Err(e) => {
            return vec![Finding::error(
                format!("could not load borg config: {e}"),
                "ensure ~/.config/sb/borg.yml exists (sb bootstrap)",
            )];
        }
    };
    let Some(sg) = config.signal.as_ref() else {
        return vec![Finding::info("signal not configured (no `signal:` block in borg.yml)")];
    };
    signal_findings_for(sg)
}

fn signal_findings_for(sg: &SignalConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    if sg.host.trim().is_empty() {
        findings.push(Finding::error(
            "signal.host is empty",
            "set signal.host to the exact hostname of the machine that should run Signal ingest",
        ));
        return findings;
    }
    let current = current_hostname();
    if !current.eq_ignore_ascii_case(&sg.host) {
        findings.push(Finding::info(format!(
            "signal.host={:?}, hostname={current:?} (this machine does not run Signal ingest)",
            sg.host
        )));
        return findings;
    }

    // state_dir is internal to borg, not a config field: the canonical
    // path comes from vault::paths. See
    // docs/design/2026-05-24-signal-state-dir-internalization.md.
    let state_dir = vault::paths::borg_signal_state_dir();
    findings.extend(state_dir_findings(&state_dir));

    // Surface the signal-rs CLI presence; operators reach this check
    // before they ever try to link, so naming the install command up
    // front beats a cryptic "command not found" later.
    findings.extend(signal_rs_cli_findings());

    // Live open + status probe, run on an isolated thread inside borg
    // (signal-rs futures are !Send) so sb's doctor carries no signal-rs
    // dependency.
    match borg::probe_signal(&state_dir) {
        Ok(borg::SignalProbe::Linked {
            account,
            device_id,
            linked_devices,
            bootstrapped,
        }) => {
            findings.push(Finding::ok(format!(
                "linked as account={account} device_id={device_id} linked_devices={linked_devices}"
            )));
            if !bootstrapped {
                findings.push(Finding::warn(
                    "linked but the phone->device sync session is not yet established - Note-to-Self will NOT be ingested until borg sends once",
                    format!(
                        "normally auto-fixed on borg (re)start; if it persists, run: signal-rs send --to self --state-dir {} \"ping\"",
                        state_dir.display()
                    ),
                ));
            }
        }
        Ok(borg::SignalProbe::NotLinked) => findings.push(Finding::error(
            format!("state_dir {} is not linked", state_dir.display()),
            format!("signal-rs link --name borg --state-dir {}", state_dir.display()),
        )),
        Ok(borg::SignalProbe::PartiallyLinked) => findings.push(Finding::error(
            format!("state_dir {} is partially linked", state_dir.display()),
            format!(
                "re-run signal-rs link --name borg --state-dir {} to resume",
                state_dir.display()
            ),
        )),
        Ok(borg::SignalProbe::Deauthorized) => findings.push(Finding::error(
            format!("state_dir {} is deauthorized", state_dir.display()),
            format!(
                "re-run signal-rs link --name borg --state-dir {} after re-authorizing on the primary phone",
                state_dir.display()
            ),
        )),
        Ok(borg::SignalProbe::OpenFailed(msg)) => findings.push(Finding::error(
            format!("Client::open failed: {msg}"),
            "inspect the signal-rs state directory; corruption usually requires re-linking",
        )),
        Ok(borg::SignalProbe::StatusFailed(msg)) => findings.push(Finding::warn(
            format!("status() failed (open succeeded): {msg}"),
            "transient; re-run `sb doctor` after the network stabilises",
        )),
        Err(e) => findings.push(Finding::error(
            format!("signal probe runtime failed: {e}"),
            "rebuild sb (this is a build-time runtime error, not a config issue)",
        )),
    }
    findings
}

fn state_dir_findings(state_dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    if !state_dir.exists() {
        findings.push(Finding::error(
            format!("state_dir {} does not exist", state_dir.display()),
            format!(
                "create it or run `signal-rs link --name borg --state-dir {}` to provision it",
                state_dir.display()
            ),
        ));
        return findings;
    }
    findings.push(Finding::ok(format!("state_dir exists at {}", state_dir.display())));
    findings
}

/// This machine's hostname for doctor host-parity messages, resolved through
/// borg (the single hostname-reading site) so sb carries no `hostname` dep.
fn current_hostname() -> String {
    borg::config::current_hostname().unwrap_or_default()
}

#[cfg(test)]
mod tests;
