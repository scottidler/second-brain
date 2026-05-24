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
            name: "embedding cache",
            findings: embedding_findings(),
        },
        Section {
            name: "borg",
            findings: borg_findings(),
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

/// Compare the shared-config files in the repo (`config/*.yml`) against the
/// installed copies in `~/.config/sb/`. Drift here means borg and
/// cortex disagree on the canonical tag vocabulary - a class of silent bugs
/// only caught by hash-comparing the source-of-truth against the runtime
/// copy. Mirror of `pattern_findings`.
fn shared_config_findings() -> Vec<Finding> {
    let repo_shared = std::path::Path::new("config");
    let installed = Some(vault::paths::config_root());
    let mut findings = Vec::new();
    let Some(installed) = installed else {
        findings.push(Finding::error(
            "dirs::config_dir() returned None",
            "check XDG_CONFIG_HOME",
        ));
        return findings;
    };
    if !repo_shared.exists() {
        findings.push(Finding::info(
            "config/ not present in CWD (run from repo root for drift detection)",
        ));
        return findings;
    }
    if !installed.exists() {
        findings.push(Finding::warn(
            format!("{} missing", installed.display()),
            "otto deploy".to_string(),
        ));
        return findings;
    }
    let repo_files: Vec<_> = std::fs::read_dir(repo_shared)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("yml"))
        .collect();
    if repo_files.is_empty() {
        findings.push(Finding::info("no .yml files under config/ (nothing to compare)"));
        return findings;
    }
    let mut drift = 0usize;
    for entry in &repo_files {
        let name = entry.file_name();
        let installed_path = installed.join(&name);
        if !installed_path.exists() {
            drift += 1;
            continue;
        }
        if std::fs::read(entry.path()).ok() != std::fs::read(&installed_path).ok() {
            drift += 1;
        }
    }
    if drift == 0 {
        findings.push(Finding::ok(format!(
            "{} shared-config file(s) in sync",
            repo_files.len()
        )));
    } else {
        findings.push(Finding::warn(
            format!("{drift} of {} shared-config file(s) drifted vs repo", repo_files.len()),
            "otto deploy".to_string(),
        ));
    }
    findings
}

fn pattern_findings() -> Vec<Finding> {
    // Compare borg/patterns/*.md in the repo (working tree) against ~/.config/sb/patterns/*.md.
    // We can only detect drift on machines where the repo is checked out at the expected path.
    let repo_patterns = std::path::Path::new("borg/patterns");
    let installed = Some(vault::paths::patterns_dir());
    let mut findings = Vec::new();
    let Some(installed) = installed else {
        findings.push(Finding::error(
            "dirs::config_dir() returned None",
            "check XDG_CONFIG_HOME",
        ));
        return findings;
    };
    if !repo_patterns.exists() {
        findings.push(Finding::info(
            "borg/patterns not present in CWD (run from repo root for drift detection)",
        ));
        return findings;
    }
    if !installed.exists() {
        findings.push(Finding::warn(
            format!("{} missing", installed.display()),
            "otto deploy".to_string(),
        ));
        return findings;
    }
    let repo_files: Vec<_> = std::fs::read_dir(repo_patterns)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    let mut drift = 0usize;
    for entry in &repo_files {
        let name = entry.file_name();
        let installed_path = installed.join(&name);
        if !installed_path.exists() {
            drift += 1;
            continue;
        }
        if std::fs::read(entry.path()).ok() != std::fs::read(&installed_path).ok() {
            drift += 1;
        }
    }
    if drift == 0 {
        findings.push(Finding::ok(format!("{} patterns in sync", repo_files.len())));
    } else {
        findings.push(Finding::warn(
            format!("{drift} of {} patterns drifted vs repo", repo_files.len()),
            "otto deploy".to_string(),
        ));
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
    // Run borg's invariant audit and surface the counts. This is the same
    // computation behind GET /health/audit, so the numbers match what the
    // running daemon's HTTP endpoint reports.
    let config = match borg::config::load_config(None) {
        Ok(c) => c,
        Err(e) => {
            return vec![Finding::error(
                format!("could not load borg config: {e}"),
                format!("ensure {} exists (sb bootstrap)", vault::paths::borg_config().display()),
            )];
        }
    };

    let health = match borg::triage::audit_health_stats(&config) {
        Ok(h) => h,
        Err(e) => {
            return vec![Finding::error(
                format!("borg audit health query failed: {e}"),
                "sb borg audit --invariant (manual investigation)".to_string(),
            )];
        }
    };

    let mut findings = vec![Finding::ok(format!(
        "intake: {} row(s), ledger: {} row(s), dlq: {} row(s)",
        health.intake_rows, health.ledger_rows, health.dlq_rows
    ))];
    if health.dlq_pending > 0 {
        findings.push(Finding::warn(
            format!("dlq: {} pending row(s)", health.dlq_pending),
            "sb borg dlq list --status pending (or sb borg dlq replay <trace>)".to_string(),
        ));
    } else {
        findings.push(Finding::ok("dlq: 0 pending".to_string()));
    }
    if health.orphan_count > 0 {
        let age = health
            .oldest_orphan_secs
            .map(|s| format!("{}s", s))
            .unwrap_or_else(|| "unknown".to_string());
        findings.push(Finding::warn(
            format!(
                "{} orphan(s) in intake without ledger or dlq resolution (oldest: {age})",
                health.orphan_count
            ),
            "sb borg audit --invariant".to_string(),
        ));
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
    findings
}

fn telegram_probe_get_me(token: &str) -> Result<String, String> {
    use teloxide::prelude::Requester;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("build current-thread runtime: {e}"))?;
    rt.block_on(async move {
        let bot = teloxide::Bot::new(token);
        match bot.get_me().await {
            Ok(me) => Ok(me.user.username.clone().unwrap_or_else(|| "<no-username>".to_string())),
            Err(e) => Err(e.to_string()),
        }
    })
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

    findings.extend(state_dir_findings(&sg.state_dir));

    // Live open + status probe. signal-rs futures are !Send so run on a
    // current-thread runtime + LocalSet.
    match signal_probe_status(&sg.state_dir) {
        Ok(SignalProbe::Linked {
            account,
            device_id,
            linked_devices,
        }) => findings.push(Finding::ok(format!(
            "linked as account={account} device_id={device_id} linked_devices={linked_devices}"
        ))),
        Ok(SignalProbe::NotLinked) => findings.push(Finding::error(
            format!("state_dir {} is not linked", sg.state_dir.display()),
            format!("signal-rs link --name borg --state-dir {}", sg.state_dir.display()),
        )),
        Ok(SignalProbe::PartiallyLinked) => findings.push(Finding::error(
            format!("state_dir {} is partially linked", sg.state_dir.display()),
            format!(
                "re-run signal-rs link --name borg --state-dir {} to resume",
                sg.state_dir.display()
            ),
        )),
        Ok(SignalProbe::Deauthorized) => findings.push(Finding::error(
            format!("state_dir {} is deauthorized", sg.state_dir.display()),
            format!(
                "re-run signal-rs link --name borg --state-dir {} after re-authorizing on the primary phone",
                sg.state_dir.display()
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

    // Resolve symlinks before comparing so a state_dir that symlinks into
    // signal-rs's CLI default is still caught.
    let configured = std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let default = dirs::data_local_dir().map(|d| d.join("signal-rs"));
    if let Some(default) = default {
        let default = std::fs::canonicalize(&default).unwrap_or(default);
        if configured == default {
            findings.push(Finding::warn(
                format!(
                    "state_dir {} collides with signal-rs's CLI default ({})",
                    configured.display(),
                    default.display()
                ),
                "use a borg-owned path like ~/.local/share/sb/borg/signal-state/ to avoid Double-Ratchet contention with ad-hoc signal-rs CLI use",
            ));
        }
    }
    findings
}

enum SignalProbe {
    Linked {
        account: String,
        device_id: u32,
        linked_devices: usize,
    },
    NotLinked,
    PartiallyLinked,
    Deauthorized,
    OpenFailed(String),
    StatusFailed(String),
}

fn signal_probe_status(state_dir: &Path) -> Result<SignalProbe, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("build current-thread runtime: {e}"))?;
    let local = tokio::task::LocalSet::new();
    let dir = state_dir.to_path_buf();
    Ok(rt.block_on(local.run_until(async move {
        use signal_rs::{Client, OpenError};
        match Client::open(&dir).await {
            Ok(client) => match client.status().await {
                Ok(status) => SignalProbe::Linked {
                    account: status.account_number,
                    device_id: status.device_id,
                    linked_devices: status.linked_devices.len(),
                },
                Err(e) => SignalProbe::StatusFailed(e.to_string()),
            },
            Err(OpenError::NotLinked) => SignalProbe::NotLinked,
            Err(OpenError::PartiallyLinked) => SignalProbe::PartiallyLinked,
            Err(OpenError::Deauthorized) => SignalProbe::Deauthorized,
            Err(e) => SignalProbe::OpenFailed(e.to_string()),
        }
    })))
}

fn current_hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_signal(host: &str, state_dir: PathBuf) -> SignalConfig {
        SignalConfig {
            state_dir,
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
        let sg = make_signal("definitely-not-this-host-abc-xyz", PathBuf::from("/nonexistent"));
        let findings = signal_findings_for(&sg);
        assert_eq!(findings.len(), 1, "host mismatch must short-circuit");
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].message.contains("does not run Signal ingest"));
    }

    #[test]
    fn signal_findings_empty_host_is_error() {
        let sg = make_signal("", PathBuf::from("/nonexistent"));
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
    fn signal_state_dir_collision_with_default_is_warn() {
        let Some(default) = dirs::data_local_dir() else {
            return;
        };
        let default = default.join("signal-rs");
        if !default.exists() {
            std::fs::create_dir_all(&default).expect("create signal-rs default");
        }
        let findings = state_dir_findings(&default);
        let has_collision = findings
            .iter()
            .any(|f| f.severity == Severity::Warn && f.message.contains("collides"));
        assert!(
            has_collision,
            "state_dir == signal-rs CLI default must emit a collision Warn"
        );
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
}
