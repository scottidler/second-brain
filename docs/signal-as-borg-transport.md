# Signal as a borg Transport — Reference

Status: reference doc summarizing the option space for adding Signal as a second
inbound transport to borg, peer to the existing Telegram path. Not a design memo
and not a roadmap. The design-memo counterpart for the `signal-cli`-as-linked-device
path lives at
`tatari-tv/thoughts/directors/scott.idler/2026-05-21-signal-ingest-via-linked-device.md`.

This doc exists so future conversations (in second-brain or elsewhere) can pick up
cold and understand:

- Why Signal is structurally harder to integrate than Telegram, even though both end
  up doing similar things from the user's perspective.
- Which pieces of borg's Telegram architecture survive unchanged under Signal, and
  which need new code.
- The full menu of client implementations that exist today, with their costs and
  failure modes.
- The two distinct integration shapes inside borg (out-of-process daemon vs
  in-process crate) and the trade-offs between them.
- The open questions that block committing to any specific path.

## The Telegram baseline

For grounding, borg's current Telegram integration:

| Aspect | Implementation |
|---|---|
| Inbound code | `borg/src/telegram.rs::run` — `teloxide::Dispatcher`, in-process |
| Outbound code | `borg/src/notify.rs::Telegram` — `Bot::send_message`, same `teloxide` crate |
| Auth | `bot-token` string, single config field |
| Wire protocol | HTTPS to `api.telegram.org`, long-poll on `getUpdates` |
| Identity | Dedicated bot account, separate from any human user |
| Off-network workflow | Phone → Telegram cloud → bot. Phone never reaches the home network. |
| Inbound port required | None |
| State to back up | Bot token (regenerable from `@BotFather` if lost) |
| Steady-state RSS | < 50 MB, in borg's own process |
| Concurrency lock | Telegram's API rejects a second `getUpdates` on the same token with `TerminatedByOtherGetUpdates`; `claim_polling_session` handles the handoff |

What borg actually needs from any transport to preserve the user experience:

1. **Off-network workflow** — send from the phone over cellular, message lands at the
   home server without the phone touching the home network.
2. **DM-to-myself shape** — there's a single "inbox" conversation; URLs sent there
   produce notes; replies appear in the same conversation.
3. **A small number of operations** — receive (subscribe to a stream of envelopes),
   send (reply with ack / Saved / Failed), classify (URL / photo / voice / audio /
   document / text).
4. **No inbound port on the home server.**
5. **A durable record of every inbound message** — handled by
   `intake_log::record_intake` against `receipts.db`, transport-agnostic.

Any Signal integration must preserve points 1-5. The differences between transports
sit underneath those points, in the layer that turns a phone message into bytes on
the home server.

## The conceptual gap

The single load-bearing fact that makes Signal harder to reason about than Telegram:

> **Telegram hosts the client for your bot. Signal makes you be the client.**

Telegram's architecture:

```
borg ──HTTPS──► api.telegram.org (Telegram hosts the bot identity)
                       │
                       │ envelope routing + queue + bot client all in one cloud
                       ▼
                  phone user
```

`teloxide` is a typed wrapper around `curl`. The Telegram bot is a first-class
concept in Telegram's servers. The bot's identity, message queue, and connection
state all live in Telegram's cloud. borg's machine does nothing except ship HTTPS
requests.

Signal's architecture:

```
borg ──in-process──► YOUR signal client ──WebSocket──► signal.org (envelope routing only)
                              │
                              │ all crypto, all state, all device identity HERE
                              │
                              ▼
                  Signal's servers route to ALL devices linked to the account
                              │
                              ▼
                         phone user (just another linked device)
```

Signal's servers do **only** end-to-end-encrypted envelope routing and key
distribution. There is no bot concept, no token endpoint, no hosted client. To
operate a Signal account, you run a real Signal client on a machine you own. That
client holds the device's cryptographic identity, ratchets sessions, decrypts
inbound envelopes, and encrypts outbound ones. None of it can be delegated to
Signal's infrastructure.

Every confusing aspect of Signal integration follows from this:

- **Why is there no bot token?** There is no bot. There are accounts, and accounts
  have devices, and devices have cryptographic identity.
- **Why does signal-cli need on-disk state?** That state is the device's identity
  and Double Ratchet session keys. Lose it, lose the device.
- **Why does linking require a QR scan from the phone?** Adding a new client is
  adding a new device, which the primary (the phone) has to cryptographically vouch
  for.
- **Why are there multiple competing implementations (signal-cli, presage,
  hypothetical signal-rs)?** They are all *client implementations*. Each one is
  someone's attempt at being a Signal client well. There is no shortcut around
  being a client.
- **Why does Keegan say "you have to run your own API infra"?** Not literal — the
  precise statement is "there is no hosted endpoint you can point at; you have to
  be your own client."

The Telegram analog would be: imagine if Telegram had no Bot API, and the way to
ship a bot was to install Telegram Desktop on a server, link it to a real phone
number, and shell out to it. That is exactly what running signal-cli is.

## What translates cleanly from the Telegram architecture

These pieces of borg's Telegram path apply unchanged regardless of which Signal
client implementation is picked:

| Telegram concept | Signal equivalent | Translation cost |
|---|---|---|
| Off-network routing via Telegram cloud | Off-network routing via Signal cloud | None — both are cloud-routed |
| `borg/src/telegram.rs::run` (loop subscribed to dispatcher) | `borg/src/signal.rs::run` (loop subscribed to envelope source) | Same shape, different envelope deserialization |
| `classify_telegram_message` (returns `(IntakeKind, preview)`) | `classify_signal_message` (same return type) | Same shape, different message accessors |
| `notify::Telegram` (trait impl on outbound) | `notify::Signal` (trait impl on outbound) | Same trait, different `send` implementation |
| `allowed-chat-ids: [i64]` config | `allowed-source-numbers: [String]` (E.164) config | One field, different type |
| `intake_log::record_intake` durable-door pattern | Identical | Zero cost; transport-agnostic by design |
| Receipt → ledger → pipeline flow | Identical | Zero cost |
| Reply-to-source-conversation pattern | Identical (reply to the number that sent it, or Note-to-Self for self-sync) | Zero cost |

The structural conclusion: the borg-side code shape barely changes. The work is
entirely in the layer below `borg::signal` — getting envelopes off the wire and
into a deserialized form borg can hand to its handler.

## Note to Self is the ingest channel

The single trick that makes a single-operator Signal ingest work without a bot
account or a second phone number: linked-device sync, plus Note to Self.

Signal's sync model fans every message envelope to every linked device on an
account:

- `dataMessage` — someone else sent you a message; fanned to all your devices.
- `syncMessage.sent` — *you* sent a message from another device; fanned to your
  other devices so their conversation views update.
- `syncMessage.read` / `syncMessage.contacts` — receipts, contact-list sync, etc.

**Note to Self is not a special chat type.** It is implemented as messages from
your account *to* your account, delivered as `syncMessage.sent` envelopes where
`source == destination == your own E.164 number`.

The ingest rule, stated in one line:

> If `envelope == SyncMessage::Sent && destination == config.account_number`, treat
> the message as inbound. Otherwise ignore.

This rule is load-bearing. Removing it produces two failure modes, in opposite
directions:

- Matching only `dataMessage` silently drops Note to Self entirely — the most common
  ingest path produces nothing.
- Matching `syncMessage.sent` without filtering on `destination` ingests **every
  message you send to anyone** on Signal — friends, group chats, everything.
  Catastrophic for both privacy and DB size.

A unit test that sends a self-sync, a sync-to-friend, and a dataMessage and asserts
exactly one ingest row exists is mandatory for any implementation.

The reply path is symmetric: `notify::Signal::result` sends `Saved: <title>` from
your account to your account, which fans out via the same sync mechanism — the
phone sees the ack in the same Note to Self conversation as the original URL.

## The client-implementation landscape

Every option for getting envelopes off Signal's wire goes through one of these
four projects. They are all client implementations of the Signal protocol; their
differences are language, packaging, maturity, and what surface they expose.

### `signalapp/libsignal` (Rust)

The foundation. Signal's official protocol and crypto library, written in Rust by
Signal themselves. Provides Double Ratchet, X3DH, sender keys, group v2 crypto.
**Not a client** — no networking, no storage, no identity orchestration. Every
other project on this list either uses it directly (presage, hypothetical
signal-rs) or wraps it through FFI (signal-cli via Java JNI).

What it is not: usable directly from a project like borg. It is a kernel; you
still need the OS around it.

Source: <https://github.com/signalapp/libsignal>

### `signal-cli` (Java + libsignal-via-JNI)

The dominant practical client. AsamK's project. Runs as a CLI for one-shot
operations or as a long-running JSON-RPC daemon over Unix socket / TCP.

| Aspect | Detail |
|---|---|
| Language | Java |
| Underlying crypto | libsignal-client (Rust) via JNI |
| Distribution | Tarball releases, also distro packages (lagging) |
| State dir | `~/.local/share/signal-cli/data/` |
| Daemon mode | `signal-cli -a +X daemon --socket /path` |
| Surface | `link`, `send`, `receive`, group ops, attachments, profiles, identity, registration, sticker packs — the whole Signal feature set |
| Steady-state RSS | ~200-300 MB (JVM) |
| Cold start | A few seconds (JVM) |
| Maintenance | Active; AsamK responsive |

This is the **default safe pick** for any Signal integration today. Mature, broad
feature coverage, daemon mode is clean, JSON-RPC schema is stable. The cost is the
JVM tax and an out-of-process boundary borg has to cross.

Source: <https://github.com/AsamK/signal-cli>

### `bbernhard/signal-cli-rest-api` (Go + signal-cli inside Docker)

A REST API wrapper around signal-cli. Distributed as a Docker container. The
endpoint surface visible at <https://bbernhard.github.io/signal-cli-rest-api/>
covers send (`/v2/send`), receive (`/v1/receive/{number}`), attachments
(`/v1/attachments` list/delete), groups (8 endpoints), typing indicators,
reactions, profile, identity, registration, link, sticker packs.

| Aspect | Detail |
|---|---|
| Language | Go (HTTP layer) wrapping Java (signal-cli) wrapping Rust (libsignal) |
| Distribution | Docker container |
| Underlying daemon | signal-cli running inside the container |
| Surface | REST endpoints |
| Steady-state cost | signal-cli's cost + Go HTTP server + Docker overhead |

**Not the right runtime dependency for borg.** Three nested layers, Docker as a
hard dependency, HTTP over localhost to reach a thing one socket-hop away. Any
borg integration that wanted out-of-process Signal access would talk to
signal-cli's Unix socket directly — strictly fewer hops.

**The right reference for borg.** bbernhard's endpoint catalog is the most
thoroughly validated enumeration of "which Signal operations have proven enough
demand that someone wrote a wrapper for them." When sizing what a Rust-native
Signal crate should cover, use bbernhard's endpoint list as the feature checklist.
See "How bbernhard is useful" below.

Source: <https://github.com/bbernhard/signal-cli-rest-api>

### `whisperfish/presage` (Rust)

A pure-Rust Signal client library on top of libsignal-rust. No daemon, no CLI —
just a crate.

| Aspect | Detail |
|---|---|
| Language | Rust |
| Underlying crypto | libsignal-client (Rust) directly, no FFI |
| Distribution | crates.io |
| State backends | sled, sqlite, in-memory |
| Surface | `Manager::register`, `Manager::link_secondary_device`, `Manager::send_message`, `Manager::receive_messages`, etc. |
| Steady-state cost | In-process; no separate process, no JVM |
| Maintenance | Active but smaller community than signal-cli; API has had breaking changes between minor versions |
| Maturity caveats | Group v2 has historically been the weak spot; 1:1 messaging and sync messages are the most-exercised path |

The relevant observation: **borg's inbound hot path is exactly the subset presage
exercises most heavily.** Note-to-Self ingest needs (a) link-secondary-device,
(b) receive sync messages, (c) send a 1:1 message back to self. No group v2, no
stories, no payments. The risky parts of presage are not the parts borg touches.

This is the candidate that flips the architectural picture from "JVM daemon
+ socket" to "in-process Rust crate."

Source: <https://github.com/whisperfish/presage>

### Hypothetical: `signal-rs`

A future project Keegan has expressed interest in: a pure-Rust replacement for
signal-cli's daemon surface, sitting directly on libsignal-rust. Not yet built.
Two functions in the ecosystem if it lands:

- A drop-in replacement for the signal-cli daemon (same JSON-RPC schema, no JVM).
- A library crate (overlapping presage's territory but with different design
  choices, particularly around a headless-link flow signal-cli doesn't support).

If `signal-rs` ships, borg picks it up the same way it would pick up signal-cli
or presage — depending on which form factor it exposes.

### Comparison

| Option | Process | Lang | JVM | Surface | Status |
|---|---|---|---|---|---|
| signal-cli | External daemon | Java | Yes | JSON-RPC over Unix socket | Mature default |
| signal-cli-rest-api | Docker container | Go + Java | Yes | REST over HTTP | Useful as a feature catalog, wrong runtime |
| presage | In-process crate | Rust | No | Rust API on libsignal | Mature for borg's subset, smaller community |
| signal-rs (hypothetical) | Either | Rust | No | TBD | Does not exist |
| Rolling your own | In-process | Rust | No | Whatever you write | Requires reimplementing what presage already does |

## Integration shapes inside borg

Independent of which client implementation gets picked, there are two distinct
shapes for how borg consumes it.

### Shape A: out-of-process daemon

```
[ signal-cli (or signal-rs) daemon ]      systemd user unit
              │
              │ JSON-RPC over Unix socket
              ▼
[ borg ]                                   systemd user unit
   │
   ├─ borg::signal::run (subscribed to socket)
   ├─ borg::notify::Signal (replies via the same socket)
   └─ intake_log + pipeline + ledger (transport-agnostic)
```

Properties:

- Two systemd units to manage (the daemon and borg).
- The Signal session survives borg restarts; reconnecting to the socket is
  trivial.
- The daemon can serve other clients on the same socket (e.g. an unrelated
  status-page poller). Not currently useful but available.
- For signal-cli specifically: pay the JVM tax (~250 MB RSS, several-second cold
  start). For a hypothetical signal-rs daemon: tax disappears, structure
  unchanged.

This is the shape the existing design memo on `tatari-tv/thoughts` describes.

### Shape B: in-process crate

```
[ borg ]                                   one systemd user unit
   │
   ├─ borg::signal::run (owns the WebSocket to Signal's servers)
   ├─ borg::notify::Signal (sends via the same WebSocket)
   ├─ presage (or signal-rs) as a cargo dependency
   └─ intake_log + pipeline + ledger
```

Properties:

- One systemd unit. No socket, no second process.
- Matches `teloxide`'s in-process posture for Telegram.
- Steady-state RSS substantially lower than signal-cli; comparable to the current
  telegram path.
- Trade-off: every borg restart triggers a fresh Signal session reconnect. For a
  daemon that restarts on `otto deploy`, this is fine; Signal tolerates frequent
  reconnects from linked devices. The reconnect window is a brief gap during
  which a message *could* land at Signal's servers and be queued until borg's
  next connect (Signal queues for ~24-48h for offline linked devices, so the
  realistic loss window is zero unless a restart hangs).
- Requires picking presage (today) or signal-rs (when/if it exists). Cannot be
  done with signal-cli, which has no Rust API.

### Which shape applies to which option

| Option | Shape A | Shape B |
|---|---|---|
| signal-cli | Native fit | Not possible (Java) |
| signal-cli-rest-api | Possible but strictly worse than signal-cli direct | Not possible |
| presage | Not applicable (no daemon) | Native fit |
| signal-rs | If it exposes a daemon | If it exposes a crate |

### What survives between Shape A and Shape B

Everything above the transport layer:

- `borg::signal::run` — the loop reading envelopes. Both shapes write this function;
  the difference is what it reads from (`UnixStream` vs `presage::Manager::receive_messages`).
- `classify_signal_message` — pure function over a deserialized envelope. Identical
  in both shapes.
- `notify::Signal::result` — sends a reply. Both shapes call the same outbound
  helper; only the underlying send mechanism differs.
- `SignalConfig` — same fields. The `socket_path` field becomes unused under Shape
  B; everything else applies.
- `intake_log::record_intake`, the pipeline, the ledger, the receipts DB — fully
  transport-agnostic. No code changes.

The choice between shapes is essentially: *does the Signal session live in its own
process, or inside borg's?* Everything else is the same code.

## How bbernhard contributes (without being a runtime dependency)

bbernhard's REST endpoint catalog is the single most useful artifact in this space
that is **not** a runtime candidate. Its value is as a **feature checklist** when
sizing what a Rust-native Signal surface should cover.

| Capability bbernhard exposes | Relevance to borg |
|---|---|
| `/v2/send` (vs deprecated `/v1/send`) | Signals which send-shape lessons have already been learned in production |
| `/v1/receive/{number}` | The receive primitive every implementation needs |
| `/v1/attachments` + `/v1/attachments/{id}` (list + delete) | **Direct solution to the "attachment dir grows unbounded" gotcha.** Any in-process implementation should expose an equivalent lifecycle API. |
| `/v1/typing-indicator/{number}` (PUT / DELETE) | Nice-to-have for L2 distills: show `...` in Note to Self while a long-running operation (Whisper on a 40-min video) is in flight |
| `/v1/remote-delete/{number}` | Allows a failed-ingest reply to be deleted and re-sent cleanly rather than accumulating noise in Note to Self |
| `/v1/groups/...` (8 endpoints) | Out of scope for single-operator Note-to-Self ingest. Included here for completeness; if group ingest is ever wanted, the operation set is pre-enumerated. |
| `/v1/identity/...`, `/v1/profile`, sticker packs | Out of scope. |

The operations transfer between bbernhard's REST and any Rust-native surface;
the transport (HTTP) does not. Treat bbernhard's docs as the answer to "did
someone already figure out which Signal operations are worth supporting?" rather
than as a thing to deploy.

## Gotchas that are specifically Signal, not just generic transport gotchas

The Telegram path does not have analogs to any of these. Anything building Signal
support inherits all of them.

### Linking requires an interactive QR scan from the primary

There is no headless way to add a new device to an existing Signal account in the
official protocol. The primary device (the phone) must scan a QR code displayed by
the new device. signal-cli, presage, and any future Rust client all hit this same
constraint.

Practical consequences:

- First-time setup on a new home server is a one-time manual ritual.
- Wiping `~/.local/share/signal-cli/data/` (or whatever the chosen client's state
  dir is) requires re-linking from the phone.
- Migrating the daemon to a new host = link the new host first, verify it works,
  optionally unlink the old host from the phone's Linked Devices list.

The single open question in the option space is whether Keegan's project would
provide an alternative linking flow that does not require the QR scan. The
protocol details of how that would work are not public yet, so this remains an
unknown.

### One client instance per state directory

Signal's Double Ratchet state cannot tolerate two concurrent clients against the
same device identity. Two `signal-cli daemon` processes sharing a state directory
will desync the ratchet, and Signal will start reporting decrypt failures and
eventually unlink the device.

Practical version: don't run an ad-hoc `signal-cli` or `presage` instance against
the production state directory while the production daemon (or borg, under Shape
B) is running.

### Self-sync surfaces every outbound message, not just Note to Self

The default `syncMessage.sent` fan-out includes every message the account sends
from any device. Without the `destination == account_number` filter, an
implementation will ingest the user's entire outbound conversation history.

A unit test asserting this filter is correctly applied is mandatory. The cost of
getting it wrong is privacy-relevant.

### Offline retention is finite

Signal queues messages for offline linked devices for ~24-48 hours, with no formal
upper-bound guarantee. If the home server is down for a long weekend, messages
sent during the window may not replay.

Recovery is manual: scroll Note to Self on the phone, find URLs without acks,
forward them into Note to Self again. signal-cli (or any client) sees them as
fresh envelopes.

Telegram's analog is roughly the same length, so this is not unique to Signal —
but the Telegram path's failure mode is easier to spot because Telegram preserves
the original message in the bot chat and the user can find unprocessed messages
trivially.

### Attachment lifecycle is the client's responsibility

signal-cli and presage both write inbound attachments to a directory on disk.
Neither prunes. Without a cleanup task, the directory grows without bound.

The receipts DB already records the attachment path; once the receipts row is
`succeeded` or `failed` (non-DLQ), the attachment file is safe to delete.
Implementing the cleanup is straightforward; the gotcha is that it has to be
remembered.

### JSON-RPC socket permissions (Shape A only)

`/run/user/$UID/signal-cli.sock` (or equivalent for signal-rs) lives in
`$XDG_RUNTIME_DIR`. Default permissions (0600 on the socket, 0700 on the parent)
are correct for single-user borg. World-readable permissions would let any local
user execute `send` and `receive` operations against the Signal account.

### Identity in replies

There is no bot identity. Replies to Note to Self appear on the phone as messages
from the user's own account, in the user's own Note to Self conversation. This
is fine — the phone's UI handles it gracefully — but it is structurally different
from Telegram's reply pattern where the bot is a separate visible identity. Worth
internalizing before being surprised by it.

## Open questions

These are unresolved at the time of writing and would need answers before
committing to a specific path.

### Is presage adequate for borg's specific operations today?

The earlier-stated concern about presage's maturity was over-broad. Presage's
weak spots (group v2, advanced features) are not parts borg touches. The
relevant question is narrower: does presage, today, reliably handle
`link_secondary_device` against a real phone, surface `SyncMessage::Sent`
envelopes through `Manager::receive_messages`, and send a 1:1 message back to
self?

This question has a binary answer. The answer is reachable by a small validation
exercise (a single Rust binary that does exactly those three things). Until the
exercise is run, the answer is unknown.

### Will Keegan's `signal-rs` project happen?

Keegan has expressed interest. No code has landed. If it happens, it likely
exposes a Rust crate suitable for Shape B *and* a daemon suitable for Shape A —
the project is sized to replace the Java stack entirely.

If it doesn't happen, the option space reduces to:

- Shape A with signal-cli (mature, JVM tax).
- Shape B with presage (mature for borg's subset, no JVM).

### What is the headless-link feature Keegan wants?

Keegan flagged that signal-cli does not support a particular device-registration
pattern he wants. The shape of that pattern was not stated explicitly. The
plausible interpretation is a way to complete linking via an out-of-band token
(HTTPS POST from a trusted device, or a one-time code typed on the primary's
Signal settings) rather than requiring a QR scan. Whether this is feasible
against the official Signal protocol — and whether it is what Keegan actually
means — is a separate conversation.

### Do other inbound shapes ever matter (DMs from other people, group chats)?

For a single-operator borg, only Note to Self is relevant. If borg ever wants to
ingest URLs sent by other people (a shared "send me interesting things" inbox),
the `dataMessage` path becomes load-bearing and the `allowed_source_numbers`
allowlist gates it. The architecture supports this without changes; it is purely
a config question.

## References

- Design memo (Shape A, signal-cli specifics):
  - Local: `~/repos/tatari-tv/thoughts/directors/scott.idler/2026-05-21-signal-ingest-via-linked-device.md`
  - GitHub: <https://github.com/tatari-tv/thoughts/blob/main/directors/scott.idler/2026-05-21-signal-ingest-via-linked-device.md>
- `signalapp/libsignal` (Rust foundation): <https://github.com/signalapp/libsignal>
- `AsamK/signal-cli` (Java daemon): <https://github.com/AsamK/signal-cli>
- `bbernhard/signal-cli-rest-api` (REST wrapper; reference catalog):
  <https://github.com/bbernhard/signal-cli-rest-api>
  Endpoint docs: <https://bbernhard.github.io/signal-cli-rest-api/>
- `whisperfish/presage` (Rust crate): <https://github.com/whisperfish/presage>
- Telegram integration in borg today: `borg/src/telegram.rs`,
  `borg/src/notify.rs` (`Telegram` struct), `borg/src/config.rs`
  (`TelegramConfig`)
- Conceptually adjacent field-engineering reference (style and structure model):
  `tatari-dev/keegan-thoughts/research/2026-05-21-oidc-ssh-browser-wrapper.md`
