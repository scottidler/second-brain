# signal-rs -> borg integration handoff

**Status: superseded-in-part.** The borg-side integration shipped per
`docs/design/2026-05-24-signal-as-borg-transport.md`. The
`signal-rs`-side contract (API surface, version pin, starter stub
shape) below remains authoritative. Anywhere this doc and the
implemented design memo disagree about borg internals (config layout,
state_dir default, doctor surface, supervisor hostname-gating
location, rate-gate semantics), trust the design memo.

This is the handoff doc from the agent that built `scottidler/signal-rs`
to the agent that integrates it as a borg transport inside
`scottidler/second-brain/borg`. It documents the state of `signal-rs`
as of **v0.2.1** (the first borg-integration-ready release), maps
borg's Telegram architecture onto the Signal equivalent, lists what is
deliberately out of scope and why none of those gaps block borg's
use case, and ends with a starter stub for `borg/src/signal.rs`.

The companion reference doc that establishes WHY borg might integrate
Signal at all (option-space comparison, Shape A vs Shape B,
privacy-load-bearing concerns) lives at
`docs/signal-as-borg-transport.md`. This doc is the HOW.

## Status of signal-rs at handoff

- Repo: `scottidler/signal-rs`
- Version: **v0.2.1**, annotated tag on `main`
- Form factor: Rust library crate + a CLI binary. Shape B integration
  (in-process, `signal-rs = { git = "...", tag = "v0.2.1" }` in
  borg's `Cargo.toml`) is the intended consumer shape.
- Design doc: `signal-rs/docs/design/2026-05-23-signal-rs-message-surface.md`,
  Status: Implemented. Records all ten phases plus the smoke findings
  from the v0.2.0/v0.2.1 ship.
- Smoke runbook: `signal-rs/docs/manual-smoke-test.md`. Covers
  link + send + receive against a real Signal account.
- Real-device validation done at handoff time: Note-to-Self send,
  Note-to-Self receive, sealed-sender peer send, attachment download
  with byte-exact decryption, read-receipt sync, encrypted device
  name confirmed on phone, binary-form recipient fallback fix
  verified end-to-end.

## Public API surface (the parts borg will touch)

```rust
use signal_rs::{
    Client, ClientStatus, Envelope, Recipient, SyncMessage,
    AttachmentPointer, ReceiveError, SendError, OpenError, StatusError,
};
```

The verbs:

```rust
// Open an existing linked state directory. Does NOT do network I/O.
// Returns OpenError::NotLinked if `signal-rs link` was never run
// against this dir.
Client::open(state_dir: &Path) -> Result<Client, OpenError>

// Long-running receive loop. Opens the auth WebSocket, decrypts
// incoming envelopes, broadcasts each decoded Envelope. Runs until
// the connection drops or the server signals deauthorize. This is
// the call you spawn into a tokio task.
client.run_receive_loop() -> Result<(), ReceiveError>

// Subscribe to decoded envelopes. Multiple subscribers allowed.
// broadcast::Receiver semantics: slow consumers may see Lagged and
// then resume; the stream itself doesn't terminate on lag.
client.receive() -> tokio::sync::broadcast::Receiver<Envelope>

// Send a 1:1 text message. `Recipient::SelfSync` for Note-to-Self
// (fanned out to your other linked devices including the phone).
// `Recipient::Aci(uuid)` for sealed-sender peer send.
// Returns the millisecond send-timestamp the server assigned.
client.send(to: Recipient, body: &str) -> Result<u64, SendError>

// Send a text message with file attachments. Each PathBuf is read,
// AES-256-CBC encrypted, uploaded to the Signal CDN, and referenced
// in the outbound DataMessage by pointer.
client.send_with_attachments(
    to: Recipient,
    body: &str,
    attachment_paths: &[PathBuf],
) -> Result<u64, SendError>

// Fetch a CDN-hosted attachment referenced by an inbound
// AttachmentPointer, decrypt, verify (HMAC + SHA-256), strip
// signal-cli's bucket padding (uses `pointer.size`), write
// plaintext to `dest`.
client.download_attachment(
    pointer: &AttachmentPointer,
    dest: &Path,
) -> Result<(), AttachmentError>

// Identity + device-list snapshot. Use as a pre-flight check after
// `Client::open` to confirm we're still linked from Signal-Server's
// perspective. Reports our ACI, our device_id, and the full
// `/v1/devices` list with the encrypted device names decrypted.
client.status() -> Result<ClientStatus, StatusError>

// (Optional outbound surface, not required for Note-to-Self ingest:)
client.typing(to: Recipient, started: bool) -> Result<(), SendError>
client.delete_for_everyone(to: Recipient, target_timestamp: u64)
    -> Result<(), SendError>
```

`Envelope` is `#[non_exhaustive]` with `#[serde(tag = "kind",
rename_all = "snake_case")]`. The variants borg actually consumes
are:

- `Envelope::SyncMessage(SyncMessage::Sent { destination, body, attachments, timestamp, .. })`
  with `destination == Some(Recipient::SelfSync)` is the
  **Note-to-Self path**. This is the only variant borg's MVP needs.
- `Envelope::DataMessage { source, body, attachments, .. }` is an
  **inbound peer message** (someone else sent you something). Out
  of scope for borg's Note-to-Self use case; safe to ignore.
- Everything else (`SyncMessage::Read`, `Receipt`, `Typing`, `Edit`,
  `Call`, `Unknown`): ignore for borg's MVP.

`Recipient` is `#[non_exhaustive]`:

- `Recipient::SelfSync` is the typed Note-to-Self destination.
  Privacy-critical filter: only accept Sent envelopes where
  `destination == Some(Recipient::SelfSync)`. The remap happens
  inside `signal-rs` (it knows our own ACI from the linked state
  and replaces matching ACI destinations with this variant), so
  consumers do NOT need to compare against our own ACI string.
- `Recipient::Aci(String)` is a peer ACI UUID. Out of scope for
  borg.
- `Recipient::Pni(String)` is a peer PNI UUID. Out of scope for
  borg (see "What is intentionally out of scope" below).

## Structural map: telegram.rs -> signal.rs

borg's existing `borg/src/telegram.rs` is ~600 lines. The vast
majority is transport-agnostic (intake log, pipeline, classification,
DLQ, notify) and stays unchanged when adding a Signal path. Only
the I/O envelope at the top of the file gets a Signal-shaped twin.

| `telegram.rs` element | `signal.rs` equivalent |
|---|---|
| `teloxide::Bot::new(&token)` | `signal_rs::Client::open(&state_dir).await` |
| `bot.get_me()` pre-flight reachability check | `client.status().await` pre-flight (also confirms still-linked: a primary-device unlink surfaces as `Deauthorized` once `run_receive_loop` starts) |
| `claim_polling_session()` to handle racing `getUpdates` from a prior process | **Not needed.** Signal-Server enforces "one authenticated WebSocket per device id" server-side. If a prior borg process holds the connection, our new `run_receive_loop` connect kicks them with `ConnectedElsewhere` and they exit. |
| `Update::filter_message().endpoint(closure)` (teloxide Dispatcher) | `let mut rx = client.receive(); while let Ok(env) = rx.recv().await { ... }` with `client.run_receive_loop()` running as a separate spawned task |
| `allowed_chat_ids` filter on `message.chat.id.0` | The **mandatory Note-to-Self filter**: pattern-match on `Envelope::SyncMessage(SyncMessage::Sent { destination: Some(Recipient::SelfSync), .. })`. Drop everything else. |
| `classify_telegram_message(&message)` returning `(IntakeKind, preview)` | `classify_signal_envelope(&body, &attachments)` returning the same shape, branching on the matched Sent body + attachments[0].content_type |
| `download_telegram_file(bot, &file_id)` -> `Vec<u8>` | `client.download_attachment(&pointer, &dest_path).await` then `std::fs::read(&dest_path)`. The pointer comes off `attachments[0]` of the matched Sent envelope. |
| `bot.send_message(chat_id, ack_text)` for processing/result acks | `client.send(Recipient::SelfSync, ack_text).await` |
| Outer `ExponentialBackoff` loop wrapping the dispatcher | Same shape, wrapping `Client::open` -> spawn `run_receive_loop` -> `recv()` until the loop returns / channel drops |

Everything below the transport envelope - `intake_log::record_intake`,
`pipeline::process_content`, `notify::Desktop` / `notify::Telegram`,
`classify_document` by MIME type, `extract_url_from_text`, the
trace_id machinery, the `record_dlq` paths - is transport-agnostic
and reused as-is. The Signal path produces the same `ContentKind`
values into the same pipeline.

## What is structurally different from Telegram

These are the things to internalize before writing code, because they
change the operational shape even though the code structure is
similar.

### State directory replaces the bot token

Telegram's identity is one string from `@BotFather`, stored in config.
Signal's identity is a SQLite database (`store.db`) plus session
state plus prekey state, held under a state directory. Default
location is `dirs::data_local_dir().join("signal-rs")` (Linux:
`~/.local/share/signal-rs`); the consumer can override via
`Client::open(custom_dir)`.

For borg, pick an explicit path under borg's own data directory
(e.g. `~/.local/share/borg/signal-state/`) and pass that to
`Client::open`. Do NOT share the state dir with any ad-hoc
`signal-rs` CLI invocation on the same host - Signal's Double
Ratchet does not tolerate two clients sharing a device identity.

### One-time interactive bootstrap

Telegram's flow is: paste token in config, start borg. Signal's
flow is: run `signal-rs link --name borg` once, which prints a QR
code to stdout and to a PNG file; scan it with the primary phone
(Settings -> Linked Devices); the link command completes and writes
the linked identity to the state dir. After that, borg can call
`Client::open` and `run_receive_loop` without any further
interaction.

There is no way to skip this step. Any Signal client (signal-cli,
presage, signal-rs) requires a one-time scan to establish the linked
device. Bake it into borg's deploy / first-run runbook, not into
borg's startup path.

If `Client::open` returns `OpenError::NotLinked`, that is the signal
to surface a "run `signal-rs link --name borg` first" error to the
operator. Don't try to auto-link from inside borg; the QR has to
go somewhere a human can scan it.

### No bot identity

Telegram's bot has a separate user account, separate username, and
appears in the chat as a distinct sender. Signal has no bot concept.
Replies that borg sends via `client.send(Recipient::SelfSync, ...)`
appear on the phone as messages from yourself in your own
Note-to-Self thread. The reference doc
(`docs/signal-as-borg-transport.md`) flagged this as "structurally
different but fine" - the phone's UI handles it gracefully but
worth internalizing before being surprised by it.

### Note-to-Self filter is privacy-load-bearing

The reference doc spells this out and it bears repeating: by default,
Signal's `SyncMessage::Sent` fan-out includes every message the
account sends from any device to anyone - replies to friends, group
chats, work conversations. Without the destination filter, borg
would ingest the user's entire outbound conversation history. This
is privacy-relevant: borg would be writing personal conversations
to its intake log and processing them through the pipeline.

The filter is the pattern match on
`destination == Some(Recipient::SelfSync)`. `signal-rs` does the
ACI-comparison-and-remap internally; consumers just match on the
variant.

**A unit test asserting this filter is correctly applied is
mandatory** for borg's signal.rs, the same way the Telegram path
has tests on its `allowed_chat_ids` filter. The cost of getting
the filter wrong is privacy-relevant, not just functional.

### Restart behavior is friendlier than Telegram

Signal queues envelopes for offline linked devices for ~24-48 hours
with no formal upper-bound. An `otto deploy` restart on the borg
host loses zero messages in practice - the reconnect window is
small and the server holds anything that arrived during it. There
is no Telegram-style `TerminatedByOtherGetUpdates` race to handle;
Signal-Server's "one connection per device id" rule kicks any
stale connection automatically on our reconnect.

If borg is down for longer than 24-48h (a long weekend with the
home server off), some messages may not replay. The same is true
of the Telegram path; recovery is manual (re-send from the phone).

### Attachment lifecycle is borg's responsibility

`client.download_attachment(&pointer, &dest)` writes the decrypted
plaintext to whatever `dest` path borg passes. signal-rs does not
prune attachments; that is borg's job. The pattern is identical to
the Telegram path: write attachments to a directory borg controls,
mark the receipts row `succeeded`/`failed` once the pipeline
completes, then sweep on a schedule.

### Cheap to share across tasks but not Clone

`Client` is internally `Arc<ClientInner>` but does not derive
`Clone`. To share a Client across tokio tasks (one task running
`run_receive_loop`, another consuming `receive()` envelopes,
others doing `send` from the pipeline result handlers), wrap it
in an outer `Arc`:

```rust
let client = Arc::new(Client::open(&state_dir).await?);

let receive_client = Arc::clone(&client);
tokio::spawn(async move {
    let _ = receive_client.run_receive_loop().await;
});

let send_client = Arc::clone(&client);
tokio::spawn(async move {
    let _ = send_client.send(Recipient::SelfSync, "ack").await;
});
```

(If borg ends up needing Clone often, signal-rs can ship
`impl Clone for Client` as a one-line patch release; the inner
state already supports it.)

## What is intentionally out of scope, and why none of it blocks borg

The signal-rs design doc lists several follow-ups that don't matter
for borg's Note-to-Self use case. They are documented here so the
next agent doesn't waste time trying to plug them before starting:

### Outbound read / delivery receipts

When a peer sends you a Signal message, you can send back "delivered"
and "read" receipts that surface on their phone as gray-tick and
blue-tick indicators. signal-rs currently does not send these out
(the receive side surfaces inbound receipts via
`SyncMessage::Read`, but there's no `Client::read_receipt(...)`).

Doesn't matter for borg: borg only processes Note-to-Self, and the
"sender" of a Note-to-Self is yourself. Your phone already shows
the message as sent to your other devices; there is no separate
peer expecting indicators.

### MismatchedDevices retry on peer fan-out

When sending a 1:1 message to a peer who has multiple linked devices,
signal-rs sends one ciphertext per device using a cached device
list. If the peer added or removed a linked device since our cache
was last refreshed, Signal-Server returns HTTP 409 `MismatchedDevices`
with the corrected list. signal-rs does not implement the retry
today. The send fails and surfaces a `SendError::Server`.

Doesn't matter for borg: Note-to-Self own-device sync uses a
different code path (we already know our own linked devices from
the link flow, and the auth WebSocket exchanges that during
connect). The MismatchedDevices issue only affects sends TO peers,
which borg does not do.

The design doc marks this as "v0.2 follow-up." We're at v0.2.1
without it. The label is overdue but the gap is real only for
peer DM use cases.

### PNI receive path lacks real-device smoke

A Signal account has two identities: ACI (Account Identity, the
UUID tied to your profile, contacts, name) and PNI (Phone Number
Identity, a separate UUID tied just to your phone number). The
PNI exists so strangers who only know your phone number can
message you without your profile; once you accept and exchange
profile keys, the conversation moves to ACI.

signal-rs has unit tests covering the PNI receive path
(`route_envelope_to_identity` PNI routing). It has not been
exercised against real PNI-addressed envelopes in production.

Doesn't matter for borg: every Note-to-Self envelope is ACI -> ACI
(your own ACI sending to your own ACI). PNI traffic only arrives
from strangers initiating contact, which borg's Note-to-Self
filter would drop anyway.

### SyncMessage::Contacts not consumed

When you link a new device, the primary phone sends down an
encrypted blob containing your address book (names, phone numbers,
profile keys for each contact). signal-rs does not decode this.
The consequence is that signal-rs has no peer profile keys on
file until it receives a `DataMessage` from that peer (inline
`profile_key` field on the message). Without a peer's profile
key, outbound sealed-sender sends fall back to unsealed, which
leaks "you sent a message to X" to Signal-Server.

Doesn't matter for borg: borg sends only to `Recipient::SelfSync`,
and we have our own profile key (it was persisted from the
provision message during link). Sealed-sender to yourself just
works.

This unblocks "strict sealed-sender mode" (refuse-on-missing-profile-key)
for general Signal client use, but borg doesn't need that gate.

## Privacy-load-bearing checks for borg

Both of these are mandatory before the first borg deploy that pulls
in the Signal transport.

1. **Note-to-Self filter unit test.** Write a synthetic
   `Envelope::DataMessage` (peer-initiated DM) and pass it through
   borg's classify+dispatch function. Assert that nothing is
   recorded to `intake_log` and nothing is dispatched. Then write
   a synthetic `Envelope::SyncMessage(SyncMessage::Sent { destination: Some(Recipient::SelfSync), .. })`
   and assert that it IS recorded and IS dispatched. The filter
   gates the entire Signal path.

2. **State directory isolation.** borg's signal state dir should
   live under borg's own data root, not the default
   `~/.local/share/signal-rs/` that the `signal-rs` CLI uses by
   default. This prevents an operator from running
   `signal-rs receive` interactively (e.g. for debugging) and
   accidentally desyncing the Double Ratchet against borg's live
   session.

## Starter stub for `borg/src/signal.rs`

This is a starting structure, not a finished implementation. It
mirrors the shape of `borg/src/telegram.rs::run` and stops at the
point where the existing classify+pipeline+notify chain takes over.
Compile-tested against signal-rs v0.2.1's public surface but not
wired into a real borg build; expect to iterate on the imports and
the `classify_signal_envelope` shape to match borg's exact types.

```rust
use crate::backoff::ExponentialBackoff;
use crate::config::{Config, SignalConfig};  // SignalConfig: new config type
use crate::intake::{self as intake_log, Kind as IntakeKind, Stage as DlqStage};
use crate::notify;
use crate::pipeline;
use crate::router::extract_url_from_text;
use crate::trace;
use crate::types::{ContentKind, IngestMethod};
use eyre::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use signal_rs::{
    AttachmentPointer, Client, Envelope, OpenError, Recipient, SyncMessage,
};

/// Entry point for the Signal transport. Mirrors `telegram::run`.
///
/// One-time bootstrap (not done here): the operator must have run
/// `signal-rs link --name borg --state-dir <state_dir>` and scanned
/// the QR with the primary phone before this function is first called.
/// `Client::open` returns `OpenError::NotLinked` if that hasn't
/// happened yet, and this function surfaces that as a fatal error so
/// the operator sees it on startup rather than a silent retry loop.
pub async fn run(
    state_dir: PathBuf,
    _signal_config: SignalConfig,
    config: Arc<Config>,
    desktop: Option<notify::Desktop>,
) -> Result<()> {
    let mut backoff = ExponentialBackoff::new();

    loop {
        log::info!("signal: opening client state_dir={}", state_dir.display());
        let client = match Client::open(&state_dir).await {
            Ok(c) => Arc::new(c),
            Err(OpenError::NotLinked) => {
                eyre::bail!(
                    "signal: state dir {} is not linked - run `signal-rs link --name borg --state-dir {}` first",
                    state_dir.display(),
                    state_dir.display(),
                );
            }
            Err(OpenError::PartiallyLinked) => {
                eyre::bail!(
                    "signal: state dir {} is partially linked - re-run `signal-rs link` to resume",
                    state_dir.display(),
                );
            }
            Err(e) => {
                log::error!("signal: Client::open failed: {e}");
                backoff.wait().await;
                continue;
            }
        };

        // Pre-flight: confirm we are still linked from Signal-Server's perspective.
        // A primary-device unlink would surface here as a 401 / Deauthorized.
        let status = match client.status().await {
            Ok(s) => s,
            Err(e) => {
                log::error!("signal: status pre-flight failed: {e}");
                backoff.wait().await;
                continue;
            }
        };
        log::info!(
            "signal: connected as account={} device_id={} linked_devices={}",
            status.account_number,
            status.device_id,
            status.linked_devices.len(),
        );
        backoff.reset();

        let mut rx = client.receive();

        // Spawn the long-running receive loop. It owns the auth
        // WebSocket. When it returns (deauthorize, connection drop,
        // server "ConnectedElsewhere"), we'll fall out of the recv()
        // loop below and reconnect.
        let loop_client = Arc::clone(&client);
        let receive_handle = tokio::spawn(async move {
            match loop_client.run_receive_loop().await {
                Ok(()) => log::info!("signal: receive loop returned Ok(()); reconnecting"),
                Err(e) => log::warn!("signal: receive loop returned err: {e}; reconnecting"),
            }
        });

        // Consume decoded envelopes off the broadcast channel. Apply
        // the Note-to-Self filter as the first step; everything else
        // is dropped silently (we don't even DLQ it because peer DMs
        // are explicitly out of scope for borg).
        while let Ok(envelope) = rx.recv().await {
            let (body, attachments, timestamp) = match envelope {
                Envelope::SyncMessage(SyncMessage::Sent {
                    destination: Some(Recipient::SelfSync),
                    body,
                    attachments,
                    timestamp,
                    ..
                }) => (body, attachments, timestamp),
                _ => continue, // not Note-to-Self; drop silently
            };

            // Durable intake BEFORE dispatch, same shape as telegram.rs.
            let trace_id = trace::generate(IngestMethod::Signal);
            let (kind, preview) = classify_signal_envelope(&body, &attachments);
            let chat_ctx = status.account_number.clone();
            if let Err(e) = intake_log::record_intake(
                &config,
                IngestMethod::Signal,
                &chat_ctx,
                kind,
                &preview,
                &trace_id,
            ) {
                log::error!("signal: failed to record intake trace={trace_id}: {e:#}");
                let _ = client
                    .send(
                        Recipient::SelfSync,
                        &format!("[{trace_id}] borg failed to record your input: {e}"),
                    )
                    .await;
                continue;
            }

            // Dispatch by content. Attachments take priority over text
            // body (a Signal message can carry both); mirrors how the
            // telegram path handles photo-with-caption.
            if let Some(att) = attachments.first() {
                let dest = attachment_tmp_path(att, timestamp);
                if let Err(e) = client.download_attachment(att, &dest).await {
                    log::error!("signal: download_attachment failed trace={trace_id}: {e}");
                    let _ = client
                        .send(
                            Recipient::SelfSync,
                            &format!("[{trace_id}] failed to download attachment: {e}"),
                        )
                        .await;
                    continue;
                }
                let data = std::fs::read(&dest)?;
                let filename = att
                    .file_name
                    .clone()
                    .unwrap_or_else(|| default_attachment_filename(att, timestamp));
                let content = classify_attachment_content(data, filename, att.content_type.as_deref());
                let extra_tags: Vec<String> = body
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|s| vec![format!("caption:{s}")])
                    .unwrap_or_default();

                spawn_pipeline(
                    content,
                    extra_tags,
                    trace_id,
                    Arc::clone(&client),
                    desktop.clone(),
                    Arc::clone(&config),
                );
                continue;
            }

            if let Some(text) = body.as_deref().filter(|s| !s.is_empty()) {
                let content = match extract_url_from_text(text) {
                    Some(url) => ContentKind::Url { url: url.to_string() },
                    None => ContentKind::Text { text: text.to_string() },
                };
                spawn_pipeline(
                    content,
                    vec![],
                    trace_id,
                    Arc::clone(&client),
                    desktop.clone(),
                    Arc::clone(&config),
                );
                continue;
            }

            // Empty Sent envelope (no body, no attachments) - log
            // and DLQ. Possible on real traffic (e.g. a sync of a
            // message type we don't unpack yet).
            log::debug!("signal: empty Sent envelope trace={trace_id}");
            intake_log::record_dlq(
                &config,
                IngestMethod::Signal,
                DlqStage::IntakeReject,
                "empty Sent envelope (no body, no attachments)",
                &preview,
                &trace_id,
                None,
            );
        }

        // recv() returned Err - the broadcast sender was dropped,
        // which means the receive loop task ended. Wait for the
        // join, log, and reconnect.
        let _ = receive_handle.await;
        log::info!("signal: reconnecting");
        backoff.wait().await;
    }
}

/// Classify a Signal Note-to-Self envelope's body + attachments into
/// the same `(IntakeKind, preview)` shape `classify_telegram_message`
/// produces. Lives next to `classify_telegram_message` in spirit;
/// borg's existing IntakeKind variants apply (URL, Photo, Voice,
/// Document, Text, Empty).
fn classify_signal_envelope(
    body: &Option<String>,
    attachments: &[AttachmentPointer],
) -> (IntakeKind, String) {
    // sketch only - shape this to match telegram's classify exactly.
    // The attachment.content_type field is the MIME type; reuse the
    // same `image/`, `audio/`, `application/pdf` branching that
    // `classify_document` in telegram.rs uses.
    todo!("mirror classify_telegram_message against Signal AttachmentPointer + body")
}

fn classify_attachment_content(
    _data: Vec<u8>,
    _filename: String,
    _mime: Option<&str>,
) -> ContentKind {
    todo!("mirror classify_document from telegram.rs")
}

fn attachment_tmp_path(att: &AttachmentPointer, timestamp: u64) -> PathBuf {
    // Pick a deterministic tmp path under borg's data dir. The
    // file is consumed by `std::fs::read` immediately, then
    // borg's attachment-lifecycle cleanup is responsible for
    // pruning it.
    let suffix = att
        .file_name
        .clone()
        .unwrap_or_else(|| default_attachment_filename(att, timestamp));
    std::env::temp_dir().join(format!("borg-signal-{timestamp}-{suffix}"))
}

fn default_attachment_filename(att: &AttachmentPointer, timestamp: u64) -> String {
    let ext = match att.content_type.as_deref() {
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some("application/pdf") => "pdf",
        Some("audio/ogg") => "ogg",
        _ => "bin",
    };
    format!("signal-attachment-{timestamp}.{ext}")
}

fn spawn_pipeline(
    _content: ContentKind,
    _extra_tags: Vec<String>,
    _trace_id: String,
    _client: Arc<Client>,
    _desktop: Option<notify::Desktop>,
    _config: Arc<Config>,
) {
    // Mirror the tokio::spawn block in telegram.rs:
    //   - call desktop.processing(...)
    //   - call client.send(Recipient::SelfSync, "Processing ...")
    //   - pipeline::process_content(...).await
    //   - call client.send(Recipient::SelfSync, "<result text>")
    //   - call desktop.result(...)
    todo!("mirror telegram.rs's tokio::spawn for the per-message pipeline")
}
```

The `todo!()`s are the borg-specific work that mirrors what
`telegram.rs` already does. Nothing about them is signal-rs-shaped;
they're just the existing classification/pipeline/notify wiring
adapted to take a Signal envelope's body+attachments as input
instead of a `teloxide::Message`.

## Recommended sequencing

If you're the next agent picking this up, this is the suggested
order. None of these steps depend on signal-rs changes; if you hit
something signal-rs is missing, that comes back as a feature request
against `scottidler/signal-rs` and a patch release is cut before
borg moves on (per the consumer-handoff convention).

1. **Add the dependency.** `cargo add signal-rs --git https://github.com/scottidler/signal-rs --tag v0.2.1` in `borg/Cargo.toml`. Confirm `cargo build` succeeds.

2. **Write the Note-to-Self filter unit test FIRST.** Synthetic
   peer DM -> filter drops it. Synthetic Note-to-Self -> filter
   accepts it. This is the privacy-load-bearing check; do it
   before any other Signal code lands.

3. **Stub borg/src/signal.rs from the template above** and resolve
   the `todo!()`s by porting `classify_telegram_message` /
   `classify_document` / the `tokio::spawn` pipeline block. Keep
   the structure identical to `telegram.rs` so future readers can
   diff the two paths and see what changes per transport.

4. **Add `SignalConfig` to borg's config.** Fields: `state_dir:
   PathBuf` (where the SQLite store lives), `enabled: bool` (to
   feature-flag the transport during shakedown). Mirror the
   `TelegramConfig` shape; the existing config-loading code is
   transport-agnostic.

5. **Bake the link step into the deploy/first-run runbook.**
   `signal-rs link --name borg --state-dir <path>` once,
   interactive, before the first `otto deploy` that has the
   Signal transport enabled.

6. **Wire the run dispatcher.** Wherever borg today decides which
   transports to start (the dispatcher that calls `telegram::run`
   and `discord::run`), add a conditional `signal::run` call
   guarded on `signal_config.enabled`.

7. **Smoke test against a real Signal account.** Send a URL to
   Note-to-Self from the phone. Confirm: intake row created,
   pipeline dispatched, ack reply lands on the phone as
   "Processing URL..." then result message. Same shape as the
   Telegram smoke.

8. **Document the discovery.** If anything in signal-rs's API
   surface turned out to be wrong-shaped for borg's use case, file
   it as a feature request against signal-rs and update this doc
   so the next agent knows what changed.

## References

- `docs/signal-as-borg-transport.md` - the option-space reference
  doc; explains WHY signal-rs, Shape B, Note-to-Self filter,
  attachment lifecycle, restart behavior. Read this first if
  any of the "structural differences" section above feels
  underspecified.
- `scottidler/signal-rs/docs/design/2026-05-23-signal-rs-message-surface.md` -
  the signal-rs design doc, Status: Implemented. Phase 10 section
  records the smoke findings from the v0.2.0/v0.2.1 ship including
  the padding-strip and TLS-pin defects that were found and fixed
  during real-device smoke. Useful background if you're chasing a
  receive-side anomaly.
- `scottidler/signal-rs/docs/manual-smoke-test.md` - the manual
  smoke runbook for signal-rs itself. The borg integration smoke
  is a superset (link with name=borg, plus the full classify+
  pipeline+notify chain), but the same primitives (rkvr the state
  dir, scan the QR, watch the log file) apply.
- `scottidler/signal-rs/src/client.rs` and `src/client/send.rs` -
  the public Client surface. Read alongside the API table above
  when shaping `borg/src/signal.rs`'s I/O envelope.
