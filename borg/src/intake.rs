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
use crate::receipts;
use crate::types::IngestMethod;
use eyre::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use vault::dlq::{self, DlqEntry, DlqStage, DlqStatus};
use vault::intake::{self, IntakeEntry, IntakeKind};
use vault::receipts::{FailureStage, ReceiptKind, failure_stage_from_dlq};

/// Map the intake-side kind (rich classification, includes media subtypes) to
/// the receipts-side kind (flat `url`/`text`/`binary`).
fn receipt_kind(kind: IntakeKind) -> ReceiptKind {
    match kind {
        IntakeKind::Url => ReceiptKind::Url,
        IntakeKind::Text | IntakeKind::Empty | IntakeKind::Unknown => ReceiptKind::Text,
        IntakeKind::Photo
        | IntakeKind::Voice
        | IntakeKind::Audio
        | IntakeKind::Document
        | IntakeKind::Sticker
        | IntakeKind::Video
        | IntakeKind::Animation
        | IntakeKind::Poll
        | IntakeKind::Location
        | IntakeKind::Contact => ReceiptKind::Binary,
    }
}

/// Best-effort receipts-DB write at the door. Errors are logged but do NOT
/// propagate: the markdown intake log already captured the input and is the
/// dual-write source of truth during the rollout window; a receipts DB
/// write failure must not block a legitimate intake.
fn receipts_record_received(method: IngestMethod, kind: IntakeKind, raw_input: &str, trace_id: &str) {
    log::debug!(
        "intake::receipts_record_received: trace={trace_id} method={method} kind={kind} raw_len={}",
        raw_input.len()
    );
    let conn = match receipts::open_default() {
        Ok(c) => c,
        Err(e) => {
            log::error!("receipts: failed to open DB for trace={trace_id}: {e:#}");
            return;
        }
    };
    if let Err(e) = receipts::record_received(&conn, trace_id, method.into(), receipt_kind(kind), raw_input) {
        log::error!("receipts: failed to record_received trace={trace_id}: {e:#}");
    }
}

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
    // Dual-write: also record the receipts-DB row. Best-effort.
    receipts_record_received(method, kind, preview, trace_id);
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
    // Dual-write: receipts-DB row. The receipts row stores the preview as
    // `raw_input` (small UTF-8); the full bytes still live in the sidecar.
    receipts_record_received(method, kind, preview, trace_id);
    log::info!(
        "intake: recorded trace={trace_id} method={method} kind={kind} preview={}",
        preview_text(preview)
    );
    Ok(())
}

/// Capture an input at the door: write the raw-input sidecar AND the
/// `received` receipts row. BOTH writes propagate - on any failure the caller
/// (the door) surfaces `Failed` and does not dispatch. This is the
/// immediate-capture invariant. The sidecar payload is unchanged from the
/// legacy path: verbatim text/URL, or a short descriptor for large binaries
/// (the remote doors do not hold the raw bytes at this checkpoint).
///
/// Replaces `record_intake` / `record_intake_with_sidecar` (markdown +
/// sidecar + best-effort receipts) once the call sites are switched in Phase 2.
pub fn record_received_with_sidecar(
    config: &Config,
    method: IngestMethod,
    kind: IntakeKind,
    preview: &str,
    sidecar_bytes: &[u8],
    trace_id: &str,
) -> Result<()> {
    log::debug!(
        "intake::record_received_with_sidecar: trace={trace_id} method={method} kind={kind} sidecar_bytes={}",
        sidecar_bytes.len()
    );
    let conn = receipts::open_default().context("open receipts DB for capture")?;
    let root = vault_root(config)?;
    record_received_with_sidecar_to(&conn, &root, method, kind, preview, sidecar_bytes, trace_id)
}

/// Conn/root-injectable core of [`record_received_with_sidecar`]. Sidecar
/// first (raw-input record lands on disk), then the receipts row. Both
/// propagate.
fn record_received_with_sidecar_to(
    conn: &Connection,
    vault_root: &Path,
    method: IngestMethod,
    kind: IntakeKind,
    preview: &str,
    sidecar_bytes: &[u8],
    trace_id: &str,
) -> Result<()> {
    intake::write_raw_input(vault_root, trace_id, sidecar_bytes).context("Failed to write intake sidecar")?;
    receipts::record_received(conn, trace_id, method.into(), receipt_kind(kind), preview)
        .with_context(|| format!("Failed to record receipts row trace={trace_id}"))?;
    log::info!(
        "intake: captured trace={trace_id} method={method} kind={kind} preview={}",
        preview_text(preview)
    );
    Ok(())
}

/// Record a terminal failure at the door (rejection or fetch-fail), replacing
/// `record_dlq`. Carries the per-site `FailureStage` so Signal's `FetchFailed`
/// is not collapsed to `IntakeRejected`. Best-effort: the input's durability
/// is already guaranteed by the preceding [`record_received_with_sidecar`]; a
/// failed `mark_failed` leaves the row `received` for the watchdog to
/// crash-promote, so we log and move on.
pub fn record_failure_at_door(method: IngestMethod, trace_id: &str, stage: FailureStage, reason: &str) {
    log::debug!("intake::record_failure_at_door: trace={trace_id} method={method} stage={stage} reason={reason}");
    let conn = match receipts::open_default() {
        Ok(c) => c,
        Err(e) => {
            log::error!("receipts: failed to open DB for failure trace={trace_id}: {e:#}");
            return;
        }
    };
    if let Err(e) = record_failure_at_door_to(&conn, method, trace_id, stage, reason) {
        log::error!("receipts: failed to record failure trace={trace_id} stage={stage}: {e:#}");
    }
}

/// Conn-injectable core of [`record_failure_at_door`]. UPSERT (gap-proof):
/// INSERT-OR-IGNORE a `received` row (a no-op preserving the real captured
/// data when the preceding capture already ran; a cold-path insert otherwise),
/// then `mark_failed` with the given stage. A rejection therefore lands a
/// `failed` row regardless of whether a prior capture ran in this control flow.
fn record_failure_at_door_to(
    conn: &Connection,
    method: IngestMethod,
    trace_id: &str,
    stage: FailureStage,
    reason: &str,
) -> Result<()> {
    // Cold-path values (kind=Text, raw_input=reason) apply only if no row
    // exists yet; INSERT OR IGNORE never clobbers a prior capture's data.
    receipts::record_received(conn, trace_id, method.into(), ReceiptKind::Text, reason)
        .with_context(|| format!("upsert received row for failure trace={trace_id}"))?;
    receipts::mark_failed(conn, trace_id, stage, reason)
        .with_context(|| format!("mark_failed trace={trace_id} stage={stage}"))?;
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
    // Dual-write: also mark the receipts row failed with the mapped stage.
    // This is the only path that emits the IntakeRejected receipts stage
    // today; other stages (FetchFailed, QualityBlocked, ...) are still
    // produced via the legacy DLQ path during the Phase-2 rollout window.
    let receipt_stage = failure_stage_from_dlq(stage);
    match receipts::open_default() {
        Ok(conn) => {
            if let Err(e) = receipts::mark_failed(&conn, trace_id, receipt_stage, reason) {
                log::error!("receipts: failed to mark_failed trace={trace_id} stage={receipt_stage}: {e:#}");
            }
        }
        Err(e) => log::error!("receipts: failed to open DB for trace={trace_id}: {e:#}"),
    }
}

// Re-export vault enums so call sites can `use crate::intake::{Kind, Stage};`
// rather than importing from two different vault modules.
pub use vault::dlq::DlqStage as Stage;
pub use vault::dlq::DlqStatus as Status;
pub use vault::intake::IntakeKind as Kind;

#[cfg(test)]
mod tests;
