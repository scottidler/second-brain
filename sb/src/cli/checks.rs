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
const SIGNAL_RS_INSTALL_HINT: &str =
    "cargo install --git https://github.com/scottidler/signal-rs --bin signal-rs --tag v0.2.1";

/// External-binary detection. Surface absence with the exact install
/// command. Signal-rs check is gated on `config.signal.is_some()` so a
/// Telegram-only install isn't pestered.
fn external_binaries_findings() -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(fabric_cli_findings());
    findings.extend(fabric_default_patterns_findings());
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

/// Drift detection for the 14 fabric patterns. Mismatch is Warn here
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
        }
        Err(e) => findings.push(Finding::warn(
            format!("borg receipts health query failed: {e}"),
            "sb borg log --status failed (manual investigation)".to_string(),
        )),
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
    let (received, succeeded, failed) = borg::receipts::count_by_status(&conn).map_err(|e| e.to_string())?;
    let by_stage = borg::receipts::count_failed_by_stage(&conn).map_err(|e| e.to_string())?;
    let stage_summary = if by_stage.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = by_stage.iter().map(|(s, n)| format!("{s}={n}")).collect();
        format!(" [{}]", parts.join(", "))
    };
    Ok(format!(
        "receipts: {received} received, {succeeded} succeeded, {failed} failed{stage_summary}"
    ))
}

fn vault_findings() -> Vec<Finding> {
    // Open the oracle SQLite index and pull two readings: total note count
    // (and schema gaps) and embedding coverage. Read-only.
    let config = match oracle::Config::load(None) {
        Ok(c) => c,
        Err(e) => {
            return vec![Finding::error(
                format!("could not load oracle config: {e}"),
                format!(
                    "ensure {} exists (sb bootstrap)",
                    vault::paths::oracle_config().display()
                ),
            )];
        }
    };
    let db = match vault::search::SearchIndex::open(&config.db_path()) {
        Ok(db) => db,
        Err(e) => {
            return vec![Finding::warn(
                format!("oracle SQLite index not openable: {e}"),
                "sb oracle index".to_string(),
            )];
        }
    };

    let mut findings = Vec::new();

    match db.stats() {
        Ok(stats) => {
            findings.push(Finding::ok(format!(
                "{} note(s) indexed across {} domain(s)",
                stats.total_notes,
                stats.by_domain.len()
            )));
            let total_gaps: u64 = stats.schema_gaps.iter().map(|(_, n)| n).sum();
            if total_gaps > 0 {
                let detail = stats
                    .schema_gaps
                    .iter()
                    .filter(|(_, n)| *n > 0)
                    .map(|(field, n)| format!("{field}={n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                findings.push(Finding::info(format!("schema gaps: {detail}")));
            }
        }
        Err(e) => findings.push(Finding::warn(
            format!("vault stats query failed: {e}"),
            "sb oracle stats".to_string(),
        )),
    }

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
            // Live get_me() probe is multi-thread tokio safe; run it in a
            // throwaway runtime so we don't depend on the caller's runtime
            // shape.
            match telegram_probe_get_me(&token) {
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

fn telegram_probe_get_me(token: &str) -> Result<String, String> {
    // `sb doctor` runs inside `#[tokio::main]`'s multi-thread runtime, so we
    // cannot build a fresh runtime on the calling thread (panics with
    // "Cannot start a runtime from within a runtime"). Spawn an isolated OS
    // thread so the new runtime owns its own thread context, then join.
    let token = token.to_string();
    std::thread::spawn(move || -> Result<String, String> {
        use teloxide::prelude::Requester;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("build current-thread runtime: {e}"))?;
        rt.block_on(async move {
            let bot = teloxide::Bot::new(&token);
            match bot.get_me().await {
                Ok(me) => Ok(me.user.username.clone().unwrap_or_else(|| "<no-username>".to_string())),
                Err(e) => Err(e.to_string()),
            }
        })
    })
    .join()
    .map_err(|_| "telegram probe thread panicked".to_string())?
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

    // Live open + status probe. signal-rs futures are !Send so run on a
    // current-thread runtime + LocalSet inside an isolated OS thread (see
    // signal_probe_status).
    match signal_probe_status(&state_dir) {
        Ok(SignalProbe::Linked {
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
        Ok(SignalProbe::NotLinked) => findings.push(Finding::error(
            format!("state_dir {} is not linked", state_dir.display()),
            format!("signal-rs link --name borg --state-dir {}", state_dir.display()),
        )),
        Ok(SignalProbe::PartiallyLinked) => findings.push(Finding::error(
            format!("state_dir {} is partially linked", state_dir.display()),
            format!(
                "re-run signal-rs link --name borg --state-dir {} to resume",
                state_dir.display()
            ),
        )),
        Ok(SignalProbe::Deauthorized) => findings.push(Finding::error(
            format!("state_dir {} is deauthorized", state_dir.display()),
            format!(
                "re-run signal-rs link --name borg --state-dir {} after re-authorizing on the primary phone",
                state_dir.display()
            ),
        )),
        Ok(SignalProbe::OpenFailed(msg)) => findings.push(Finding::error(
            format!("Client::open failed: {msg}"),
            "inspect the signal-rs state directory; corruption usually requires re-linking",
        )),
        Ok(SignalProbe::StatusFailed(msg)) => findings.push(Finding::warn(
            format!("status() failed (open succeeded): {msg}"),
            "transient; re-run `sb doctor signal` after the network stabilises",
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

enum SignalProbe {
    Linked {
        account: String,
        device_id: u32,
        linked_devices: usize,
        /// Whether a successful cold-start bootstrap self-send has been
        /// recorded for this identity. `false` while linked means Note-to-Self
        /// ingest is not yet established (the phone has no sync session to us).
        /// See docs/design/2026-05-28-signal-cold-start-bootstrap.md.
        bootstrapped: bool,
    },
    NotLinked,
    PartiallyLinked,
    Deauthorized,
    OpenFailed(String),
    StatusFailed(String),
}

fn signal_probe_status(state_dir: &Path) -> Result<SignalProbe, String> {
    // See `telegram_probe_get_me` for the runtime-isolation rationale.
    // signal-rs's futures are !Send, so we need a current-thread runtime plus
    // a LocalSet; both live on the spawned OS thread.
    let dir = state_dir.to_path_buf();
    std::thread::spawn(move || -> Result<SignalProbe, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("build current-thread runtime: {e}"))?;
        let local = tokio::task::LocalSet::new();
        Ok(rt.block_on(local.run_until(async move {
            use signal_rs::{Client, OpenError};
            match Client::open(&dir).await {
                Ok(client) => match client.status().await {
                    Ok(status) => {
                        let bootstrapped = borg::signal::bootstrap_recorded(&status.account_number, status.device_id);
                        SignalProbe::Linked {
                            account: status.account_number,
                            device_id: status.device_id,
                            linked_devices: status.linked_devices.len(),
                            bootstrapped,
                        }
                    }
                    Err(e) => SignalProbe::StatusFailed(e.to_string()),
                },
                Err(OpenError::NotLinked) => SignalProbe::NotLinked,
                Err(OpenError::PartiallyLinked) => SignalProbe::PartiallyLinked,
                Err(OpenError::Deauthorized) => SignalProbe::Deauthorized,
                Err(e) => SignalProbe::OpenFailed(e.to_string()),
            }
        })))
    })
    .join()
    .map_err(|_| "signal probe thread panicked".to_string())?
}

fn current_hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_signal(host: &str) -> SignalConfig {
        SignalConfig {
            allowed_senders: vec![],
            notification_recipient: None,
            host: host.to_string(),
            notetoself_rate_threshold_per_hour: 100,
        }
    }

    fn make_telegram(token: &str, host: Option<&str>) -> TelegramConfig {
        TelegramConfig {
            bot_token: token.to_string(),
            allowed_chat_ids: vec![],
            notification_chat_id: None,
            host: host.map(str::to_string),
        }
    }

    #[test]
    fn signal_findings_host_mismatch_short_circuits() {
        let sg = make_signal("definitely-not-this-host-abc-xyz");
        let findings = signal_findings_for(&sg);
        assert_eq!(findings.len(), 1, "host mismatch must short-circuit");
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].message.contains("does not run Signal ingest"));
    }

    #[test]
    fn signal_findings_empty_host_is_error() {
        let sg = make_signal("");
        let findings = signal_findings_for(&sg);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn signal_state_dir_missing_is_error() {
        let findings = state_dir_findings(Path::new("/nonexistent-signal-state-dir-xyz"));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("does not exist"));
    }

    #[test]
    fn telegram_findings_host_mismatch_short_circuits() {
        let tg = make_telegram("DUMMY_TOKEN_ENV_VAR", Some("definitely-not-this-host-abc-xyz"));
        let findings = telegram_findings_for(&tg);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].message.contains("does not run Telegram ingest"));
    }

    #[test]
    fn firefox_finding_warns_on_snap_with_migration_fix() {
        use borg::extension::install::FirefoxInstall;
        let f = firefox_finding(&FirefoxInstall::Snap);
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.message.contains("snap"));
        assert!(
            f.suggested_fix.as_deref().unwrap_or_default().contains("firefox-opt"),
            "snap fix must name the firefox-opt migration"
        );
    }

    #[test]
    fn firefox_finding_ok_on_opt_tarball_and_info_on_unknown() {
        use borg::extension::install::FirefoxInstall;
        assert_eq!(
            firefox_finding(&FirefoxInstall::Tarball(std::path::PathBuf::from("/opt/firefox"))).severity,
            Severity::Ok
        );
        assert_eq!(firefox_finding(&FirefoxInstall::Unknown).severity, Severity::Info);
    }

    #[test]
    fn telegram_findings_empty_token_is_error() {
        let tg = make_telegram("", None);
        let findings = telegram_findings_for(&tg);
        assert!(
            findings
                .iter()
                .any(|f| f.severity == Severity::Error && f.message.contains("bot-token")),
            "empty bot-token must emit Error finding"
        );
    }

    mod drift {
        use super::super::*;
        use serial_test::serial;

        struct EnvGuard {
            key: &'static str,
            original: Option<std::ffi::OsString>,
        }

        impl EnvGuard {
            fn set(key: &'static str, value: &Path) -> Self {
                let original = std::env::var_os(key);
                // SAFETY: env mutation for path-resolution tests; restored on Drop.
                unsafe { std::env::set_var(key, value) };
                Self { key, original }
            }
        }

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                // SAFETY: restoring env.
                unsafe {
                    match self.original.take() {
                        Some(v) => std::env::set_var(self.key, v),
                        None => std::env::remove_var(self.key),
                    }
                }
            }
        }

        #[test]
        #[serial(env_xdg)]
        fn shared_config_missing_emits_errors() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

            let findings = shared_config_findings();
            let errors: Vec<_> = findings.iter().filter(|f| f.severity == Severity::Error).collect();
            assert_eq!(errors.len(), 3, "all three shared YAMLs should be missing");
            for f in &errors {
                assert!(f.suggested_fix.as_deref() == Some("sb bootstrap"));
            }
        }

        #[test]
        #[serial(env_xdg)]
        fn shared_config_match_emits_ok() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
            crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");

            let findings = shared_config_findings();
            assert!(
                findings.iter().all(|f| f.severity == Severity::Ok),
                "all match -> all Ok"
            );
        }

        #[test]
        #[serial(env_xdg)]
        fn shared_config_edit_emits_info() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
            crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");
            std::fs::write(tmp.path().join("sb").join("canonical-tags.yml"), "edited").expect("mutate");

            let findings = shared_config_findings();
            let info: Vec<_> = findings.iter().filter(|f| f.severity == Severity::Info).collect();
            assert!(info.iter().any(|f| f.message.contains("canonical-tags.yml")));
        }

        #[test]
        #[serial(env_xdg)]
        fn patterns_missing_emits_error() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

            let findings = pattern_findings();
            assert!(
                findings
                    .iter()
                    .any(|f| f.severity == Severity::Error && f.message.contains("missing")),
                "missing patterns must emit Error: {:?}",
                findings
            );
        }

        #[test]
        #[serial(env_xdg)]
        fn patterns_match_emits_ok() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
            crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");

            let findings = pattern_findings();
            assert_eq!(findings.len(), 1, "single Ok finding when all match");
            assert_eq!(findings[0].severity, Severity::Ok);
        }

        #[test]
        #[serial(env_xdg)]
        fn fabric_cli_findings_emits_error_when_missing() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = EnvGuard::set("PATH", tmp.path());

            let findings = super::super::fabric_cli_findings();
            assert!(findings.iter().any(|f| f.severity == Severity::Error
                && f.message.contains("fabric")
                && f.suggested_fix.as_deref() == Some(super::super::FABRIC_INSTALL_HINT)));
        }

        #[test]
        #[serial(env_xdg)]
        fn fabric_cli_findings_emits_ok_when_present() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let stub = tmp.path().join("fabric");
            std::fs::write(&stub, "#!/bin/sh\necho 'fabric 1.2.3'\n").expect("write stub");
            // chmod +x via a Permissions write
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            let _guard = EnvGuard::set("PATH", tmp.path());

            let findings = super::super::fabric_cli_findings();
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].severity, Severity::Ok);
            assert!(findings[0].message.contains("fabric 1.2.3"));
        }

        #[test]
        #[serial(env_xdg)]
        fn signal_rs_cli_findings_emits_error_when_missing() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = EnvGuard::set("PATH", tmp.path());

            let findings = super::super::signal_rs_cli_findings();
            assert!(findings.iter().any(|f| f.severity == Severity::Error
                && f.suggested_fix.as_deref() == Some(super::super::SIGNAL_RS_INSTALL_HINT)));
        }

        #[test]
        #[serial(env_xdg)]
        fn patterns_drift_emits_warn() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
            crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");
            let pattern_path = tmp.path().join("sb").join("patterns").join("distill-article.md");
            std::fs::write(&pattern_path, "drifted pattern body").expect("mutate pattern");

            let findings = pattern_findings();
            let warns: Vec<_> = findings.iter().filter(|f| f.severity == Severity::Warn).collect();
            assert_eq!(warns.len(), 1, "one drifted pattern -> one Warn: {:?}", findings);
            assert!(warns[0].message.contains("drifted"));
            assert_eq!(warns[0].suggested_fix.as_deref(), Some("sb bootstrap --force"));
        }
    }
}
