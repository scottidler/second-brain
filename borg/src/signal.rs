//! Signal transport. Peer to `telegram.rs` and the other inbound transports;
//! delegates to the same classify+intake+pipeline+notify chain.
//!
//! Privacy: the [`accepted_envelope`] helper is the single gate that decides
//! whether an envelope reaches the pipeline. Its pattern match is
//! privacy-load-bearing - see `docs/design/2026-05-24-signal-as-borg-transport.md`
//! and the negative-direction tests in `tests.rs`.
//!
//! Hostname gating happens upstream in `lib.rs::serve_init` via
//! `config::is_local_host`. By the time `run` is called the supervisor has
//! already decided this is the right machine.

use crate::assets;
use crate::backoff::ExponentialBackoff;
use crate::config::{Config, SignalConfig};
use crate::intake::{self as intake_log, Kind as IntakeKind};
use crate::notify;
use crate::pipeline;
use crate::router::extract_url_from_text;
use crate::trace;
use crate::types::{ContentKind, IngestMethod};
use vault::receipts::FailureStage;

use eyre::Result;
use signal_rs::{AttachmentPointer, Client, Envelope, OpenError, ReceiveError, Recipient, SyncMessage};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

mod bootstrap;

#[cfg(test)]
mod tests;

const RATE_GATE_WINDOW: Duration = Duration::from_secs(3600);
const ATTACHMENT_TMP_PREFIX: &str = "borg-signal-";

/// Inbound source kind, after the privacy gate has accepted the envelope.
/// `SelfSync` is the Note-to-Self path; `Peer` is an allowlisted DM. The two
/// variants drive recipient resolution for the outbound ack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptedSource {
    SelfSync,
    Peer { aci: String },
}

impl AcceptedSource {
    /// Resolve the reply [`Recipient`]. `SelfSync` replies stay on the
    /// Note-to-Self thread; peer DMs reply to the peer's ACI.
    fn reply_recipient(&self) -> Recipient {
        match self {
            Self::SelfSync => Recipient::SelfSync,
            Self::Peer { aci } => Recipient::Aci(aci.clone()),
        }
    }

    fn display(&self) -> String {
        match self {
            Self::SelfSync => "self-sync".to_string(),
            Self::Peer { aci } => format!("peer:{aci}"),
        }
    }
}

/// Privacy gate. Returns `Some(source)` for envelopes the transport will
/// process and `None` for everything else.
///
/// Two accept patterns:
/// - Note-to-Self: `Envelope::SyncMessage(SyncMessage::Sent {
///   destination: Some(Recipient::SelfSync), group_id: None, .. })`
/// - Allowed-sender peer DM: `Envelope::DataMessage {
///   source: Recipient::Aci(aci), group_id: None, .. }` where the ACI is
///   in `allowed_senders`.
///
/// Every other variant is rejected. The `_ => None` catch-all is the
/// default-deny posture against future signal-rs variants (Envelope and
/// Recipient are `#[non_exhaustive]`).
pub fn accepted_envelope(env: &Envelope, allowed_senders: &[String]) -> Option<AcceptedSource> {
    log::debug!(
        "signal::accepted_envelope: variant_check allowed_count={}",
        allowed_senders.len()
    );
    match env {
        Envelope::SyncMessage(SyncMessage::Sent {
            destination: Some(Recipient::SelfSync),
            group_id: None,
            ..
        }) => Some(AcceptedSource::SelfSync),
        Envelope::DataMessage {
            source: Recipient::Aci(aci),
            group_id: None,
            ..
        } if allowed_senders.iter().any(|allowed| allowed == aci) => Some(AcceptedSource::Peer { aci: aci.clone() }),
        _ => None,
    }
}

/// Output of `classify_signal_envelope`. `PartialMultiAttachment` exists
/// because Signal envelopes can carry several attachments at once but the
/// pipeline today processes one ContentKind per dispatch; we process the
/// first attachment and emit an explicit "Saved 1 of N attachments" ack
/// rather than silently dropping the rest.
#[derive(Debug, Clone)]
pub enum ClassifyOutcome {
    Single {
        kind: IntakeKind,
        preview: String,
    },
    PartialMultiAttachment {
        kind: IntakeKind,
        preview: String,
        dropped_count: usize,
        dropped_summary: Vec<String>,
    },
    Empty,
}

/// Classify a Signal envelope's body + attachment list into the same
/// `(IntakeKind, preview)` shape `classify_telegram_message` produces.
///
/// Resolution order (mirrors the design doc):
/// 1. Empty envelope -> `Empty`.
/// 2. Single attachment -> classify on `attachments[0]`.
/// 3. Multiple attachments -> classify on `attachments[0]` AND record the
///    dropped count + per-attachment summary for the partial-ack render.
/// 4. No attachment but body text -> Url (if extractable) or Text.
pub fn classify_signal_envelope(body: Option<&str>, attachments: &[AttachmentPointer]) -> ClassifyOutcome {
    let body = body.map(str::trim).filter(|s| !s.is_empty());
    log::debug!(
        "signal::classify_signal_envelope: body_present={} attachment_count={}",
        body.is_some(),
        attachments.len()
    );
    if attachments.is_empty() {
        return match body {
            None => ClassifyOutcome::Empty,
            Some(text) => match extract_url_from_text(text) {
                Some(url) => ClassifyOutcome::Single {
                    kind: IntakeKind::Url,
                    preview: url,
                },
                None => ClassifyOutcome::Single {
                    kind: IntakeKind::Text,
                    preview: intake_log::preview_text(text),
                },
            },
        };
    }

    let (kind, preview) = classify_attachment_intake(&attachments[0]);
    if attachments.len() == 1 {
        return ClassifyOutcome::Single { kind, preview };
    }

    let dropped: Vec<String> = attachments.iter().skip(1).map(attachment_descriptor).collect();
    ClassifyOutcome::PartialMultiAttachment {
        kind,
        preview,
        dropped_count: attachments.len() - 1,
        dropped_summary: dropped,
    }
}

fn attachment_descriptor(pointer: &AttachmentPointer) -> String {
    let filename = pointer.file_name.as_deref().unwrap_or("<unnamed>");
    let mime = pointer.content_type.as_deref().unwrap_or("application/octet-stream");
    format!("{filename} ({mime})")
}

fn classify_attachment_intake(pointer: &AttachmentPointer) -> (IntakeKind, String) {
    let filename = pointer
        .file_name
        .clone()
        .unwrap_or_else(|| synthesized_filename(pointer));
    let mime = pointer.content_type.as_deref();
    let size = pointer.size.unwrap_or(0) as usize;
    let kind = intake_kind_from_pointer(pointer);
    let preview = intake_log::binary_descriptor(kind, &filename, size, mime);
    (kind, preview)
}

fn intake_kind_from_pointer(pointer: &AttachmentPointer) -> IntakeKind {
    if pointer.voice_note {
        return IntakeKind::Voice;
    }
    if let Some(mime) = pointer.content_type.as_deref() {
        if mime.starts_with("image/") {
            return IntakeKind::Photo;
        }
        if mime == "application/pdf" {
            return IntakeKind::Document;
        }
        if mime.starts_with("audio/") {
            return IntakeKind::Audio;
        }
        if mime.starts_with("video/") {
            return IntakeKind::Video;
        }
    }
    if let Some(filename) = pointer.file_name.as_deref() {
        if assets::is_image_extension(filename) {
            return IntakeKind::Photo;
        }
        if assets::is_audio_extension(filename) {
            return IntakeKind::Audio;
        }
        if assets::is_pdf_extension(filename) || assets::is_document_extension(filename) {
            return IntakeKind::Document;
        }
    }
    IntakeKind::Document
}

fn synthesized_filename(pointer: &AttachmentPointer) -> String {
    let ext = match pointer.content_type.as_deref() {
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some("image/webp") => "webp",
        Some("image/gif") => "gif",
        Some("application/pdf") => "pdf",
        Some("audio/ogg") => "ogg",
        Some("audio/aac") => "aac",
        Some("audio/mpeg") => "mp3",
        Some("video/mp4") => "mp4",
        _ => "bin",
    };
    format!("signal-attachment-{}.{ext}", pointer.cdn_id)
}

/// Sliding-window rate gate over accepted Note-to-Self envelopes. Trips
/// fail-closed when the count over the configured window exceeds the
/// threshold. The trip persists for the lifetime of the process; resume
/// requires a daemon restart, by design (see Security in the design doc).
pub struct NoteToSelfRateGate {
    inner: Mutex<RateGateInner>,
    paused: AtomicBool,
    /// Latch so the tripped-gate alert is sent to Note-to-Self exactly once.
    /// Without it, every dropped envelope sent another outbound alert -
    /// unbounded alert spam in exactly the flood the gate guards against.
    alert_sent: AtomicBool,
    threshold: u32,
    window: Duration,
}

struct RateGateInner {
    timestamps: VecDeque<Instant>,
}

impl NoteToSelfRateGate {
    pub fn new(threshold: u32) -> Self {
        Self {
            inner: Mutex::new(RateGateInner {
                timestamps: VecDeque::new(),
            }),
            paused: AtomicBool::new(false),
            alert_sent: AtomicBool::new(false),
            threshold,
            window: RATE_GATE_WINDOW,
        }
    }

    /// Claim the one-shot alert slot. Returns `true` exactly once (the first
    /// caller after a trip); every later caller gets `false` so the outbound
    /// Note-to-Self alert is sent once, not once per dropped envelope.
    pub fn take_alert_slot(&self) -> bool {
        self.alert_sent
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    /// Record a SelfSync arrival and return `true` if the gate is still
    /// open, `false` if the gate just tripped or was already tripped.
    pub fn check_and_record(&self) -> bool {
        if self.paused.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        let mut inner = self.inner.lock().expect("rate-gate mutex poisoned");
        while let Some(front) = inner.timestamps.front() {
            if *front < cutoff {
                inner.timestamps.pop_front();
            } else {
                break;
            }
        }
        inner.timestamps.push_back(now);
        let count = inner.timestamps.len();
        if count > self.threshold as usize {
            self.paused.store(true, Ordering::Relaxed);
            log::error!(
                "signal::rate-gate TRIPPED: SelfSync envelopes={count} over window={:?} exceeded threshold={}; \
                 fail-closed pause until daemon restart",
                self.window,
                self.threshold
            );
            return false;
        }
        log::debug!(
            "signal::rate-gate: SelfSync count={count} threshold={} window={:?}",
            self.threshold,
            self.window
        );
        true
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    #[cfg(test)]
    pub fn reset(&self) {
        self.paused.store(false, Ordering::Relaxed);
        self.inner.lock().expect("rate-gate mutex poisoned").timestamps.clear();
    }
}

/// Open the linked state directory. Maps `NotLinked` / `PartiallyLinked` to
/// an operator-actionable error; transient open failures fall through to
/// the backoff loop.
async fn open_or_fail(state_dir: &Path) -> Result<Arc<Client>> {
    log::debug!("signal::open_or_fail: state_dir={}", state_dir.display());
    match Client::open(state_dir).await {
        Ok(client) => Ok(Arc::new(client)),
        Err(OpenError::NotLinked) => {
            eyre::bail!(
                "signal: state dir {} is not linked - run `signal-rs link --name borg --state-dir {}` first",
                state_dir.display(),
                state_dir.display(),
            )
        }
        Err(OpenError::PartiallyLinked) => {
            eyre::bail!(
                "signal: state dir {} is partially linked - re-run `signal-rs link --name borg --state-dir {}` to resume",
                state_dir.display(),
                state_dir.display(),
            )
        }
        Err(OpenError::Deauthorized) => {
            eyre::bail!(
                "signal: state dir {} is deauthorized (primary device removed the link) - re-run `signal-rs link --name borg --state-dir {}` to relink",
                state_dir.display(),
                state_dir.display(),
            )
        }
        Err(e) => Err(eyre::eyre!("signal: Client::open failed: {e}")),
    }
}

/// Decrypt one attachment into memory. Writes the plaintext to a tempfile
/// under the system tmp dir, reads it back, then unlinks. Caller owns the
/// returned (bytes, filename) tuple.
async fn download_signal_attachment(
    client: &Client,
    pointer: &AttachmentPointer,
    trace_id: &str,
) -> Result<(Vec<u8>, String)> {
    let filename = pointer
        .file_name
        .clone()
        .unwrap_or_else(|| synthesized_filename(pointer));
    log::debug!(
        "signal::download_signal_attachment: trace={trace_id} cdn_id={} filename={filename}",
        pointer.cdn_id
    );
    let tmp_path: PathBuf = std::env::temp_dir().join(format!(
        "{ATTACHMENT_TMP_PREFIX}{trace_id}-{}",
        sanitize_for_path(&filename)
    ));
    let download_result = client.download_attachment(pointer, &tmp_path).await;
    if let Err(e) = download_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(eyre::eyre!("signal: download_attachment failed: {e}"));
    }
    let bytes = match std::fs::read(&tmp_path) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(eyre::eyre!(
                "signal: failed to read decrypted attachment {}: {e}",
                tmp_path.display()
            ));
        }
    };
    if let Err(e) = std::fs::remove_file(&tmp_path) {
        log::debug!("signal::download_signal_attachment: tmp cleanup failed (non-fatal) trace={trace_id}: {e}");
    }
    Ok((bytes, filename))
}

fn sanitize_for_path(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn attachment_to_content(data: Vec<u8>, filename: String, pointer: &AttachmentPointer) -> ContentKind {
    let mime = pointer.content_type.as_deref();
    if let Some(mime) = mime {
        if mime.starts_with("image/") {
            return ContentKind::Image { data, filename };
        }
        if mime == "application/pdf" {
            return ContentKind::Pdf { data, filename };
        }
        if mime.starts_with("audio/") {
            return ContentKind::Audio { data, filename };
        }
    }
    if assets::is_image_extension(&filename) {
        return ContentKind::Image { data, filename };
    }
    if assets::is_pdf_extension(&filename) {
        return ContentKind::Pdf { data, filename };
    }
    if assets::is_audio_extension(&filename) {
        return ContentKind::Audio { data, filename };
    }
    ContentKind::Document { data, filename }
}

#[derive(Clone)]
struct DispatchEnv {
    config: Arc<Config>,
    client: Arc<Client>,
    notify_signal: notify::Signal,
    desktop: Option<notify::Desktop>,
    allowed_senders: Arc<Vec<String>>,
    rate_gate: Arc<NoteToSelfRateGate>,
}

async fn dispatch_envelope(env: Envelope, ctx: DispatchEnv) -> Result<()> {
    let Some(source) = accepted_envelope(&env, &ctx.allowed_senders) else {
        log::debug!("signal::dispatch_envelope: envelope rejected by privacy gate");
        return Ok(());
    };

    if matches!(source, AcceptedSource::SelfSync) && !ctx.rate_gate.check_and_record() {
        let trace_id = trace::generate(IngestMethod::Signal);
        log::error!(
            "signal::dispatch_envelope: SelfSync envelope dropped by tripped rate gate (trace={trace_id} threshold={})",
            ctx.rate_gate.threshold()
        );
        // Send the outbound alert ONCE per trip (latched). Every subsequent
        // dropped envelope only logs - otherwise the alert path itself floods
        // Note-to-Self during exactly the flood the gate exists to stop.
        if ctx.rate_gate.take_alert_slot() {
            let alert = format!(
                "intake-rate anomaly: Note-to-Self ingestion paused at >{}/hour; verify signal-rs has not regressed; restart the borg daemon after verifying",
                ctx.rate_gate.threshold()
            );
            let _ = ctx
                .notify_signal
                .processing(&trace_id, &alert, Some(&Recipient::SelfSync))
                .await;
        }
        return Ok(());
    }

    let (body, attachments) = match &env {
        Envelope::SyncMessage(SyncMessage::Sent { body, attachments, .. }) => (body.clone(), attachments.clone()),
        Envelope::DataMessage { body, attachments, .. } => (body.clone(), attachments.clone()),
        _ => return Ok(()),
    };

    let outcome = classify_signal_envelope(body.as_deref(), &attachments);
    let trace_id = trace::generate(IngestMethod::Signal);
    log::info!(
        "signal::dispatch_envelope: accepted source={} trace={trace_id} outcome_kind={}",
        source.display(),
        outcome_label(&outcome)
    );

    match &outcome {
        ClassifyOutcome::Empty => {
            log::debug!("signal::dispatch_envelope: empty envelope dropped trace={trace_id}");
            if let Err(e) = intake_log::record_received_with_sidecar(
                &ctx.config,
                IngestMethod::Signal,
                IntakeKind::Empty,
                "[empty]",
                b"[empty]",
                &trace_id,
            ) {
                log::warn!("signal::dispatch_envelope: failed to record empty intake trace={trace_id}: {e:#}");
            }
            intake_log::record_failure_at_door(
                IngestMethod::Signal,
                &trace_id,
                FailureStage::IntakeRejected,
                "empty Signal envelope (no body, no attachments)",
            );
            return Ok(());
        }
        ClassifyOutcome::Single { kind, preview } => {
            if let Err(e) = intake_log::record_received_with_sidecar(
                &ctx.config,
                IngestMethod::Signal,
                *kind,
                preview,
                preview.as_bytes(),
                &trace_id,
            ) {
                log::error!("signal::dispatch_envelope: failed to record intake trace={trace_id}: {e:#}");
                let _ = ctx
                    .notify_signal
                    .processing(
                        &trace_id,
                        &format!("borg failed to record your input: {e}"),
                        Some(&source.reply_recipient()),
                    )
                    .await;
                return Ok(());
            }
        }
        ClassifyOutcome::PartialMultiAttachment {
            kind,
            preview,
            dropped_count,
            dropped_summary,
        } => {
            log::warn!(
                "signal::dispatch_envelope: multi-attachment envelope trace={trace_id} processed=1 dropped={dropped_count}: {dropped_summary:?}"
            );
            if let Err(e) = intake_log::record_received_with_sidecar(
                &ctx.config,
                IngestMethod::Signal,
                *kind,
                preview,
                preview.as_bytes(),
                &trace_id,
            ) {
                log::error!(
                    "signal::dispatch_envelope: failed to record partial-attachment intake trace={trace_id}: {e:#}"
                );
                return Ok(());
            }
        }
    }

    let (kind_for_dispatch, content, display_source, extra_tags, partial_dropped) =
        match build_dispatch_payload(&ctx.client, &outcome, body.as_deref(), &attachments, &trace_id).await {
            Some(payload) => payload,
            None => {
                log::error!("signal::dispatch_envelope: payload build failed trace={trace_id}");
                intake_log::record_failure_at_door(
                    IngestMethod::Signal,
                    &trace_id,
                    FailureStage::FetchFailed,
                    "failed to materialise Signal payload",
                );
                let _ = ctx
                    .notify_signal
                    .processing(&trace_id, "failed to fetch attachment", Some(&source.reply_recipient()))
                    .await;
                return Ok(());
            }
        };

    let reply_recipient = source.reply_recipient();
    let processing_text = match kind_for_dispatch {
        IntakeKind::Url => "Processing URL...".to_string(),
        IntakeKind::Text => "Processing...".to_string(),
        IntakeKind::Photo => "Processing image...".to_string(),
        IntakeKind::Voice => "Processing voice note...".to_string(),
        IntakeKind::Audio => "Processing audio...".to_string(),
        IntakeKind::Document => "Processing document...".to_string(),
        _ => "Processing...".to_string(),
    };

    let notify_signal = ctx.notify_signal.clone();
    let desktop = ctx.desktop.clone();
    let config = Arc::clone(&ctx.config);
    let trace_for_pipeline = trace_id.clone();

    // signal-rs's `client.send` futures are !Send (libsignal-protocol's
    // storage uses `Rc<RefCell<...>>`), so the per-trace pipeline task that
    // also acks back via notify::Signal must stay on the LocalSet.
    tokio::task::spawn_local(async move {
        let prior_desktop = if let Some(d) = &desktop {
            d.processing(&trace_for_pipeline, &processing_text).await
        } else {
            None
        };
        let _ = notify_signal
            .processing(&trace_for_pipeline, &processing_text, Some(&reply_recipient))
            .await;

        let result = pipeline::process_content(
            content,
            extra_tags,
            IngestMethod::Signal,
            false,
            &config,
            Some(trace_for_pipeline.clone()),
        )
        .await;
        log::debug!(
            "signal::dispatch_envelope: pipeline returned trace={trace_for_pipeline} status={:?}",
            result.status
        );

        match partial_dropped {
            Some(dropped) => {
                notify_signal
                    .result_partial(&result, &display_source, dropped, Some(&reply_recipient))
                    .await;
            }
            None => {
                notify_signal
                    .result(&result, &display_source, Some(&reply_recipient))
                    .await;
            }
        }

        if let Some(d) = desktop {
            d.result(&result, &display_source, prior_desktop).await;
        }
    });

    Ok(())
}

async fn build_dispatch_payload(
    client: &Client,
    outcome: &ClassifyOutcome,
    body: Option<&str>,
    attachments: &[AttachmentPointer],
    trace_id: &str,
) -> Option<(IntakeKind, ContentKind, String, Vec<String>, Option<usize>)> {
    match outcome {
        ClassifyOutcome::Empty => None,
        ClassifyOutcome::Single { kind, .. } => match kind {
            IntakeKind::Url => {
                let text = body.unwrap_or("");
                let url = extract_url_from_text(text)?;
                let display_source = url.clone();
                Some((IntakeKind::Url, ContentKind::Url(url), display_source, vec![], None))
            }
            IntakeKind::Text => {
                let text = body.unwrap_or("").to_string();
                let display_source = display_for_text(&text);
                Some((IntakeKind::Text, ContentKind::Text(text), display_source, vec![], None))
            }
            _ => {
                let pointer = attachments.first()?;
                let (bytes, filename) = match download_signal_attachment(client, pointer, trace_id).await {
                    Ok(pair) => pair,
                    Err(e) => {
                        log::error!("signal::build_dispatch_payload: download failed trace={trace_id}: {e:#}");
                        return None;
                    }
                };
                let content = attachment_to_content(bytes, filename.clone(), pointer);
                let display_source = display_for_attachment(&content, &filename);
                let extra_tags = body
                    .and_then(|c| {
                        let trimmed = c.trim();
                        if trimmed.is_empty() { None } else { Some(vec![format!("caption:{trimmed}")]) }
                    })
                    .unwrap_or_default();
                Some((*kind, content, display_source, extra_tags, None))
            }
        },
        ClassifyOutcome::PartialMultiAttachment {
            kind, dropped_count, ..
        } => {
            let pointer = attachments.first()?;
            let (bytes, filename) = match download_signal_attachment(client, pointer, trace_id).await {
                Ok(pair) => pair,
                Err(e) => {
                    log::error!("signal::build_dispatch_payload: partial download failed trace={trace_id}: {e:#}");
                    return None;
                }
            };
            let content = attachment_to_content(bytes, filename.clone(), pointer);
            let display_source = display_for_attachment(&content, &filename);
            let extra_tags = body
                .and_then(|c| {
                    let trimmed = c.trim();
                    if trimmed.is_empty() { None } else { Some(vec![format!("caption:{trimmed}")]) }
                })
                .unwrap_or_default();
            Some((*kind, content, display_source, extra_tags, Some(*dropped_count)))
        }
    }
}

fn display_for_text(text: &str) -> String {
    vault::text::truncate_with_ellipsis(text, 50)
}

fn display_for_attachment(content: &ContentKind, filename: &str) -> String {
    let label = match content {
        ContentKind::Image { .. } => "image",
        ContentKind::Pdf { .. } => "pdf",
        ContentKind::Audio { .. } => "audio",
        ContentKind::Document { .. } => "document",
        ContentKind::Url(_) => "url",
        ContentKind::Text(_) => "text",
    };
    format!("[{label}: {filename}]")
}

fn outcome_label(outcome: &ClassifyOutcome) -> &'static str {
    match outcome {
        ClassifyOutcome::Empty => "empty",
        ClassifyOutcome::Single { kind, .. } => kind.as_str(),
        ClassifyOutcome::PartialMultiAttachment { kind, .. } => kind.as_str(),
    }
}

/// Entry point for the Signal transport. Mirrors `telegram::run`.
///
/// `state_dir` is the resolved canonical signal-state path
/// (`vault::paths::borg_signal_state_dir()`). It is supplied by the
/// daemon supervisor and never read from `SignalConfig` -- the path
/// is a borg implementation detail, not an operator-tunable config
/// field. See `docs/design/2026-05-24-signal-state-dir-internalization.md`.
/// Public probe for `sb doctor`: has a successful Signal cold-start bootstrap
/// been recorded for this identity? Resolves the marker path internally so the
/// caller (the `sb` crate) does not depend on borg's data-dir layout. A `false`
/// while linked means Note-to-Self ingest is not yet established.
pub fn bootstrap_recorded(account: &str, device_id: u32) -> bool {
    bootstrap::bootstrap_done(&vault::paths::borg_signal_bootstrap_marker(), account, device_id)
}

/// Cold-start self-ping. If this identity has not recorded a successful
/// bootstrap send, send one Note-to-Self so the phone establishes its outbound
/// sync session to us, then latch on success. Suppressed under the same gate as
/// every other borg Signal send. See `bootstrap.rs`.
async fn maybe_bootstrap_session(client: &Client, marker_path: &Path, account: &str, device_id: u32) {
    if notify::real_notifications_disabled() {
        log::debug!("signal: cold-start bootstrap suppressed (real notifications disabled)");
        return;
    }
    if bootstrap::bootstrap_done(marker_path, account, device_id) {
        log::debug!(
            "signal: cold-start bootstrap already recorded (account={account} device_id={device_id}); skipping"
        );
        return;
    }
    log::info!(
        "signal: cold-start bootstrap not recorded (account={account} device_id={device_id}); \
         sending one Note-to-Self to establish the phone->device sync session"
    );
    match client
        .send(Recipient::SelfSync, bootstrap::COLD_START_BOOTSTRAP_BODY)
        .await
    {
        Ok(sent_at_ms) => {
            bootstrap::record_bootstrap(marker_path, account, device_id, sent_at_ms);
            log::info!("signal: cold-start bootstrap self-ping sent ts={sent_at_ms}; latch recorded");
        }
        Err(e) => log::warn!(
            "signal: cold-start bootstrap self-ping failed: {e} (Note-to-Self ingest will not \
             work until this succeeds; it retries on next borg start, or run \
             `signal-rs send --to self`)"
        ),
    }
}

pub async fn run(
    signal_config: SignalConfig,
    state_dir: PathBuf,
    config: Arc<Config>,
    desktop: Option<notify::Desktop>,
) -> Result<()> {
    log::info!(
        "signal::run: state_dir={} host={} allowed_senders={} rate_threshold={}",
        state_dir.display(),
        signal_config.host,
        signal_config.allowed_senders.len(),
        signal_config.notetoself_rate_threshold_per_hour
    );

    let allowed_senders = Arc::new(signal_config.allowed_senders.clone());
    let rate_gate = Arc::new(NoteToSelfRateGate::new(
        signal_config.notetoself_rate_threshold_per_hour,
    ));

    let marker_path = vault::paths::borg_signal_bootstrap_marker();
    let mut bootstrap_attempted = false;
    let mut backoff = ExponentialBackoff::new();

    loop {
        let client = open_or_fail(&state_dir).await?;

        // Pre-flight: confirm we're still linked from the server's view.
        let status = match client.status().await {
            Ok(status) => {
                log::info!(
                    "signal: connected account={} device_id={} linked_devices={}",
                    status.account_number,
                    status.device_id,
                    status.linked_devices.len()
                );
                status
            }
            Err(e) => {
                log::warn!("signal: status pre-flight failed: {e}");
                backoff.wait().await;
                continue;
            }
        };
        let connected_at = std::time::Instant::now();

        // Cold-start bootstrap: a freshly-linked device receives no
        // Note-to-Self until it has sent once (the phone builds its outbound
        // sync session lazily). Attempt at most once per run(); the on-disk
        // latch keeps it idempotent across restarts. See bootstrap.rs and
        // docs/design/2026-05-28-signal-cold-start-bootstrap.md.
        if !bootstrap_attempted {
            bootstrap_attempted = true;
            maybe_bootstrap_session(&client, &marker_path, &status.account_number, status.device_id).await;
        }

        let notify_signal = match notify::Signal::new(Arc::clone(&client), &signal_config) {
            Some(s) => s,
            None => {
                log::error!("signal: notify::Signal::new returned None despite valid config");
                backoff.wait().await;
                continue;
            }
        };

        let mut rx = client.receive();
        let ctx_template = DispatchEnv {
            config: Arc::clone(&config),
            client: Arc::clone(&client),
            notify_signal: notify_signal.clone(),
            desktop: desktop.clone(),
            allowed_senders: Arc::clone(&allowed_senders),
            rate_gate: Arc::clone(&rate_gate),
        };

        // signal-rs's storage futures are !Send (libsignal-protocol's
        // `dyn SessionStore` is not Send); co-run the receive loop and the
        // recv consumer on the same task via tokio::select!. Per-envelope
        // dispatch is spawned with `spawn_local` so each envelope can
        // process its own attachments / notify acks without blocking
        // additional inbound envelopes.
        let loop_fut = client.run_receive_loop();
        tokio::pin!(loop_fut);
        let mut deauthorized = false;
        let mut should_reconnect = false;
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Ok(env) => {
                            let ctx = ctx_template.clone();
                            tokio::task::spawn_local(async move {
                                if let Err(e) = dispatch_envelope(env, ctx).await {
                                    log::error!("signal: dispatch failed: {e:#}");
                                }
                            });
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("signal: receive channel lagged by {n} envelopes");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            log::warn!("signal: receive channel closed; reconnecting");
                            should_reconnect = true;
                            break;
                        }
                    }
                    if rate_gate.is_paused() {
                        log::error!("signal: rate gate is paused; ingest stays paused until daemon restart");
                    }
                }
                res = &mut loop_fut => {
                    match res {
                        Ok(()) => {
                            log::info!("signal: receive loop returned Ok(()), reconnecting");
                            should_reconnect = true;
                        }
                        Err(ReceiveError::Deauthorized) => {
                            log::error!("signal: receive loop returned Deauthorized; relinking required");
                            deauthorized = true;
                        }
                        Err(e) => {
                            log::warn!("signal: receive loop returned err {e}, reconnecting");
                            should_reconnect = true;
                        }
                    }
                    break;
                }
            }
        }

        if deauthorized {
            // Fail-closed: bail out of the transport so the operator notices
            // via systemd's failed-unit signal.
            eyre::bail!(
                "signal: receive loop returned Deauthorized - the primary device removed this link; \
                 re-run `signal-rs link --name borg --state-dir {}` and restart the borg daemon",
                state_dir.display()
            );
        }
        if !should_reconnect {
            // Defensive: the select! arm should always set one of the two
            // flags. Treat an unset state as a transient anomaly and back
            // off rather than hot-looping.
            log::warn!("signal: select! exited without a typed disposition; treating as transient");
        }
        // Reset only after a sustained-healthy run; an immediate post-handshake
        // drop keeps the backoff growing instead of hot-looping at the base.
        backoff.reset_if_healthy(connected_at);
        backoff.wait().await;
    }
}
