# Design Document: Signal Note-to-Self Cold-Start Bootstrap

**Author:** Scott Idler
**Date:** 2026-05-28
**Status:** Implemented (code landed + `otto ci` green; manual wiped-state-dir validation still pending - see Testing Strategy)
**Review Passes Completed:** 5/5 (Draft; Correctness; Clarity/Edge Cases; Excellence; Advisor review - reworked fingerprint from "own-account session presence" to a "bootstrap-send-succeeded" latch after the advisor surfaced a commit-before-send hole; confirmed against libsignal that session acknowledgement is not publicly exposed)

## Summary

A freshly-linked borg Signal device receives **zero** Note-to-Self messages until it has *sent* at least once, even though the link is healthy and the auth websocket is connected. The cause is a property of the Signal protocol, confirmed from `libsignal` source: a "sent transcript" sync (the primary forwarding a copy of a self-message to its linked devices) requires the **primary** to hold an outbound Double-Ratchet session to the **secondary**, and those sessions are built **lazily on first send** - never at link time. Because borg only ever *listens*, the phone never builds that session and never fans the transcript out. The fix is to have borg send one Note-to-Self self-ping at first start, which delivers a `PreKeySignalMessage` that bootstraps **and acknowledges** the phone->borg session for good. borg records a **bootstrap-sent latch** the moment that send returns `Ok` (i.e. the message was accepted for delivery); the bootstrap and a `sb doctor` Warn both key off that latch, so the self-ping fires exactly once on success and re-fires if the send ever failed - and the cold-start state stays visible to the operator if it recurs (re-link, wiped store). The whole change is contained in `second-brain`; no `signal-rs` change is required.

## Problem Statement

### Background

borg's Signal transport (per `docs/design/2026-05-24-signal-as-borg-transport.md`) consumes the in-process `signal-rs` library as a linked **secondary** device (the user's phone is the primary). Its daily job is to ingest **Note-to-Self** messages: the user sends a URL to their own Signal "Note to Self" conversation, the phone forwards a copy to its linked devices as `SyncMessage::Sent { destination: SelfSync }`, and borg classifies and ingests it - the structural peer of the Telegram path.

On 2026-05-28 a live shakedown found that borg, despite being linked (`device_id=2`, named "borg"), `host: desk` correct, daemon active, and `sb doctor` all-green, had **never ingested a single Note-to-Self**. Every reconnect logged `server reports queue empty`; no inbound data envelope ever arrived for a self-message. A standalone `signal-rs receive` on borg's state dir reproduced the same nothing, ruling out a borg embedding bug. A single sealed-sender DM from a *different* account *had* reached the device once (correctly rejected by the privacy gate because `allowed-senders: []`), proving inbound peer delivery and decrypt worked.

The fix discovered live: send one Note-to-Self **from** borg (`signal-rs send --to self`). Immediately afterward the phone began delivering self-sync transcripts; a fresh YouTube URL then ingested end-to-end and published with `method: signal`.

### The protocol mechanism (confirmed from source)

The "why" is not speculation. From `signalapp/libsignal` (~v0.94.1) and `scottidler/signal-rs`:

1. **Sent-transcript sync requires a primary->secondary Double-Ratchet session.** signal-rs's own symmetric fan-out, `dispatch_sync_to_own_devices` (`signal-rs/src/client/send.rs:815-1004`), proves the shape: to encrypt a sync for each other device it either reuses a persisted session (hot path, `:840-848`) or, when none exists, fetches the device's prekey bundle and runs `process_prekey_bundle` to build one (cold path, `:852-941`). The phone's job is the mirror of this.

2. **Sessions are built lazily, never at link time.** `signal-rs/src/link.rs` (`:363-405`) only *generates and uploads* prekey batches; it calls `process_prekey_bundle` nowhere. Linking publishes keys to the server but builds zero sessions. So immediately after linking, the phone has no session to borg.

3. **The empty queue is the smoking gun.** signal-rs handles `QueueEmpty` at `client/client.rs:238-246`. An empty queue on every reconnect means the phone never *enqueued* anything for device 2 - the failure is pre-enqueue, on the primary's addressing side, not a decrypt or stuck-session problem on borg.

4. **A single self-send bootstraps *and acknowledges* the session.** When borg sends one Note-to-Self, the phone receives a `PreKeySignalMessage` from device 2. That inbound message bootstraps the phone->borg session and acknowledges it (libsignal clears the pending pre-key on the first reply: `rust/protocol/src/state/session.rs:564-585`, invoked from the decrypt path at `session_management.rs:593`).

5. **An acknowledged session never goes stale; this will not recur.** The only time-based expiry in the protocol is `MAX_UNACKNOWLEDGED_SESSION_AGE = 30 days` (`rust/protocol/src/consts.rs:25`), and it applies **only** to a locally-initiated session that was never acknowledged (`state/session.rs:260-290`). An acknowledged or peer-initiated session is never stale (`session.rs:113-116`), and even the 30-day flag self-heals via a bundle refetch rather than silently breaking delivery. There is no server-side TTL and no unused-device pruning. A device only drops out of fan-out on explicit deauthorization (`OpenError::Deauthorized`). **Therefore the bug is a cold-start condition, not ongoing staleness:** once borg has sent once, the session is acknowledged and durable. It can only recur if borg's local store is wiped or the device is re-linked - both of which return it to cold-start.

### Problem

borg silently fails its primary purpose (Note-to-Self ingest) on every fresh link, with no signal to the operator that anything is wrong - `sb doctor` reports green because the link *is* healthy; only the invisible primary-side session is missing. The operator has no way to know the remedy is "send once from borg."

### Goals

- **Eliminate the cold-start failure automatically.** A freshly-linked (or freshly re-linked / wiped-store) borg establishes the phone->borg session on its own at first start, with no operator action.
- **Make the cold-start state visible.** `sb doctor`'s `signal` section warns when borg is linked but has not recorded a successful bootstrap send, naming the exact remedy, so a recurrence (or a machine where auto-bootstrap was disabled/failed) is diagnosable in one command instead of a multi-hour live debug.
- **No behavior change in the healthy case.** Once the latch is set, neither the bootstrap nor the doctor check does anything. No extra Signal traffic on healthy restarts.
- **Idempotent and hole-free.** The latch is set **only** when the self-send returns `Ok`. A send that fails (after the local session was already committed) does not set the latch, so the bootstrap correctly re-fires on the next start rather than concluding "healthy" off a half-built session.

### Non-Goals

- **Periodic keepalive sends.** Proven unnecessary: acknowledged sessions do not expire (see Background #5). We explicitly do **not** add a recurring self-send.
- **`last_seen_ms`-based staleness detection.** `GET /v1/devices` `lastSeen` only proves the device *connected* (which it did during the incident, with an empty queue), is day-granular, and would not have caught this bug. It is at most secondary info, not the detector.
- **Active self-ping probe inside `sb doctor`.** Sending a real Note-to-Self on every `doctor` run would pollute the receipts log and risk the rate gate. The doctor check stays passive (read session state); the *bootstrap* (in the daemon) is the only thing that sends.
- **Wrapping `signal-rs link`** or changing the out-of-band bootstrap flow.

## Proposed Solution

### Overview

The change is entirely in `second-brain`; `signal-rs` is unchanged.

1. **Auto-bootstrap** in `borg/src/signal.rs::run()`: after `Client::open` + `status()` succeed and before the receive loop starts, if borg has **not recorded a successful bootstrap send for this identity**, send one Note-to-Self self-ping. On `Ok`, write the bootstrap-sent latch. Guarded, logged, test-suppressed.
2. **Bootstrap-sent latch:** a small borg-owned marker recording `{account, device_id, sent_at}` of the last successful bootstrap send.
3. **Doctor Warn** in `sb/src/cli/checks.rs`: read the same latch; emit a `Warn` when linked but the latch is absent/identity-mismatched, with the remediation text.

### Why a "send-succeeded" latch and not "session present"

The obvious fingerprint - "does borg hold an own-account session?" - has a correctness hole. In `signal-rs`'s `dispatch_sync_to_own_devices` (`src/client/send.rs:902-995`), the new borg->phone session is `process_prekey_bundle`'d and **committed to the store (`:981`) *before* the network `send_sync_message` (`:989`)** - it cannot hold a SQLite transaction across a network call. So if the network send fails after commit, borg has a local session but the phone never received the `PreKeyMessage`, the phone->borg session is never built, Note-to-Self still never arrives - yet a "session present" check would read healthy and never re-fire. The protocol-true alternative (count only *acknowledged* sessions) is not available: libsignal's acknowledgement marker (`pending_pre_key`) is `pub(crate)` and `has_usable_sender_chain` reports send-ability, not acknowledgement (`signalapp/libsignal` `rust/protocol/src/state/session.rs:260,507-585`). The **send-succeeded latch** keys off exactly the condition that closes the hole: `client.send(SelfSync, …)` returning `Ok` means `send_sync_message` succeeded, i.e. the `PreKeyMessage` was accepted for delivery to the phone. A failed send returns `Err`, the latch stays unset, and the next start re-fires.

### Architecture

```
  ┌────────────────────────────────────────────────────────────┐
  │ borg-owned bootstrap latch:  ~/.local/share/sb/borg/         │
  │   signal-bootstrap.json  { account, device_id, sent_at_ms }  │
  └────────────────────────────────────────────────────────────┘
        ▲ write on send Ok            ▲ read
        │                             │
  ┌─────┴───────────────┐   ┌─────────┴──────────────────────────┐
  │ borg/src/signal.rs  │   │ sb/src/cli/checks.rs               │
  │ run() startup:      │   │ signal section:                    │
  │  latch absent for   │   │  latch absent for current identity │
  │  current identity?  │   │  -> Warn (passive, no send)        │
  │   -> send SelfSync  │   └────────────────────────────────────┘
  │   -> on Ok, write   │
  │      latch          │
  └─────────────────────┘
```

The daemon **acts** (sends, then writes the latch); doctor only **reads** it. Keying the latch to `{account, device_id}` (both from `status()`) means a re-link to a different identity correctly reads as "not bootstrapped" even if a stale latch file survives.

### Data Model

No SQLite schema changes (this also avoids a Rust-side schema migration, which project rules forbid). One new borg-owned JSON marker file:

```jsonc
// ~/.local/share/sb/borg/signal-bootstrap.json
{ "account": "+15039990803", "device_id": 2, "sent_at_ms": 1780035141921 }
```

Path resolved via a new `vault::paths` helper (e.g. `borg_signal_bootstrap_marker()`), kept **outside** the signal-rs-owned `signal-state/` dir so it never collides with signal-rs's store. The marker is "valid" only when its `account` and `device_id` equal the live `status()` values; otherwise it is treated as absent.

### API Design

**borg — bootstrap latch helpers** (a small module, e.g. `borg/src/signal/bootstrap.rs`):

```rust
/// Read the bootstrap-sent latch; returns true only if a marker exists AND its
/// {account, device_id} match the live identity (so a re-link invalidates it).
fn bootstrap_done(marker_path: &Path, account: &str, device_id: u32) -> bool;

/// Persist the latch after a successful self-send. Best-effort: a write failure
/// is logged (WARN) but not fatal — worst case is a redundant ping next start.
fn record_bootstrap(marker_path: &Path, account: &str, device_id: u32, sent_at_ms: u64);
```

**borg — auto-bootstrap** in `borg/src/signal.rs::run()` (the existing startup path that already does `Client::open` -> `client.status()` -> builds `notify::Signal` -> `client.receive()` -> `client.run_receive_loop()`, around `:712-779`):

```rust
// After status() logs "signal: connected ..."; status gives account + device_id.
if !notify::real_notifications_disabled()
    && !bootstrap::bootstrap_done(&marker_path, &status.account_number, status.device_id)
{
    log::info!("signal: bootstrap not recorded for this identity; sending one \
                Note-to-Self to establish the phone->device sync session");
    match client.send(Recipient::SelfSync, COLD_START_BOOTSTRAP_BODY).await {
        Ok(ts) => {
            bootstrap::record_bootstrap(&marker_path, &status.account_number, status.device_id, ts);
            log::info!("signal: bootstrap self-ping sent ts={ts}; latch recorded");
        }
        // Err -> latch NOT written -> re-fires next start. Hole closed.
        Err(e) => log::warn!("signal: bootstrap self-ping failed: {e} \
                    (Note-to-Self ingest will not work until this succeeds; \
                     it retries on next borg start, or run `signal-rs send --to self`)"),
    }
}
```

`COLD_START_BOOTSTRAP_BODY` is a module-level `const` (a recognizable string such as `"borg: establishing Signal sync session"`). The send runs on the same `LocalSet` as the receive loop (signal-rs futures are `!Send`); `run()` is already invoked inside that `LocalSet` by the supervisor.

**Placement and socket coexistence.** The check/send is placed before `client.run_receive_loop()` for simplicity, but in-process contention is *not* a concern: borg's `notify::Signal` acks already call `client.send` while the receive loop holds the auth socket and they coexist (proven live - the "Processing" ack arrives without bumping the loop). The `ConnectedElsewhere` bump only happens between *separate processes* opening a second auth socket for the same device (e.g. a standalone `signal-rs` CLI run alongside the daemon), which this design does not do.

**Gate rationale.** The bootstrap rides `notify::real_notifications_disabled()` - the same guard every other borg Signal send already consults (per CLAUDE.md, all three sinks check it). This is semantically correct, not just convenient: that guard means "borg performs no real outbound Signal traffic" (tripped by `cfg!(test)`, `CARGO_TARGET_TMPDIR`, `NEXTEST_RUN_ID`, or the operator's `BORG_DISABLE_DESKTOP_NOTIFY` override), and a cold-start self-ping is outbound Signal traffic, so it should honor the same switch.

**borg — doctor Warn.** `signal_probe_status` (`sb/src/cli/checks.rs:854`) already calls `Client::open` + `status()`; it gains the marker read and surfaces a `bootstrapped: bool` on `SignalProbe::Linked` (computed via `bootstrap_done` against `status.account_number` / `status.device_id`). `signal_findings_for` emits, when `bootstrapped == false`:

```
⚠️ [signal] linked but the phone->device sync session is not yet established —
   Note-to-Self will NOT be ingested until borg sends once. Normally auto-fixed on
   borg (re)start; if it persists, run:
   signal-rs send --to self --state-dir <state_dir> "ping"
```

### Implementation Plan

All phases are in `second-brain`; no `signal-rs` change or version bump.

#### Phase 1: bootstrap latch module + path helper
**Model:** opus
- Add `vault::paths::borg_signal_bootstrap_marker()` (outside `signal-state/`), tilde-safe via the existing path helpers.
- Add `borg/src/signal/bootstrap.rs` with `bootstrap_done` / `record_bootstrap` (+ a `Marker { account, device_id, sent_at_ms }` serde struct). Unit tests: absent marker -> false; matching identity -> true; mismatched account or device_id -> false; corrupt/unreadable file -> false (treated as absent).

#### Phase 2: borg auto-bootstrap wiring
**Model:** opus
- Wire the latch check + self-ping into `borg/src/signal.rs::run()` after `status()`, guarded by `real_notifications_disabled()` and the module-level `const` body; write the latch only on send `Ok`.
- Test: with the send gate suppressed (test mode), assert the decision logic — absent latch -> would-send path taken and (on simulated Ok) latch recorded; present matching latch -> skip; failed send -> latch not written. Use a seam (e.g. a small sender closure) so the decision is testable without a live `Client`.

#### Phase 3: doctor Warn
**Model:** sonnet
- Add `bootstrapped: bool` to `SignalProbe::Linked`; compute it in `signal_probe_status` via `bootstrap_done(status.account_number, status.device_id)`; emit the Warn in `signal_findings_for`.
- Tests in the existing checks test module: Warn when latch absent, Ok when latch present.

#### Phase 4: docs + ship
**Model:** sonnet
- Update `docs/design/2026-05-24-signal-as-borg-transport.md` and `CLAUDE.md`'s Signal-transport bullet to note the cold-start bootstrap + doctor check.
- Update this doc's Status to Implemented.
- `otto ci`, `bump`, `otto install` + `systemctl --user restart borg` (non-extension change - per `feedback-skip-extension-resign`, NOT `otto deploy`).
- **Manual validation (required, see Testing Strategy):** the unit tests cannot exercise the phone-side handshake; the only real proof is a wiped/re-linked state dir on a machine, confirming the journal logs the bootstrap send, the phone shows the ping, the latch is written, and a subsequent Note-to-Self ingests.

## Alternatives Considered

### Alternative 1: Doctor detection only (no auto-bootstrap)
- **Description:** Only add the doctor Warn; operator runs `signal-rs send --to self` by hand.
- **Pros:** Smallest surface; borg sends nothing on its own.
- **Cons:** Leaves the daily-driver transport silently broken on every fresh link until the operator happens to run `doctor` and read the hint. The whole point is that the failure is invisible.
- **Why not chosen:** User selected belt-and-suspenders; an invisible failure on the primary path should self-heal, not wait for a manual step.

### Alternative 2: Periodic keepalive self-send
- **Description:** borg sends a self-ping on a timer to keep the session "warm."
- **Pros:** Would paper over any session loss.
- **Cons:** Unnecessary - acknowledged sessions never expire (Background #5); adds recurring self-traffic, receipt noise, and rate-gate pressure for no protocol benefit.
- **Why not chosen:** Contradicts the confirmed protocol behavior; violates "no unbounded/standing fan-out" instinct.

### Alternative 3: Bootstrap via `sb bootstrap --signal` / link wrapper
- **Description:** Do the self-send as part of an explicit bootstrap verb run at link time.
- **Pros:** Sends only at the moment of linking, conceptually where it belongs.
- **Cons:** Linking is out-of-band (`signal-rs link`), and we deliberately do not wrap it (per the transport design's Non-Goals). An explicit verb is one more step the operator must remember - the same fragility as Alternative 1.
- **Why not chosen:** First-daemon-start is the natural, unmissable trigger and is already where `Client::open` happens.

### Alternative 4: `last_seen_ms` staleness check
- **Description:** Surface borg's own `DeviceEntry.last_seen_ms` from `status()` and warn if old.
- **Pros:** Data already on the wire (currently discarded).
- **Cons:** Detects "device not connecting," not "primary not fanning out" - the incident had a fresh `last_seen` and an empty queue. Day-granular. Wrong signal for this failure.
- **Why not chosen:** Would not have caught the bug. May be added later as secondary info, out of scope here.

### Alternative 5: "Own-account session present" fingerprint (a signal-rs accessor)
- **Description:** Expose `Client::own_account_session_device_ids` reading existing session rows; bootstrap/doctor key on "no own-account session."
- **Pros:** No new persisted state; reads protocol state directly; one shared cross-repo accessor.
- **Cons:** **Holey.** `dispatch_sync_to_own_devices` commits the local session before the network send (`send.rs:981` then `:989`), so a failed send leaves a session present while the phone got nothing - the check reads healthy and never re-fires. Tightening to "acknowledged sessions only" is not possible: libsignal's `pending_pre_key` is `pub(crate)` and `has_usable_sender_chain` is not an acknowledgement signal.
- **Why not chosen:** The hole defeats the central goal (self-healing). The send-succeeded latch closes it with no signal-rs change.

### Alternative 6: Received-self-sync latch (in signal-rs or via receipts)
- **Description:** Latch on "borg has received a `SyncMessage::Sent{SelfSync}`" (a new signal-rs store flag, or borg's existing `receipts` rows where `method=signal`).
- **Pros:** Directly measures the end goal (the phone *is* delivering to us); great doctor signal.
- **Cons:** Not idempotent for the bootstrap: a received self-sync only happens after the *user* writes a Note-to-Self, which can be long after the bootstrap. Until then the latch stays unset and borg would re-ping on every restart/deploy. The send-succeeded latch flips on borg's own action, so it is idempotent immediately.
- **Why not chosen:** Idempotency. (Receipts also conflate "stale" with "user sent nothing.")

## Technical Considerations

### Dependencies
- None new. The change is contained in `second-brain` (`vault::paths`, `borg`, `sb`); no `signal-rs` change, no version bump, no cross-repo tag sequencing. Uses the already-pinned `signal-rs` (`Client::send`, `Client::status`).

### Performance
- One extra local SQLite read at daemon start (negligible). At most one extra outbound Signal message per cold-start (effectively once per link, lifetime). Zero cost on healthy restarts.

### Security / Privacy
- The bootstrap sends a Note-to-Self to the user's *own* account only (`Recipient::SelfSync`) - no new recipient surface, no peer exposure. The body is a fixed innocuous string. The doctor check reads local session state only (no network beyond the `status()` call it already makes).
- The self-ping is an *outbound send*, not an inbound Note-to-Self, so it does not count against `signal.notetoself_rate_threshold_per_hour` (which gates *received* envelopes).

### Edge cases handled explicitly
- **Phone offline when borg bootstraps.** The self-ping is a normal Signal send: the `PreKeySignalMessage` is queued server-side for the phone and processed when the phone next comes online, establishing the session then. No timing coupling between borg's start and the phone being awake.
- **Multi-machine install.** The supervisor host-gates `signal::run` on `signal.host` (per the transport design), so only the pinned machine runs the receive loop *and* the bootstrap. Non-Signal machines never reach the bootstrap path - no duplicate self-pings.
- **Bootstrap on a machine whose session already exists** (e.g. `desk` after the live fix): the accessor reports non-empty, the bootstrap no-ops, doctor reports Ok. Healthy restarts are silent.

### Testing Strategy
- signal-rs: unit test `own_account_session_device_ids` with a store seeded with zero / one own-account session, asserting self-device-id exclusion.
- borg: test that empty session-ids -> exactly one send, non-empty -> zero sends, and that `real_notifications_disabled()` suppresses the send under `cfg!(test)`.
- doctor: test Warn-on-cold-start and Ok-on-session-present in the `checks` test module.
- Manual: on a wiped/re-linked state dir, start borg, confirm the journal logs the bootstrap send, the phone shows the ping, and a subsequent Note-to-Self ingests - the exact loop validated live on 2026-05-28.

### Rollout Plan
- Ship signal-rs tag, then second-brain. Non-extension change: `otto install` + `systemctl --user restart borg`. On `desk` the session is already established (fixed live), so the bootstrap will no-op there; verification requires a deliberately wiped/re-linked state dir or a second machine.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Bootstrap send fails at startup (network blip) | Med | Low | `send` returns `Err` -> latch **not** written -> next daemon restart re-fires. Logged at WARN with the manual remedy. Hole-free by construction (latch set only on `Ok`). |
| Re-link to a new identity but stale marker file survives (state dir not wiped) | Low | Med | Marker is keyed to `{account, device_id}` and treated as absent on mismatch -> bootstrap re-fires. Wiping the state dir (the documented re-link flow) also removes nothing borg needs since the marker lives outside `signal-state/`. |
| Latch write fails (disk/permissions) after a successful send | Low | Low | Best-effort write, logged WARN; worst case is one redundant self-ping on the next start. Never blocks ingest. |
| Auto-send surprises an operator who expected borg to be receive-only | Low | Low | One self-only message, once per identity, logged at INFO, documented in CLAUDE.md and the transport doc; `real_notifications_disabled()` keeps it out of tests and honors the operator's no-outbound override. |
| Marker present but session genuinely broken by an exotic event (phone reinstalled) | Low | Low | A phone reinstall re-keys the primary and deauthorizes borg -> surfaced as `OpenError::Deauthorized` (existing doctor Error + re-link), a different and already-handled path. |

## Open Questions
- [x] **Resolved (advisor review):** session-presence vs acknowledged-only vs send-succeeded. Chose the **send-succeeded latch** - session-presence is holey (commit-before-send) and acknowledged-only is not exposed by libsignal.
- [ ] Should doctor also surface borg's own `last_seen_ms` as secondary info (clearly labeled "connection, not sync")? Deferred; not required for the fix.
- [ ] Should the marker also be invalidated if `signal-state/store.db` is newer than the marker (catching a store rebuild that kept the same identity)? Deferred; the identity-key check covers the common re-link case and the doctor Warn is the backstop.

## References
- `docs/design/2026-05-24-signal-as-borg-transport.md` - the transport this fixes
- `docs/design/2026-05-24-signal-state-dir-internalization.md` - the "mirror Telegram" invariant
- libsignal: `rust/protocol/src/state/session.rs:113-116, 260-290, 507-585` (acknowledgement is `pub(crate)`), `rust/protocol/src/consts.rs:25`, `rust/protocol/src/session_management.rs:593`
- signal-rs: `src/client/send.rs:815-1004` (sync fan-out; **commit `:981` precedes send `:989`** - the hole), `src/link.rs:363-405` (no session at link), `src/client/client.rs:238-246` (QueueEmpty)
- borg: `borg/src/signal.rs:712-779` (run/startup), `borg/src/notify.rs:335-401` (Signal sink send path), `sb/src/cli/checks.rs:854-885` (signal_probe_status)
- Memory: `project-signal-note-to-self-needs-outbound-first`
