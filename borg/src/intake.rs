//! Borg-side intake / DLQ entry points.
//!
//! Every intake path (telegram, http, ntfy, discord, cli) generates a
//! `trace_id` and calls `record_intake` BEFORE any classification, filter,
//! or pipeline dispatch. When the input is rejected outright (disallowed
//! chat, unsupported media, bad payload) the same path calls `record_dlq`
//! with `DlqStage::IntakeReject`. Together these are the durable-capture
//! invariant from the 2026-05-11 intake-log + DLQ design doc.
//!
//! The vault helpers handle the actual file writes; this module exists to
//! resolve paths from `borg::config::Config` and to keep the borg crate's
//! call sites uniform.

use crate::config::Config;
use crate::types::IngestMethod;
use eyre::{Context, Result};
use std::path::PathBuf;
use vault::dlq::{self, DlqEntry, DlqStage, DlqStatus};
use vault::intake::{self, IntakeEntry, IntakeKind};

/// Resolve the vault root from borg config via the unified resolver.
pub fn vault_root(config: &Config) -> Result<PathBuf> {
    config.vault_root()
}

pub fn intake_path(config: &Config) -> Result<PathBuf> {
    Ok(intake::intake_path(&vault_root(config)?))
}

pub fn dlq_path(config: &Config) -> Result<PathBuf> {
    Ok(dlq::dlq_path(&vault_root(config)?))
}

fn now_date_time(config: &Config) -> (String, String) {
    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = chrono::Utc::now().with_timezone(&tz);
    (now.format("%Y-%m-%d").to_string(), now.format("%H:%M").to_string())
}

/// Truncate `s` to 80 characters (preview budget), appending `...` if cut.
pub fn preview_text(s: &str) -> String {
    let one_line = s.replace(['\n', '\r'], " ");
    if one_line.len() <= 80 { one_line } else { format!("{}...", &one_line[..80]) }
}

/// Build a structured descriptor for a binary input. Used as the preview
/// (and raw-input sidecar contents) for photos, voice notes, audio,
/// documents, stickers, etc.
pub fn binary_descriptor(kind: IntakeKind, filename: &str, bytes_len: usize, mime: Option<&str>) -> String {
    let kind_label = kind.as_str();
    match mime {
        Some(m) => format!("[{kind_label}: {filename}, {bytes_len} bytes, {m}]"),
        None => format!("[{kind_label}: {filename}, {bytes_len} bytes]"),
    }
}

/// Append the intake row + write the raw-input sidecar. Returns an error
/// (which the caller MUST propagate / surface to the user); silent drops
/// are the bug this whole subsystem exists to prevent.
pub fn record_intake(
    config: &Config,
    method: IngestMethod,
    origin_ctx: &str,
    kind: IntakeKind,
    preview: &str,
    trace_id: &str,
) -> Result<()> {
    log::debug!(
        "intake::record_intake: trace={trace_id} method={method} kind={kind} origin={origin_ctx} preview_len={}",
        preview.len()
    );
    let root = vault_root(config)?;
    let intake_md = intake::intake_path(&root);
    let (date, time) = now_date_time(config);
    let entry = IntakeEntry {
        date,
        time,
        method: method.into(),
        origin_ctx: origin_ctx.to_string(),
        kind,
        preview: preview.to_string(),
        trace_id: trace_id.to_string(),
    };
    intake::append_entry(&intake_md, &entry).context("Failed to append intake row")?;
    // Sidecar mirrors the preview by default; binary inputs already pass a
    // structured descriptor. Callers that want the verbatim text body can
    // override via `record_intake_with_sidecar`.
    intake::write_raw_input(&root, trace_id, preview.as_bytes()).context("Failed to write intake sidecar")?;
    log::info!(
        "intake: recorded trace={trace_id} method={method} kind={kind} preview={}",
        preview_text(preview)
    );
    Ok(())
}

/// Like `record_intake` but the sidecar receives the explicit `raw_bytes`
/// instead of the preview - used for text bodies where the caller wants the
/// full input persisted but only a truncated preview in the table.
pub fn record_intake_with_sidecar(
    config: &Config,
    method: IngestMethod,
    origin_ctx: &str,
    kind: IntakeKind,
    preview: &str,
    raw_bytes: &[u8],
    trace_id: &str,
) -> Result<()> {
    log::debug!(
        "intake::record_intake_with_sidecar: trace={trace_id} method={method} kind={kind} raw_bytes={}",
        raw_bytes.len()
    );
    let root = vault_root(config)?;
    let intake_md = intake::intake_path(&root);
    let (date, time) = now_date_time(config);
    let entry = IntakeEntry {
        date,
        time,
        method: method.into(),
        origin_ctx: origin_ctx.to_string(),
        kind,
        preview: preview.to_string(),
        trace_id: trace_id.to_string(),
    };
    intake::append_entry(&intake_md, &entry).context("Failed to append intake row")?;
    intake::write_raw_input(&root, trace_id, raw_bytes).context("Failed to write intake sidecar")?;
    log::info!(
        "intake: recorded trace={trace_id} method={method} kind={kind} preview={}",
        preview_text(preview)
    );
    Ok(())
}

/// Append a DLQ row. Best-effort: errors are logged but do NOT propagate -
/// the caller (intake path or pipeline) already has its own error to
/// surface, and we don't want a DLQ write failure to mask the real failure.
pub fn record_dlq(
    config: &Config,
    method: IngestMethod,
    stage: DlqStage,
    reason: &str,
    preview: &str,
    trace_id: &str,
    replay_of: Option<&str>,
) {
    log::debug!(
        "intake::record_dlq: trace={trace_id} method={method} stage={stage} reason={reason} replay_of={replay_of:?}"
    );
    let dlq_md = match dlq_path(config) {
        Ok(p) => p,
        Err(e) => {
            log::error!("dlq: vault root not configured for trace={trace_id}: {e:#}");
            return;
        }
    };
    let (date, time) = now_date_time(config);
    let entry = DlqEntry {
        date,
        time,
        method: method.into(),
        stage,
        reason: reason.to_string(),
        preview: preview.to_string(),
        retries: 0,
        status: DlqStatus::Pending,
        trace_id: trace_id.to_string(),
        replay_of: replay_of.map(String::from),
    };
    if let Err(e) = dlq::append_entry(&dlq_md, &entry) {
        log::error!("dlq: failed to append row for trace={trace_id}: {e:#}");
    } else {
        log::info!("dlq: recorded trace={trace_id} method={method} stage={stage} reason={reason}");
    }
}

// Re-export vault enums so call sites can `use crate::intake::{Kind, Stage};`
// rather than importing from two different vault modules.
pub use vault::dlq::DlqStage as Stage;
pub use vault::dlq::DlqStatus as Status;
pub use vault::intake::IntakeKind as Kind;
