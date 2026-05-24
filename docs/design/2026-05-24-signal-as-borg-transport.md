# Design Document: Signal as a borg Transport

**Author:** Scott Idler
**Date:** 2026-05-24
**Status:** Implemented
**Review Passes Completed:** 5/5 + Architect Rounds 1-2 (verified against borg/src/lib.rs and signal-rs/src/envelope.rs; six Round 1 findings applied, two Round 2 corrections applied: result_partial sink method for accurate partial-attachment ack, intake-rate anomaly gate promoted to mandatory v1)

## Summary

Add Signal as a second inbound + outbound transport for borg, peer to the existing Telegram path, consuming the in-process `signal-rs` v0.2.1 library that the parallel `scottidler/signal-rs` repo shipped specifically for this integration. Scope is full parity with the Telegram surface: inbound receive (Note-to-Self - the Signal-native private conversation with one's own account - plus allowlisted peer DMs), outbound notify sink for processing / Saved / Duplicate / Failed acks, intake_log + receipts.db participation, doctor checks, single-machine operation pinned by hostname. Bootstrap (the one-time QR scan that links borg as a Signal device) happens out of band via `signal-rs link`, just as Telegram bootstrap happens out of band via `@BotFather`; borg never wraps the link verb. A privacy-load-bearing Note-to-Self structural filter is enforced by a mandatory unit test so a future refactor cannot silently regress it into ingesting every outbound message the user sends from any linked device.

Terms used throughout: **ACI** is the Signal Account Identity (a UUID tied to the profile); **Note-to-Self** is the private conversation where `destination == own ACI`; **DLQ** is the dead-letter queue for failed pipeline stages, recorded against `receipts.db` and surfaced via `sb borg log`.

## Problem Statement

### Background

borg today has five inbound transports plus the manual cli path: telegram, discord, http, ntfy, clipboard. The daily driver is Telegram - the operator sends URLs and photos from the phone to a private bot conversation, the bot polls Telegram's cloud, borg's `telegram::run` dispatcher classifies and dispatches into the pipeline, the existing `notify::Telegram` sink acks back to the same conversation, and `notify::Desktop` mirrors the ack to the local desktop notification daemon. The shape is well-understood and the code is roughly 600 lines in `borg/src/telegram.rs` plus a `notify::Telegram` sink in `borg/src/notify.rs`.

Telegram works, but the architecture has properties the user has reasons to dilute: every Telegram message routes through Telegram's servers (the privacy boundary belongs to a third party that controls the message queue, the bot identity, and the routing fabric); the inbound identity is a bot rather than a self-account (a structural choice Telegram requires); and outages or rate-limit incidents at Telegram propagate directly into borg's inbound path. Signal is the obvious alternative: end-to-end-encrypted envelope routing only, no bot concept, no third-party-controlled identity, no central queue with privileged access to plaintext.

The reference doc at `docs/signal-as-borg-transport.md` lays out the option space - Shape A (out-of-process daemon, e.g. `signal-cli` as a long-lived child) vs Shape B (in-process library); client implementation menu; the conceptual gap between "Telegram hosts your client" and "Signal makes you be the client". `scottidler/signal-rs` was built explicitly as the Shape B consumer-ready library; v0.2.1 is the first release validated against a real Signal account, with the public API surface documented in `docs/signal-rs-consumer-integration-handoff.md`. That handoff doc includes a structural map of `borg/src/telegram.rs` onto its Signal equivalent, a starter stub for `borg/src/signal.rs`, and an enumeration of upstream gaps with explicit justification for each one as "does not block borg's Note-to-Self use case".

### Problem

borg needs a Signal transport that achieves operational parity with Telegram (the user does not want a half-Signal that handles inbound but not acks, or that requires a separate manual ingest workflow), while preserving the structural Note-to-Self filter that distinguishes the privacy-load-bearing "messages I sent to myself" pattern from the much wider "every outbound message I sent from any linked device" stream. Without that filter, borg would ingest the user's entire outbound conversation history - replies to friends, group chats, work conversations - because Signal's `SyncMessage::Sent` fan-out includes them all by default. The filter is enforced inside `signal-rs` (it knows our own ACI from the linked state and remaps matching destinations to the typed `Recipient::SelfSync` variant), but borg's code still has to apply the pattern match correctly, and a regression there is privacy-relevant, not just functional.

The transport also needs to integrate without disturbing the parts of borg that are already transport-agnostic: `intake::record_intake` and `record_intake_with_sidecar` accept any `IngestMethod`, the receipts.db schema's `method` column is enum-driven, the pipeline dispatches on `ContentKind` rather than transport-of-origin, and `borg-ledger.md` rendering picks up new methods automatically. The Signal path should plug into those interfaces with the same shape Telegram uses, not bypass or special-case them.

Three secondary problems sit alongside the main one. First, `sb doctor` today does not have a `telegram` section despite Telegram being borg's daily driver - any link-health surface introduced for Signal should bring a parity catch-up for Telegram. Second, signal-rs's CLI defaults the linked state directory to `~/.local/share/signal-rs/`, which would collide with any operator running ad-hoc `signal-rs receive` for debugging and desync the Double Ratchet against borg's live session; borg needs to default to a distinct path under its own data root. Third, Signal-Server fans Note-to-Self envelopes out to every linked device, so a multi-machine borg install would ingest the same message twice without a single-machine pin equivalent to Telegram's API-level polling lock.

### Goals

- **Inbound parity.** Note-to-Self envelopes (`Envelope::SyncMessage(SyncMessage::Sent { destination: Some(Recipient::SelfSync), group_id: None, .. })`) and peer DM envelopes (`Envelope::DataMessage { source: <ACI in allowed_senders>, group_id: None, .. }`) feed `pipeline::process_content` with the same `ContentKind` values Telegram produces. Same classifier logic, same DLQ stages, same trace_id propagation. The `group_id: None` requirement on both filters keeps group-context traffic out of borg, since signal-rs surfaces group traffic through the same `DataMessage` / `Sent` variants with `group_id: Some(..)`.
- **Outbound parity.** A `notify::Signal` sink, peer of `notify::Telegram` and `notify::Desktop`, sends Processing / Saved / Duplicate / Failed acks back to the inbound source (SelfSync for Note-to-Self, peer ACI for allowed-sender DMs). Cross-method notifications (e.g., an ntfy ingest acknowledged via Signal) route to a `notification_recipient` config field, defaulting to SelfSync.
- **Out-of-band bootstrap.** The one-time `signal-rs link --name borg --state-dir <path>` step happens entirely outside borg. borg's job at startup is to call `Client::open` and surface `OpenError::NotLinked` with an operator-actionable message naming the exact `signal-rs link` invocation. This treats `signal-rs` the way Telegram treats `@BotFather`: identity provisioning is upstream's concern.
- **Privacy-load-bearing tests.** A mandatory test suite asserts the structural filter. Positive direction: `Envelope::SyncMessage(SyncMessage::Sent { destination: Some(SelfSync), group_id: None, .. })` and `Envelope::DataMessage { source: <allowed ACI>, group_id: None, .. }` are accepted. Negative direction: same envelopes with `destination: Some(Aci(other))`, `destination: None`, `group_id: Some(..)`, or `source: Recipient::Pni(..)` are rejected. The tests lock the pattern match against borg-side regressions; the upstream `signal-rs` wire-ACI to typed-variant mapping is a separate boundary out of borg's reach (covered in Security and the Risks table).
- **State directory isolation.** Default state dir is `~/.local/share/sb/borg/signal-state/`, under sb's own data root. The bootstrap template documents this; the `signal` doctor section detects when state_dir overlaps `~/.local/share/signal-rs/`.
- **Single-machine pin.** A `signal.host: String` config field (mandatory, validated at config-load) declares the single hostname that runs Signal ingest. The daemon supervisor in `borg/src/lib.rs` gates spawning of `signal::run` via the same `config::is_local_host` helper Telegram, Discord, and ntfy use. The asymmetry with `telegram.host: Option<String>` is intentional: Telegram's polling API enforces single-machine server-side, Signal does not, so borg fails closed when the operator omits `host`.
- **Doctor parity.** Add a `signal` section to `sb/src/cli/checks.rs::all_sections` and a parity `telegram` section that does not exist today. Both report config presence, link/auth health, host constraint, and (signal-specific) state_dir existence and uniqueness.
- **`IngestMethod::Signal` first-class.** Receipts.db rows, intake_log markdown entries, borg-ledger.md aggregation, and `sb borg log --method signal` queries route through the same enum-driven paths as Telegram. No special-casing.

### Non-Goals

- **Wrapping `signal-rs link` as an sb subcommand.** The CLI exists upstream for this exact purpose; the borg-specific work that justifies the `sb borg extension {sign, install, ...}` wrapping pattern (manifest synthesis from `env!`, install policy, idempotency tracking) has no equivalent here. State_dir path discipline is handled at config-validation time and reported via the doctor section, not via CLI wrapping.
- **Multi-machine concurrent Signal ingest.** Option B (receipts-level dedupe across machines via a `(method, sent_timestamp, source_aci)` uniqueness key) is captured under Alternatives Considered but not chosen. First cut is sole-machine via `signal.host`.
- **Group messages, typing indicators, edits, deletions, ephemeral messages.** Every one of these maps onto an `Envelope` variant signal-rs already decodes, but none is needed for the Note-to-Self + allowed-sender DM use case, and each adds classifier complexity and DLQ edge cases.
- **Outbound delivery / read receipts to peers.** signal-rs's v0.2.1 surface does not include `Client::read_receipt`; the omission is acknowledged in the handoff doc and does not block borg because borg's outbound is to `Recipient::SelfSync` (yourself - no peer to send indicators to) or to allowed-sender peers (where receipt indicators are a UX nicety the user has not asked for).
- **MismatchedDevices retry on peer fan-out.** signal-rs's v0.2.1 surface does not handle Signal-Server's HTTP 409 retry-with-corrected-device-list response. The handoff doc justifies skipping it for borg: Note-to-Self own-device sync uses a different code path that exchanges device lists at WebSocket connect time. Allowed-sender peer sends are infrequent (acks only); a one-off MismatchedDevices failure surfaces as a `SendError::Server`, the existing DLQ path captures it, and the operator can request a peer redelivery if it matters.
- **PNI receive smoke.** PNI traffic only arrives from strangers initiating contact; the Note-to-Self filter and `allowed_senders` allowlist drop it before classification. signal-rs's PNI receive path has unit tests but no real-device smoke, per the handoff doc; the gap does not affect borg.
- **`SyncMessage::Contacts` consumption.** signal-rs does not decode the encrypted address book blob the primary phone sends down at link time. The consequence is that signal-rs has no peer profile keys until it receives a `DataMessage` from each peer (carrying inline `profile_key`). Outbound sealed-sender to peers without profile keys falls back to unsealed. For borg this matters only on the first ack to a new allowed-sender peer; sealed-sender to SelfSync always works (we have our own profile key from the provision message). Acceptable tradeoff.
- **`signal-rs` crates.io publish.** Git-tag pin (`tag = "v0.2.1"`) is the intended consumer shape per the handoff doc; crates.io polish is upstream's call, not a prerequisite for borg.
- **Migration off Telegram.** Telegram remains the daily driver. Signal joins as a peer transport. No deprecation, no config migration, no removal.

## Proposed Solution

### Overview

1. **`borg/src/signal.rs` is a flat sibling of `telegram.rs`.** One public entry point: `pub async fn run(signal_config: SignalConfig, config: Arc<Config>, desktop: Option<notify::Desktop>) -> Result<()>`. The outer `ExponentialBackoff` loop wraps `Client::open` -> spawn `client.run_receive_loop()` -> consume `client.receive()` envelopes. This is the same shape as `telegram::run`'s outer-loop-around-Dispatcher pattern.

2. **`Client` is shared across tasks via outer `Arc`.** signal-rs's `Client` is internally `Arc<ClientInner>` but does not derive `Clone`. The receive loop and the `notify::Signal` sink both hold `Arc<Client>`; one task drives `run_receive_loop()` to keep the WebSocket alive, the consumer task pulls from `client.receive()`, and the per-envelope dispatch hands the `Arc` to `notify::Signal` for the ack.

3. **The Note-to-Self filter is the privacy gate, separate from `allowed_senders`.** A private helper `accepted_envelope(env, allowed_senders) -> Option<AcceptedSource>` returns `Some(AcceptedSource::SelfSync)` for the structural Note-to-Self pattern (`Sent { destination: Some(SelfSync), group_id: None, .. }`), `Some(AcceptedSource::Peer { aci })` for a peer DM whose ACI is in `allowed_senders` AND has `group_id: None`, and `None` for everything else (peer DMs from non-allowed ACIs, any envelope with `group_id: Some(..)`, `SyncMessage::Read`, `Receipt`, `Typing`, `Edit`, `Call`, `Unknown`, future signal-rs variants). The Note-to-Self filter never consults `allowed_senders` - your own ACI is privileged structurally, not by membership. The `group_id` check on both filter arms is mandatory because `signal_rs::envelope::Envelope::DataMessage` and `SyncMessage::Sent` both carry a `group_id: Option<Vec<u8>>` field; the variant alone does not distinguish 1:1 from group traffic.

4. **`SignalConfig` hangs off `Config` as `Option<SignalConfig>`.** Presence of the section enables the transport, matching the existing pattern for `telegram`, `discord`, `ntfy`, `desktop`. No `enabled: bool`. Fields: `state_dir: PathBuf`, `allowed_senders: Vec<String>`, `notification_recipient: Option<String>`, `host: String` (mandatory when `signal:` is present, validated at config-load).

5. **`notify::Signal` mirrors `notify::Telegram` exactly.** Same constructor shape (`Option<Self>`), same `.processing()` and `.result()` methods with the same timeout discipline, same `resolve_recipient(override) -> Recipient` helper. Reply routing: SelfSync for Note-to-Self, `Recipient::Aci(source)` for allowed-sender peer DMs, `notification_recipient` (defaulting to SelfSync) for cross-method notifications.

6. **`IngestMethod::Signal` is added to the enum.** `borg::types::IngestMethod` gains a `Signal` variant; `fmt::Display` gains `Self::Signal => "signal"`; the `From<IngestMethod> for vault::schema::Method` impl gains the matching arm. `vault::schema::Method` (upstream of borg in the `vault` crate) gains a `Signal` variant first.

7. **`sb doctor` gets `signal` and `telegram` sections.** Two new `Section`s in `sb/src/cli/checks.rs::all_sections()`. `signal_findings()`: config presence, state_dir exists, `Client::open` succeeds, state_dir is not signal-rs's CLI default, host matches gethostname, `client.status()` reports linked-device info. `telegram_findings()` (parity catch-up): config presence, bot_token non-empty, `bot.get_me()` succeeds, host matches gethostname.

8. **Bootstrap is documented, not automated.** `config/templates/borg.yml.template` ships a commented-out `signal:` block with the inline runbook (`signal-rs link --name borg --state-dir ~/.local/share/sb/borg/signal-state/`). `sb bootstrap` continues to drop the template; the operator uncomments the block, runs the link command once, restarts the borg systemd unit. No `sb bootstrap --signal` flag (the extension precedent does not apply here; see Alternatives 1).

### Architecture

```
borg's existing transports                  new Signal path
+--------------------+                      +-------------------+
| telegram.rs::run   |                      | signal.rs::run    |
|  teloxide::Bot     |                      |  signal_rs::      |
|  Dispatcher        |                      |    Client (Arc)   |
|  allowed_chat_ids  |                      |  Note-to-Self     |
|                    |                      |    filter +       |
|                    |                      |  allowed_senders  |
+----------+---------+                      +---------+---------+
           |                                          |
           v                                          v
+--------------------------------------------------------------+
|  classify -> intake::record_intake -> pipeline::             |
|  process_content -> notify (Telegram | Signal | Desktop)     |
+--------------------------------------------------------------+
```

Everything below the transport-envelope row (intake_log, pipeline, classify_document, ContentKind dispatch, trace_id propagation, DLQ stages, receipts.db) is transport-agnostic and reused unchanged. Only the I/O envelope at the top is new code.

### Structural Map: `telegram.rs` -> `signal.rs`

| `telegram.rs` element | `signal.rs` equivalent |
|---|---|
| `teloxide::Bot::new(&token)` | `signal_rs::Client::open(&state_dir).await` |
| `bot.get_me()` pre-flight | `client.status().await` pre-flight (and as the doctor check) |
| `claim_polling_session()` (Telegram concurrency lock) | Not needed; Signal-Server enforces one auth WebSocket per device id |
| `Update::filter_message().endpoint(...)` (teloxide Dispatcher) | `let mut rx = client.receive(); while let Ok(env) = rx.recv().await { ... }` with `run_receive_loop` in a sibling task |
| `allowed_chat_ids` filter | Structural Note-to-Self pattern match + `allowed_senders` allowlist (two filters, distinct purposes) |
| `classify_telegram_message(&msg)` | `classify_signal_envelope(body, attachments)` returning the same `(IntakeKind, preview)` shape |
| `download_telegram_file(bot, file_id)` | `client.download_attachment(&pointer, &tempfile).await` then `std::fs::read(tempfile)` |
| `bot.send_message(chat_id, ack)` | `client.send(recipient, ack).await` (recipient = `SelfSync` or `Aci`) |
| Outer `ExponentialBackoff` around dispatch | Same shape, around `Client::open` + spawn `run_receive_loop` + receive loop |

### Data Model

#### `SignalConfig` (new)

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SignalConfig {
    /// Directory holding the linked state from
    /// `signal-rs link --state-dir <here>`. The bootstrap template
    /// defaults to `~/.local/share/sb/borg/signal-state/`; the field
    /// is mandatory in config (no implicit default at deserialize time
    /// because a misconfigured path could silently collide with
    /// signal-rs's CLI default).
    pub state_dir: PathBuf,

    /// ACI UUIDs (string form) allowed to send borg peer DMs.
    /// Note-to-Self is structural and never gated by this list - the
    /// `Recipient::SelfSync` filter is separate.
    #[serde(default)]
    pub allowed_senders: Vec<String>,

    /// Default reply target for cross-method notifications (e.g., an
    /// ntfy ingest acknowledged via Signal). `None` = `SelfSync`;
    /// `Some(<ACI UUID>)` = peer.
    #[serde(default)]
    pub notification_recipient: Option<String>,

    /// Pin Signal ingest to a specific hostname. **Mandatory** when
    /// the `signal:` block is present (no `Option`), because Signal-Server
    /// fans out Note-to-Self envelopes to every linked device and there
    /// is no server-side polling lock equivalent to Telegram's
    /// `TerminatedByOtherGetUpdates`. Leaving this unset on a multi-machine
    /// install would silently double-ingest. The asymmetry with
    /// `TelegramConfig::host` (which is `Option<String>`) is intentional:
    /// Telegram's API enforces single-machine via the polling lock, Signal
    /// does not, so borg enforces it here at config-load time.
    pub host: String,

    /// Maximum accepted Note-to-Self envelopes per hour before the rate
    /// gate trips and pauses ingest until the daemon is restarted. Default
    /// is deliberately well above the typical single-digits/day pattern
    /// documented in the reference doc, so legitimate bursts (e.g., a
    /// session of 20-30 rapid pastes) do not trip. Allowlisted peer DMs
    /// are NOT counted - the gate exists to backstop upstream `signal-rs`
    /// regressions in the wire-ACI to `Recipient::SelfSync` mapping
    /// (privacy-load-bearing). See the Security section.
    #[serde(default = "default_signal_rate_threshold")]
    pub notetoself_rate_threshold_per_hour: u32,
}

fn default_signal_rate_threshold() -> u32 { 100 }
```

Wired into `Config` at `borg/src/config.rs:150`-ish (alongside `telegram`, `discord`, `ntfy`):

```rust
pub signal: Option<SignalConfig>,
```

YAML shape after the bootstrap template renders:

```yaml
signal:
  state-dir: ~/.local/share/sb/borg/signal-state/
  allowed-senders:
    - "00000000-0000-0000-0000-000000000000"  # peer ACI (uncomment + replace)
  # notification-recipient defaults to Note-to-Self (your own account)
  host: home-server  # MANDATORY: hostname (output of `hostname`) of the machine that runs Signal ingest
  # notetoself-rate-threshold-per-hour: 100  # default; raise only after a documented baseline reason
```

`host` is a required field. Config-load fails with a clear error if it is unset or empty. `host` MUST match `gethostname` exactly on the machine that should ingest; every other machine running borg with this config sees the mismatch and skips Signal startup (the daemon supervisor short-circuits before spawning `signal::run`).

#### `IngestMethod::Signal` (new variant)

`borg/src/types.rs:205`:

```rust
pub enum IngestMethod {
    Telegram,
    Discord,
    Http,
    Clipboard,
    Cli,
    Ntfy,
    Signal,  // new
}
```

Add the `fmt::Display` arm:

```rust
Self::Signal => write!(f, "signal"),
```

And the `From<IngestMethod> for vault::schema::Method` arm:

```rust
IngestMethod::Signal => Self::Signal,
```

Requires a corresponding new `Method::Signal` variant in `vault/src/schema.rs` (the canonical enum); downstream consumers that match `Method` exhaustively need a matching arm.

#### `ContentKind` (unchanged)

Signal produces the same `ContentKind` variants Telegram already produces: `Url(String)`, `Image { data, filename }`, `Pdf { data, filename }`, `Audio { data, filename }`, `Text(String)`, `Document { data, filename }`. The pipeline does not learn anything new about Signal at the `ContentKind` boundary.

### API Design

#### `borg/src/signal.rs` public surface

```rust
pub async fn run(
    signal_config: SignalConfig,
    config: Arc<Config>,
    desktop: Option<notify::Desktop>,
) -> Result<()>
```

The signature does not take a `notify::Signal` parameter. signal.rs builds its own `Arc<Client>` and constructs the `notify::Signal` sink internally, because the receive loop and the reply path must share the same underlying client (per the handoff doc - signal-rs's `Client` is `Arc<ClientInner>` but not `Clone`, so an outer `Arc` is the share mechanism).

Private helpers, in dataflow order:

```rust
async fn open_or_fail(state_dir: &Path) -> Result<Arc<Client>>;
// Maps OpenError::NotLinked / PartiallyLinked to an eyre::bail! carrying
// the operator-actionable `signal-rs link --name borg --state-dir <path>`
// command. NOT retried by the outer backoff; the operator has to act.

enum AcceptedSource {
    SelfSync,
    Peer { aci: String },
}

fn accepted_envelope(
    env: &Envelope,
    allowed_senders: &[String],
) -> Option<AcceptedSource>;
// The privacy gate. Both accepted patterns additionally require
// group_id.is_none() because signal-rs's Envelope::DataMessage and
// SyncMessage::Sent both carry an Option<Vec<u8>> group_id field; group
// traffic shares the variants with 1:1 traffic and is distinguished
// only by this field. A naive source/destination match without the
// group_id check would accept group chatter.
//
// Matches Envelope::SyncMessage(SyncMessage::Sent {
//   destination: Some(Recipient::SelfSync),
//   group_id: None,
//   ..
// }) -> SelfSync.
//
// Matches Envelope::DataMessage {
//   source: Recipient::Aci(s),
//   group_id: None,
//   ..
// } where allowed_senders.contains(&s) -> Peer { aci: s.clone() }.
//
// All other variants and shapes -> None. Envelope and Recipient are
// #[non_exhaustive] in signal-rs; the `_ => None` catch-all arm is the
// intentional default-deny posture so future signal-rs additions (new
// sync variants, new envelope sub-messages) do not silently start being
// accepted. A new accepted variant requires an explicit code change here
// AND a corresponding test in Phase 6.
//
// Defense-in-depth limitation: this filter validates borg's pattern
// match given a typed envelope; it does NOT validate signal-rs's
// translation from the on-wire ACI to the typed Recipient::SelfSync
// variant. That boundary is upstream's responsibility. If signal-rs
// regressed to mapping every outbound destination to SelfSync, borg's
// tests would stay green while production silently ingested the user's
// entire outbound conversation history. Mitigations: (a) intake-rate
// anomaly observation against receipts.db serves as a runtime backstop
// (a sudden 10x in method=signal volume is the visible failure
// signature); (b) the Phase 6 test suite includes negative cases that
// catch borg-side pattern-match loosening (Sent with destination=Some(Aci),
// Sent with group_id=Some, DataMessage with group_id=Some); (c) the
// upstream signal-rs project owns the wire-to-typed mapping and its own
// test coverage on that boundary.

fn classify_signal_envelope(
    body: Option<&str>,
    attachments: &[AttachmentPointer],
) -> ClassifyOutcome;

enum ClassifyOutcome {
    Single { kind: IntakeKind, preview: String },
    PartialMultiAttachment {
        // The first attachment is processed; the rest are logged and
        // dropped. The dispatch path uses the dropped count to render
        // an accurate "Saved 1 of N attachments" ack instead of a
        // misleading "Saved" generic ack.
        kind: IntakeKind,
        preview: String,
        dropped_count: usize,
        dropped_summary: Vec<String>,  // content_type + filename of each dropped attachment, for the log line
    },
    Empty,
}
// Resolution order:
// 1. If attachments.is_empty() and body is None or empty:
//    ClassifyOutcome::Empty (caller routes to intake_log with
//    IntakeKind::Empty and a "<empty envelope>" preview; no pipeline
//    dispatch).
// 2. If attachments.len() == 1: classify on attachments[0]. Branch on
//    content_type ("image/*", "audio/*", "application/pdf", document
//    MIMEs); fall back to filename-extension detection via the existing
//    assets::is_*_extension helpers (same fallback chain Telegram uses
//    in classify_document). Returns ClassifyOutcome::Single.
// 3. If attachments.len() > 1: classify on attachments[0] only AND
//    return ClassifyOutcome::PartialMultiAttachment with the dropped
//    metadata. The caller's notify::Signal::result emits an explicit
//    "Saved 1 of N attachments; multi-attachment support is v2" ack so
//    the user sees that not all content was ingested. The dropped
//    attachments are logged at warn level with content_type + filename
//    for forensic visibility. Full multi-attachment support is open
//    question 4.
// 4. If attachments.is_empty() and body has content: try
//    router::extract_url_from_text first (IntakeKind::Url) or fall back
//    to IntakeKind::Text. Returns ClassifyOutcome::Single.

async fn download_signal_attachment(
    client: &Client,
    pointer: &AttachmentPointer,
) -> Result<(Vec<u8>, String)>;
// Wraps client.download_attachment with a tempfile dest, returns
// (decrypted_bytes, filename). Filename comes from pointer.file_name
// when present, falls back to a synthesized "signal-<kind>.<ext>" name.

async fn dispatch_envelope(
    client: Arc<Client>,
    env: Envelope,
    config: Arc<Config>,
    notify_signal: Arc<notify::Signal>,
    desktop: Option<notify::Desktop>,
    allowed_senders: Arc<Vec<String>>,
) -> Result<()>;
// The per-envelope work: accepted_envelope -> classify -> trace_id ->
// intake::record_intake -> pipeline::process_content -> notify_signal
// + notify_desktop. Returns Err for surfaced DLQ stages; the receive
// loop logs and continues.
```

The receive-loop body inside `run`:

```rust
let client = open_or_fail(&signal_config.state_dir).await?;
let notify_signal = Arc::new(
    notify::Signal::new(Arc::clone(&client), &signal_config)
        .expect("notify::Signal::new returns Some for any valid config")
);
let allowed = Arc::new(signal_config.allowed_senders.clone());

let receive_client = Arc::clone(&client);
tokio::spawn(async move {
    if let Err(e) = receive_client.run_receive_loop().await {
        log::error!("signal: receive loop exited: {e}");
    }
});

let mut rx = client.receive();
loop {
    match rx.recv().await {
        Ok(env) => {
            let task_client = Arc::clone(&client);
            let task_config = Arc::clone(&config);
            let task_notify = Arc::clone(&notify_signal);
            let task_desktop = desktop.clone();
            let task_allowed = Arc::clone(&allowed);
            tokio::spawn(async move {
                if let Err(e) = dispatch_envelope(
                    task_client, env, task_config, task_notify,
                    task_desktop, task_allowed,
                ).await {
                    log::error!("signal: dispatch failed: {e}");
                }
            });
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            log::warn!("signal: receive lagged {n} envelopes");
        }
        Err(broadcast::error::RecvError::Closed) => {
            log::warn!("signal: receive channel closed; reconnecting");
            break;  // outer backoff loop reconnects
        }
    }
}
```

#### `notify::Signal`

```rust
#[derive(Clone)]
pub struct Signal {
    client: Arc<signal_rs::Client>,
    default_recipient: Recipient,
}

impl Signal {
    pub fn new(client: Arc<Client>, signal_config: &SignalConfig) -> Option<Self> {
        let default_recipient = match &signal_config.notification_recipient {
            None => Recipient::SelfSync,
            Some(aci) => Recipient::Aci(aci.clone()),
        };
        log::info!("notify: Signal notifications enabled (default={default_recipient:?})");
        Some(Self { client, default_recipient })
    }

    fn resolve_recipient(&self, override_recipient: Option<&Recipient>) -> Recipient {
        override_recipient.cloned().unwrap_or_else(|| self.default_recipient.clone())
    }

    pub async fn processing(
        &self, trace_id: &str, description: &str,
        override_recipient: Option<&Recipient>,
    ) -> Result<(), ()> { /* same timeout shape as notify::Telegram */ }

    pub async fn result(
        &self, result: &IngestResult, display_source: &str,
        override_recipient: Option<&Recipient>,
    ) { /* same timeout shape as notify::Telegram */ }

    /// Signal-specific partial-attachment ack. Called by `dispatch_envelope`
    /// when `classify_signal_envelope` returns `ClassifyOutcome::PartialMultiAttachment`.
    /// Renders an explicit "Saved 1 of {1 + dropped_count} attachments
    /// (multi-attachment support is v2)" body instead of `notify::Telegram`-shape
    /// generic "Saved" so the user knows not all content was ingested.
    /// Isolated to the Signal sink because partial-attachment is a Signal-only
    /// concern (Telegram is one-media-per-message structurally); the
    /// transport-agnostic `IngestResult` struct is not widened with a
    /// Signal-only field.
    pub async fn result_partial(
        &self, result: &IngestResult, display_source: &str,
        dropped_count: usize,
        override_recipient: Option<&Recipient>,
    ) { /* same timeout shape; appends partial-context to the standard body */ }
}
```

The constructor returns `Option<Self>` to match `notify::Telegram::new`'s shape, even though it always succeeds today (`SelfSync` is always a valid default). The Option leaves a return-None path open for future config-validation failures without a signature change.

#### `sb` doctor sections

Both new sections plug into `sb/src/cli/checks.rs::all_sections()`:

```rust
Section { name: "signal", findings: signal_findings() },
Section { name: "telegram", findings: telegram_findings() },
```

`signal_findings()` is host-gated: when `signal.host` is set and does not match `gethostname`, the section short-circuits at the host comparison and skips state_dir / link checks (the laptop is not expected to have a linked state on disk). Findings, in evaluation order:

- "signal not configured" (Info) when `Config::signal` is `None`. Section returns here.
- "host=<configured> hostname=<actual> (this machine does not run Signal ingest)" (Info) when host is set and mismatched. Section returns here - the remaining checks are not meaningful on a non-Signal host.
- "state_dir exists" (Ok) / "state_dir does not exist: <path>" (Error) - includes suggested fix "create the dir or run signal-rs link --state-dir <path>".
- "state_dir collides with signal-rs CLI default (`~/.local/share/signal-rs/`)" (Warn) - includes suggested fix "use a borg-owned path like `~/.local/share/sb/borg/signal-state/`". The comparison resolves symlinks via `std::fs::canonicalize` before comparing, so a symlink-into-the-default is caught.
- "linked as <aci> device_id=<n> (linked devices: <n>)" (Ok) - from `Client::open` + `client.status()`.
- "NotLinked / PartiallyLinked" (Error) - includes suggested fix `signal-rs link --name borg --state-dir <path>`.

`telegram_findings()` (parity catch-up, equivalent shape):

- "telegram not configured" (Info) when `Config::telegram` is `None`.
- "bot_token configured" (Ok) / "bot_token missing or empty" (Error).
- "bot.get_me succeeded: @<username>" (Ok) / "bot.get_me failed: <err>" (Error).
- "host=<configured> hostname=<actual>" (Info) when host is set and mismatched.

### Implementation Plan

The seven phases below describe code work, not release gates. The entire integration ships in a single `bump`-and-`otto deploy` cycle. There is no soak time between phases and no evidence-gated cutover - phases are a structural ordering of the implementation, not a release sequence.

**Model assignment summary:** Phases 1, 3, 4, 5, 7 are sonnet (mechanical scaffolding, sink mirroring, intake/receipts wiring, doctor sections, docs). Phases 2 and 6 are opus (the privacy-load-bearing core: receive loop + classify + dispatch with the Note-to-Self filter, and the tests that lock it in).

#### Phase 1: Cargo + vault::schema + types + config scaffolding
**Model:** sonnet

- `borg/Cargo.toml`: add `signal-rs = { git = "https://github.com/scottidler/signal-rs", tag = "v0.2.1" }`. Run `cargo add` to get latest from-tag dependency.
- `vault/src/schema.rs`: add `Method::Signal` variant; update any exhaustive matches on `Method` (Display, serde, JsonSchema derives).
- `borg/src/types.rs`: add `IngestMethod::Signal` variant. Update `fmt::Display` and `From<IngestMethod> for vault::schema::Method`.
- `borg/src/config.rs`: add `SignalConfig` struct (kebab-case serde rename, all four fields). Add `pub signal: Option<SignalConfig>` to `Config`. Add round-trip unit tests mirroring the existing Telegram config tests. Resolve `state_dir` to an absolute path at config-load time (via `dunce::canonicalize` or equivalent) so the value is stable across CWD changes between startup and reconnect. `host: String` is mandatory (no `Option`); add a `Config::validate_signal()` hook called from `Config::load()` that returns `Err` when `signal.is_some()` and `signal.host.is_empty()`, with a clear message naming the missing field. This is the structural defense against multi-machine dup ingest the Architect's design review flagged.
- `config/templates/borg.yml.template`: add a commented-out `signal:` block with inline runbook comment (`# Run `signal-rs link --name borg --state-dir ~/.local/share/sb/borg/signal-state/` once`), and the default state_dir path.

#### Phase 2: Core signal.rs - open, receive loop, classify, dispatch
**Model:** opus

- `borg/src/signal.rs`: implement `run`, `open_or_fail`, `AcceptedSource` enum, `accepted_envelope`, `classify_signal_envelope`, `download_signal_attachment`, `dispatch_envelope`.
- Outer `ExponentialBackoff` loop with the same cadence as `telegram::run`. The backoff loop catches every transient error class (network drops, WebSocket churn, per-envelope decryption failures, broadcast `RecvError::Closed`) and reconnects. Only three error classes propagate `Err` out of `signal::run`: `OpenError::NotLinked`, `OpenError::PartiallyLinked`, and `ReceiveError::Deauthorized`. Every other error is handled inside the loop or DLQ-routed per envelope.
- **Hostname gating is upstream, not inside `signal::run`.** Following the existing pattern (`borg/src/lib.rs:241` for Telegram, `:266` for Discord, `:293` for ntfy), the daemon supervisor in `borg/src/lib.rs` calls `config::is_local_host(&Some(signal_config.host.clone()))` BEFORE spawning the `signal::run` task. On hostname mismatch the supervisor logs `"Signal configured but host {:?} does not match this machine, skipping"` and marks the subsystem as `SubsystemStatus::SkippedHostMismatch` (same enum value the other transports use). `signal::run` itself does not check hostname.
- `OpenError::NotLinked` / `OpenError::PartiallyLinked` map to `eyre::bail!` with the exact `signal-rs link ...` command. Not retried by backoff. The supervisor logs and continues serving other transports per the existing `ServerHandle::wait` shape (`borg/src/lib.rs::tasks.join_next` logs failed tasks with `Ok(Err(e)) => log::error!("a daemon task failed: {e:#}")`).
- `ReceiveError::Deauthorized` exits the loop with `bail!`. **The transport stays down until the borg systemd unit is restarted.** This is intentional - a Deauthorized means the operator's primary device revoked the link, which is an operator-acknowledged action; auto-restarting the receive loop would just hit Deauthorized again forever. Operators observe the down state via `sb doctor signal` (the section catches NotLinked / Deauthorized) and restore by re-linking.
- `broadcast::error::RecvError::Lagged(n)` on the inbound channel: log a `warn!` with the lag count and continue the loop (the broadcast stream does not terminate on lag). `RecvError::Closed` triggers a backoff reconnect (the receive task exited unexpectedly; the outer loop calls `Client::open` again, which usually succeeds since the state on disk is unchanged).
- Trace_id generation at envelope arrival via the existing `borg::trace::new()` (same module Telegram and the other transports use); trace_id flows through `intake::record_intake`, `pipeline::process_content`, and the notify acks.
- `ClassifyOutcome::PartialMultiAttachment` propagates through `dispatch_envelope` to a dedicated `notify::Signal::result_partial(&result, display_source, dropped_count, ...)` method so the ack reflects partial success ("Saved 1 of N attachments; multi-attachment support is v2"). The pipeline still processes the first attachment; only the ack format and a warn-level log of dropped attachments differ from the single-attachment case. `IngestResult` is NOT widened with a Signal-specific `dropped_count` field; the partial-ack concern lives on the Signal sink alone.
- **Note-to-Self intake-rate anomaly gate (mandatory v1).** A sliding-window counter over accepted `AcceptedSource::SelfSync` envelopes (peer DM acceptance is excluded - allowlisted senders are by definition expected traffic). When the rate over the configurable window exceeds `signal.notetoself_rate_threshold_per_hour` (default 100, deliberately well above the typical single-digits/day pattern documented in the reference doc), `dispatch_envelope`:
  - Refuses further envelopes (`accepted_envelope` returns `None` for any Note-to-Self until reset).
  - Emits a CRITICAL log line naming the rate, the threshold, and the trip time.
  - Sends a `notify::Signal::result(...)` alert to Note-to-Self ("intake-rate anomaly: Note-to-Self ingestion paused at {observed}/hour; verify signal-rs has not regressed; restart the borg daemon after verifying").
  - Sets a process-local `signal_notetoself_paused: AtomicBool` that persists for the lifetime of the borg process. There is no auto-resume.
  - Resume requires `systemctl --user restart borg`. The daemon comes back up with the counter cleared. If the rate stays high, the gate trips again - by design.
- This is the runtime backstop against the upstream wire-ACI to typed-variant regression class. borg cannot type-level-verify the boundary; the rate gate observes the symptom (10x volume spike) and fails closed. The Architect's design review called this a hard ship blocker for v1, not a deferrable open question.
- Function-level DEBUG logs at every helper entry per `~/.claude/rules/log.md`.
- `borg/src/lib.rs`: register the new `signal` module (`pub mod signal;`). Add a `signal_status: SubsystemStatus` field to the daemon's startup tracking. Wire the Signal spawn block alongside the existing Telegram / Discord / ntfy blocks at roughly `borg/src/lib.rs:241`, using the same shape:
  ```rust
  if let Some(signal_config) = config.signal.clone() {
      if !config::is_local_host(&Some(signal_config.host.clone())) {
          log::info!(
              "Signal configured but host {:?} does not match this machine, skipping",
              signal_config.host
          );
          signal_status = SubsystemStatus::SkippedHostMismatch;
      } else {
          let config_arc = Arc::clone(&config);
          let desktop_clone = desktop.clone();
          tasks.spawn(async move {
              signal::run(signal_config, config_arc, desktop_clone).await
          });
          signal_status = SubsystemStatus::Running;
      }
  }
  ```
  `signal::run` itself does NOT consult hostname; the upstream gate is the single source of truth.

#### Phase 3: notify::Signal sink
**Model:** sonnet

- `borg/src/notify.rs`: add `Signal` struct alongside `Telegram` and `Desktop`. `Signal::new`, `Signal::processing`, `Signal::result`, `Signal::resolve_recipient`. New module-level const `SIGNAL_TIMEOUT_MS: u64 = 3000` matching the existing `TELEGRAM_TIMEOUT_MS = 3000` value; outbound `client.send` calls wrap in `tokio::time::timeout(Duration::from_millis(SIGNAL_TIMEOUT_MS), ...)` per Design Invariant 2 from the notify module.
- `borg/src/signal.rs::dispatch_envelope`: call `notify_signal.processing(...)` at start, `notify_signal.result(...)` at end, with `override_recipient` set per the inbound `AcceptedSource`.
- Cross-method dispatch (when the daemon-side router routes an ack to Signal as the configured cross-method sink): use `default_recipient` from `notify::Signal` (set from `signal_config.notification_recipient`).

#### Phase 4: Intake + receipts + DLQ integration
**Model:** sonnet

- Confirm `intake::record_intake` accepts `IngestMethod::Signal` with no special-casing (the enum's `Display` impl carries it through automatically).
- Confirm `receipts::record_received` accepts `vault::schema::Method::Signal`.
- DLQ stages reused from `vault::receipts::failure_stage_from_dlq`: `intake-rejected`, `classify-failed`, `fetch-failed`, `pipeline-timed-out`, `publish-failed`, `crashed`. No new stages.
- Verify `sb borg log --method signal` query works against a hand-inserted row.
- Verify `borg-ledger.md` renders Signal entries (the renderer is enum-driven; this is a verification step, not a new code path).

#### Phase 5: sb doctor sections
**Model:** sonnet

- `sb/src/cli/checks.rs`: add `signal_findings()` and `telegram_findings()` functions. Register both in `all_sections()`.
- `signal_findings` calls `Client::open` and `client.status` (best effort; errors become Error findings).
- Hostname comparison uses `gethostname::gethostname()`.
- Unit tests for both new sections: synthetic config inputs, asserted finding sets.

#### Phase 6: Tests
**Model:** opus

- `borg/src/signal.rs`: add `#[cfg(test)] mod tests;` declaration at the bottom (Rust 2018+ submodule style).
- `borg/src/signal/tests.rs`: tests body.
  - **Privacy filter (mandatory):** synthetic `Envelope::DataMessage` with a peer ACI not in `allowed_senders` -> `accepted_envelope` returns `None`. Synthetic `Envelope::SyncMessage(SyncMessage::Sent { destination: Some(SelfSync), group_id: None, body: Some("hi"), .. })` -> returns `Some(AcceptedSource::SelfSync)`.
  - **Allowed-senders:** peer DM from ACI in `allowed_senders` with `group_id: None` -> `Some(Peer { aci })`. Same envelope with ACI not in list -> `None`.
  - **Group filter (mandatory):** `SyncMessage::Sent { destination: Some(SelfSync), group_id: Some(group_bytes), .. }` -> `None` (rejects Note-to-Self-shaped group fanout). `DataMessage { source: Recipient::Aci(<allowed>), group_id: Some(group_bytes), .. }` -> `None` (rejects allowed-sender group chatter). These cases catch the "group via shared variant" footgun confirmed against `signal_rs::envelope::Envelope` source.
  - **Rate gate (mandatory):** synthetic loop submits N+1 Note-to-Self envelopes within the window where N is the configured threshold; asserts the (N+1)-th call to `accepted_envelope` returns `None` and the `signal_notetoself_paused` flag is set. A second submission for a peer DM from an allowed sender still returns `Some(Peer { .. })` (the gate is Note-to-Self-only). A second submission for Note-to-Self after manual reset (test-only helper) accepts again.
  - **Failure-direction filter tests (mandatory, catch pattern-match loosening):** `Sent { destination: Some(Recipient::Aci("other-aci")), .. }` -> `None` (destination is not SelfSync even though the variant matched). `Sent { destination: None, .. }` -> `None`. `DataMessage { source: Recipient::Pni("..."), .. }` -> `None` (Pni is not Aci, allowlist comparison is over Aci strings).
  - **Unknown variants stay rejected:** `SyncMessage::Read`, `Receipt`, `Typing`, `Edit`, `Call`, `Unknown` -> `None`.
  - **`classify_signal_envelope` cases:** Empty envelope (no body, no attachments) -> `ClassifyOutcome::Empty`. URL body, plain text body -> `Single { kind: Url, .. }` / `Single { kind: Text, .. }`. Single photo / voice / document attachment -> `Single { kind: Image | Voice | Document, .. }`. Multi-attachment (2 photos, or 1 photo + 1 document) -> `PartialMultiAttachment { kind: <first kind>, dropped_count: 1, dropped_summary: [..], .. }`.
- `borg/src/notify/tests.rs`: add `Signal` constructor tests. Default recipient = `SelfSync` when `notification_recipient` is None; equals `Aci(...)` when set.
- `sb/src/cli/checks/tests.rs`: add `signal_findings` and `telegram_findings` cases (config-None, config-some-state_dir-missing, config-some-state_dir-collision, host-mismatch).

#### Phase 7: Docs and runbook
**Model:** sonnet

- `CLAUDE.md`: add a one-paragraph entry under "Key Conventions" about Signal transport (state_dir under `~/.local/share/sb/borg/signal-state/`, single-machine pin, sole-machine ingest, allowlist for peer DMs).
- `config/templates/borg.yml.template`: confirm the commented-out `signal:` block is present and the runbook line names the link command with the borg-owned state_dir.
- `docs/signal-rs-consumer-integration-handoff.md`: mark as superseded-in-part by this design doc (the API surface and starter stub remain authoritative for signal-rs's contract).
- `docs/signal-as-borg-transport.md`: status header gains a "see also: 2026-05-24-signal-as-borg-transport.md (design memo)" line.

## Alternatives Considered

### Alternative 1: `sb borg signal {link, status, unlink}` wraps `signal-rs link`

- **Description:** Wrap `signal-rs`'s CLI verbs as `sb borg signal {link, status, unlink}` subcommands, mirroring the `sb borg extension {sign, install, stage, show, version}` pattern.
- **Pros:** Single-binary surface; `sb --help` discoverability; consistent verb namespace.
- **Cons:** Pure duplication of work `signal-rs`'s CLI already does. The extension wrapping precedent does not apply because borg has to synthesize the unsigned XPI input around `web-ext` (manifest from `env!`, schema, AMO sidecar, install policy, idempotency tracking); there is no equivalent borg-specific work around `signal-rs link`. State_dir path discipline is handled at config-validation time and reported via the `signal` doctor section, not via CLI wrapping. Adding the wrapper means the operator now has to learn about both `sb borg signal link` and the underlying `signal-rs link` they sometimes invoke directly for debugging - two surfaces where one suffices.
- **Why not chosen:** `signal-rs` is the BotFather-equivalent for Signal. Telegram never wraps BotFather; borg should not wrap `signal-rs`. The right structural fit is visible in the codebase: Telegram's bootstrap is paste-token-into-config and identity creation is upstream's job. The wrapping instinct, modeled on the `sb borg extension` precedent, is misapplied here because the extension wrapping exists only because borg has to synthesize the unsigned XPI input (manifest, schema, install policy) - work that has no equivalent for the Signal link step.

### Alternative 2: Receipts-level dedupe across multiple linked machines

- **Description:** Allow multiple borg machines to each run `signal-rs link` against separate state dirs. All receive every Note-to-Self envelope (Signal-Server's fan-out behavior). Dedupe at the receipts.db level using a uniqueness key on `(method, sent_timestamp, source_aci)`. Losers write a noop row tagged as duplicate-deferred or skip the write entirely.
- **Pros:** No single-machine bottleneck. Resilient to one machine being offline (the other ingests). Avoids the "what hostname should we pin?" config question.
- **Cons:** Every linked machine does redundant work: ratchet decryption, classify, fabric distill if hot-path. Wasted CPU and network for the same result. The dedupe key is fragile under timestamp collisions when the phone sends two messages in the same millisecond (rare but possible with bulk pastes). The dedupe path adds a uniqueness constraint to the receipts.db schema, plus a "loser writes a no-op" code path that complicates the receipts contract. The design drift from Telegram's structurally-pinned-single-machine shape introduces an asymmetry between transports for no compelling reason (the user has not asked for HA; one machine is enough). Adds linked-device count pressure on the Signal account (each machine consumes a slot).
- **Why not chosen:** Sole-machine pin via `signal.host` matches the existing Telegram field, ships in one config line per environment, and avoids the dedupe-key complexity. If the user later wants multi-machine, it can be added as a v2 feature without retracting the v1 contract.

### Alternative 3: Wait for crates.io publish of `signal-rs`

- **Description:** Block this work until `signal-rs` is published to crates.io with the standard polish pass (license headers, README badges, docs.rs build green, version-bump policy, semver guarantees).
- **Pros:** Standard dependency story; no git-tag-pin gotchas; `cargo update` semantics fully predictable.
- **Cons:** The polish pass is non-trivial upstream work that `signal-rs` has not signed up for. Tagging git-pin is the consumer shape the handoff doc explicitly recommends. Blocking on crates.io means stalling borg for arbitrary upstream sequencing.
- **Why not chosen:** Git-tag pin is the recommended consumer shape per the `signal-rs` handoff doc, is one line in `Cargo.toml`, and the tag is annotated (immutable on remote). `cargo update` respects the tag and will not silently drift.

### Alternative 4: Out-of-process `signal-cli` daemon (Shape A from the reference doc)

- **Description:** Run `signal-cli --daemon` (Java) as a long-lived subprocess. Borg communicates with it via DBus or its JSON-RPC socket.
- **Pros:** `signal-cli` is the most-deployed Signal client outside the official desktop app. Mature, widely smoke-tested. Failure isolation: a signal-cli crash does not take borg down.
- **Cons:** JVM resident memory (300-500 MB) plus borg's own footprint. JSON-RPC marshalling adds latency for every send. DBus dependency on Linux. Bootstrap is a separate process lifecycle (systemd unit for signal-cli, systemd unit for borg, both must be coordinated). Crash isolation cuts both ways: a signal-cli crash means signal-cli's queue holds messages that borg never sees.
- **Why not chosen:** `signal-rs` is the in-process library specifically built for this consumer shape (`< 50 MB` resident, no IPC layer, single systemd unit). The reference doc covers this comparison in depth; this design doc adopts the recommendation.

## Technical Considerations

### Dependencies

- **New (borg):** `signal-rs = { git = "https://github.com/scottidler/signal-rs", tag = "v0.2.1" }`. Brings in protobuf, ed25519-dalek, x25519-dalek, sha2, hmac, aes-gcm, rusqlite (already a transitive dep for cortex/oracle's embedding cache), reqwest (already used), tokio (already used).
- **Upgrade strategy for signal-rs.** Annotated tag pins are immutable on remote; `cargo update` does not silently bump. To move to a future v0.2.2 (patch) or v0.3.0 (minor): bump the `tag` value in `borg/Cargo.toml`, re-run `otto ci` against the workspace, run the signal-rs manual smoke runbook against a real linked device, then ship under a normal `bump`-and-`otto deploy` cycle. Breaking API changes in signal-rs surface at compile time because borg consumes the typed surface (`Envelope`, `Recipient`, `Client::*`); pinning makes the upgrade an explicit code change rather than a silent dependency drift.
- **Modified (vault):** `vault/src/schema.rs::Method` gains `Signal` variant. Affects any exhaustive match in vault, cortex, oracle, distillers, sb. Compiler enforces the catch.
- **Modified (borg):** `borg/src/types.rs::IngestMethod` gains `Signal` variant. Affects `fmt::Display` and the `From<IngestMethod> for vault::schema::Method` impl. Compiler enforces catch.
- **No new external system dependencies.** No new daemon, no DBus, no Java runtime. signal-rs's WebSocket connection to `signal.org` is the only outbound from borg's process; the home network needs egress on TCP 443 (already required for Telegram).

### Performance

- **Steady-state memory:** signal-rs holds a SQLite session DB plus ratchet state. The handoff doc cites < 50 MB resident, in borg's own process. Adds linearly to borg's existing footprint; no replacement.
- **Per-message CPU:** one x25519 ratchet step per inbound, one ed25519 sign per outbound, AES-256-CBC attachment decrypt. Negligible compared to the fabric+ffmpeg+markitdown downstream pipeline costs.
- **Restart cost:** signal-rs's receive loop reconnects via WebSocket on startup; Signal-Server holds queued envelopes for 24-48h. An `otto deploy` restart loses zero messages in practice. Telegram has the same property via `getUpdates` offset handling.
- **Attachment fetch:** serial today (one CDN download per attachment) - matches the existing Telegram behavior. Concurrent fetch is a future optimization, not in scope.
- **Reingest pressure on Signal-Server:** borg's pipeline already caps reingest concurrency (the 2026-05-12 incident where 20+ unbounded ffmpeg jobs pegged the system led to this cap). Carrying forward to Signal-bound replies: a bulk replay must not send 20+ acks in parallel against Signal-Server's per-account rate limit. Reuses the existing concurrency cap; no new mechanism needed.

### Security

- **Privacy-load-bearing structural filter, defense in two layers.** Layer one is inside `signal-rs`: it knows our ACI from the linked state and remaps matching destinations to the `Recipient::SelfSync` variant. Layer two is in borg: `accepted_envelope` pattern-matches the typed variant AND requires `group_id: None` AND requires the destination to be exactly `Some(SelfSync)`. Phase 6 mandatory tests assert both positive and negative directions (correct envelopes accepted; loosened patterns rejected). Failure mode if borg's pattern match regressed (e.g., someone changed `destination: Some(SelfSync)` to `destination.is_some()`): every outbound 1:1 fan-out from any linked device would land in borg's intake_log. The negative-direction tests catch this class at PR review.
- **The wire-ACI to typed-variant boundary is upstream's responsibility - with an active runtime backstop.** borg cannot independently verify that signal-rs's remap (raw destination ACI -> `Recipient::SelfSync`) is correct, because by the time the typed envelope reaches borg the wire ACI is gone, replaced by the variant. If signal-rs regressed to mapping every outbound destination to `SelfSync`, borg's tests stay green while production silently ingests the user's entire outbound history. Mitigations (defense in depth, all three active in v1):
  - (a) `signal-rs`'s own test coverage on the mapping at the upstream boundary.
  - (b) **Mandatory v1: Note-to-Self intake-rate anomaly gate.** Per Phase 2, accepted Note-to-Self envelopes are counted in a sliding hourly window; exceeding `signal.notetoself_rate_threshold_per_hour` (default 100) trips a fail-closed pause that refuses further envelopes, emits a CRITICAL log + Note-to-Self alert, and requires `systemctl --user restart borg` to resume. The Architect's design review categorized this as the only active defense against the catastrophic privacy regression class; v1 ships with it, not as a v1.1 follow-up.
  - (c) Prominent documentation in this doc and in signal-rs's own design doc that the typed variant carries privacy weight, so upstream changes near `route_envelope_to_identity` get extra scrutiny at PR review.
- **State-dir isolation.** Default state_dir is `~/.local/share/sb/borg/signal-state/`, distinct from signal-rs's CLI default of `~/.local/share/signal-rs/`. Sharing the dir between an ad-hoc `signal-rs` CLI invocation and borg's daemon corrupts the Double Ratchet (two clients holding the same device identity). The `signal` doctor section detects state_dir collision with the CLI default and emits a `Warn` finding.
- **Allowed-senders allowlist.** Peer DMs require explicit ACI in `allowed_senders`. Strangers who only have the phone number cannot send borg material; PNI-initiated traffic falls through to the Note-to-Self filter (which rejects it) or to the allowlist (which they are not on).
- **No bot identity exposure.** Replies appear from the user's own account on the phone, in the user's own Note-to-Self thread or in the peer's existing thread. Structurally different from Telegram's bot-account model but acceptable per the reference doc.
- **Daemon-restart-during-link.** If the operator runs `signal-rs link` while borg's daemon is open against the same state_dir, the two processes hold competing SQLite WAL connections. Runbook discipline: stop borg's systemd unit, run the link command, restart. The doctor section's "linked devices" report makes mid-link state diagnosable.

### Testing Strategy

- **Unit tests** per Phase 6. The privacy filter test is mandatory; the rest are conventional coverage of new code.
- **No real-network E2E in CI.** signal-rs's manual smoke runbook (`signal-rs/docs/manual-smoke-test.md`) covers real-account validation. After the first borg deploy, the operator runs a smoke: send a Note-to-Self URL from the phone, observe the note in the vault, observe the Saved reply in Note-to-Self.
- **Peer-DM smoke:** a second smoke once a peer is added to `allowed_senders` - have the peer send a URL, observe ingestion + reply to the peer's thread.
- **Doctor section smoke:** run `sb doctor` before and after `signal-rs link`, confirm Error -> Ok transition.

### Rollout Plan

- Ship phases 1-7 as one PR, one `bump -m`, one `otto deploy`.
- First-machine bootstrap on the designated Signal host (let `$SIGNAL_HOST` = the hostname of the machine that should run Signal ingest, e.g. the home server):
  1. `otto deploy` lands the new sb binary on every machine.
  2. Operator edits `~/.config/sb/borg.yml` on `$SIGNAL_HOST` to uncomment the `signal:` block and set `host: $SIGNAL_HOST` (this field is mandatory; the daemon refuses to start if `signal:` is set and `host` is missing or empty).
  3. Operator stops the borg daemon: `systemctl --user stop borg`.
  4. Operator runs `signal-rs link --name borg --state-dir ~/.local/share/sb/borg/signal-state/`, scans the QR with the primary phone (Settings -> Linked Devices).
  5. Operator restarts the borg daemon: `systemctl --user start borg`.
  6. Operator runs `sb doctor` and confirms the `signal` section is all Ok.
- Other machines (laptops, secondary hosts) leave the `signal:` block enabled with `host: $SIGNAL_HOST`; their `signal::run` sees hostname mismatch and returns early. No link required.
- Smoke: send a Note-to-Self URL from the phone; observe a new note in the vault and a Saved reply in Note-to-Self.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Note-to-Self filter regresses on borg's side (pattern match loosened, group filter removed) | Low | High | Phase 6 includes negative-direction tests: `destination: Some(Aci(other))` -> None, `destination: None` -> None, `Sent { group_id: Some(..) }` -> None, `DataMessage { group_id: Some(..) }` -> None. A pattern-match regression fails the test suite at PR review |
| Upstream `signal-rs` regresses on the wire-ACI to `Recipient::SelfSync` mapping (every outbound message gets mapped to SelfSync) | Low | High | Per Architect Round 2 consensus, the mandatory v1 Note-to-Self intake-rate anomaly gate (Phase 2 + `SignalConfig::notetoself_rate_threshold_per_hour`, default 100/hour) is the active runtime backstop. Exceeding the threshold trips a fail-closed pause that requires daemon restart. The 10x volume spike of an unfiltered outbound history would saturate the threshold within the first hour, well before the user's intake_log fills with private conversations |
| Operator runs `signal-rs link` against the wrong state_dir, colliding with the CLI default | Low | Medium | Bootstrap template defaults to the borg-owned path; runbook names the path explicitly; `signal` doctor section emits a `Warn` on collision |
| Multiple borg machines accidentally linked separately | Low | Medium | `signal.host` is a mandatory `String` (not `Option`); config-load fails closed when missing. Doctor reports the host comparison; runbook documents the single-machine ingest model. Lowered from "Medium likelihood" because the strict schema constraint changes the failure mode from "default-on misconfiguration" to "must explicitly set the wrong hostname" |
| signal-rs git-tag drift on `cargo update` | Low | Low | Tag pin is immutable (annotated tag on signal-rs main); Cargo respects the tag |
| `Deauthorized` at runtime (primary device revoked the link) | Low | Medium | `bail!` with operator-actionable message; the broader borg process keeps serving Telegram + Discord + ntfy + Desktop; `sb doctor signal` surfaces the diagnosis on next run; runbook covers re-link |
| Attachment download stalls or fails | Medium | Low | Reuses Telegram's DLQ stage (`fetch-failed`); pipeline timeout applies; same recovery shape |
| Group messages arrive on the receive socket and get misclassified or ingested | Medium | High | `signal_rs::envelope::Envelope::DataMessage` and `SyncMessage::Sent` both carry a top-level `group_id: Option<Vec<u8>>` field; group traffic shares variants with 1:1 traffic. `accepted_envelope` filters both arms on `group_id.is_none()` in addition to the variant / source / destination match. Phase 6 includes group-traffic test cases. Raised likelihood from "Low" to "Medium" after the Architect design-review caught that the type system does not separate group from 1:1 |
| Daemon restart mid-link corrupts state_dir | Low | High | Runbook says "stop daemon, run link, restart"; signal-rs uses SQLite WAL which is reasonably robust to a clean SIGTERM; if corruption happens, the recovery is "delete state_dir, re-link" (a documented operation) |
| Signal-Server rate-limits ack replies on bulk reingest | Low | Low | Existing reingest concurrency cap applies to all outbound, including Signal; no new mechanism needed |
| `signal::run` Err propagates and crashes the borg daemon | Low | High | The Architect's design review verified that `borg/src/lib.rs::ServerHandle::wait` calls `tasks.join_next` and treats `Ok(Err(e))` as `log::error!("a daemon task failed: {e:#}")` then continues. Signal's `Err` is contained at the daemon supervisor level; other transports keep running. Phase 2 restricts `Err` propagation to three deterministic cases (`NotLinked`, `PartiallyLinked`, `Deauthorized`); transient errors are caught by the outer `ExponentialBackoff` loop and never escape |
| Signal ingest goes permanently offline after a single transient `Err` escapes the backoff (no auto-restart at the supervisor level) | Low | Medium | Phase 2 catches every transient error class inside `signal::run`'s own retry loop so escape-to-Err is reserved for cases where auto-restart would be wrong anyway (`Deauthorized` = re-link required). Operators monitor via `sb doctor signal`; the runbook covers manual systemd restart for unexpected outages |
| First ack to a new allowed-sender peer falls back to unsealed-send and leaks "sent to X" to Signal-Server | Medium | Low | Acknowledged Non-Goal; signal-rs's `SyncMessage::Contacts` consumption is upstream future work. Acks to SelfSync (the common case) always sealed |
| Operator sets `state_dir` as a symlink into signal-rs's CLI default | Low | Medium | Doctor section's collision check canonicalizes before comparing; symlinked-into-default is caught |

## Open Questions

1. Should `sb doctor` flag a separate `Warn` when `signal.state_dir` is set but the directory is empty (i.e., the operator set the path but has not run `signal-rs link` yet)? The `Client::open` `NotLinked` Error covers this functionally, but a distinct "you set the path but did not link" finding might be clearer than "NotLinked" for someone seeing the dashboard cold.
2. Confirm by test that running `signal-rs link` against an already-linked state_dir is a no-op or a documented error from signal-rs (rather than silently relinking and invalidating the prior session). This affects the runbook's idempotency guarantees on re-runs.
3. **Full multi-attachment support (v2 follow-up).** v1 emits an explicit "Saved 1 of N attachments" ack and drops the rest. Real-world frequency of multi-attachment envelopes from the phone vs the desktop client is unknown; if it turns out to be common, v2 should fan attachments[1..] into sibling ingests sharing a trace_id family. The v1 design's partial-ack avoids silent data loss; v2's fan-out avoids dropped content entirely. The decision to wait until v1 ships and observe real envelopes is intentional - sizing the v2 work without data would be guessing.
4. (Resolved in Architect Round 2.) The intake-rate anomaly gate IS shipping in v1 as a hard requirement on Note-to-Self envelopes only; see `notetoself_rate_threshold_per_hour` in `SignalConfig` and the rate-gate behavior in Phase 2. Threshold tuning (default 100/hour) is empirical and may need adjustment after the first shakedown.

## References

- [`docs/signal-as-borg-transport.md`](../signal-as-borg-transport.md) - reference doc: option space, Shape A vs Shape B, conceptual gap, privacy concerns
- [`docs/signal-rs-consumer-integration-handoff.md`](../signal-rs-consumer-integration-handoff.md) - handoff doc: signal-rs v0.2.1 API surface, structural map of `telegram.rs` -> `signal.rs`, starter stub
- `signal-rs/docs/design/2026-05-23-signal-rs-message-surface.md` - signal-rs implementation history (in the parallel `scottidler/signal-rs` repo)
- `signal-rs/docs/manual-smoke-test.md` - real-device validation runbook
- `borg/src/telegram.rs` - structural model being mirrored
- `borg/src/notify.rs` - sink pattern being mirrored for `notify::Signal`
- `borg/src/intake.rs` - transport-agnostic recording path (`record_intake`, `record_intake_with_sidecar`)
- `sb/src/cli/checks.rs` - where new doctor sections plug in
