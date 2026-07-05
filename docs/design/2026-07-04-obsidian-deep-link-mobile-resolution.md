# Design Document: Obsidian Deep Link Mobile Resolution

**Author:** Scott Idler (owner); investigation + implementation by Claude
**Date:** 2026-07-04
**Status:** Implemented in working tree; ship pending (`otto ci` + `bump` + `otto deploy`)
**Review Passes Completed:** 3/5 (Draft, Correctness, Edge; empirically validated on-device)
**Related:** [2026-03-26-stable-obsidian-deep-links.md](2026-03-26-stable-obsidian-deep-links.md) (Superseded)

## Summary

Borg's "Open in Obsidian" deep link opened the Obsidian app on the Pixel but did
not navigate to the ingested note. Root cause, confirmed on-device: the link
hardcoded `vault=obsidian`, but the phone's vault is named **`obsidian-remote`**,
so Obsidian could not resolve the named vault and never navigated. The same link
is tapped on devices with different vault names (desktop `obsidian`, phone
`obsidian-remote`), so **no hardcoded vault name can be correct**. Fix: drop the
`vault=` parameter entirely - `obsidian://open?file=<stem>` opens the file in
whichever vault is current on each device.

## Problem Statement

### Background

Borg emits an `obsidian://` deep link in every notification (`build_obsidian_url`,
`borg/src/pipeline/publish.rs:16`). Telegram/Signal strip custom URI schemes, so
the link is wrapped in a static GitHub Pages redirect (`scottidler/ob`,
`notify.rs:179`) that bounces into the scheme on tap. The shipped form was
`obsidian://open?vault=obsidian&file=<bare-stem>`.

### Problem

Tapping the link opened Obsidian on the Pixel but did not navigate to the note.
A prior investigation (Mina) attributed this to the `file=` value (bare stem vs
vault-relative path) and proposed a vault-relative path. That premise was wrong
on two counts, both proven here.

### Root cause (confirmed on-device 2026-07-04)

A Phase 0 spike emitted three link variants in the live Telegram reply and the
operator tapped each on the Pixel with the note confirmed present:

| Variant | Result |
|---------|--------|
| `open?file=<stem>` (no `vault=`) | navigates |
| `open?file=<dir>/<stem>` (no `vault=`, path) | does not navigate |
| `open?vault=obsidian-remote&file=<stem>` | navigates |

Conclusions:
- The failure was **`vault=obsidian` not matching the phone vault `obsidian-remote`**.
  Every prior form failed identically because they all carried the wrong vault.
- The **bare stem navigates; the path form does not** - and the bare stem is also
  location-independent, so it survives cortex's `inbox/ -> notes/` promotion
  (`cortex/src/classify.rs:492`). Both requirements met by one form.
- **Sync latency is real and separate:** the note must reach the phone (Obsidian
  Sync, open-triggered) before any link can navigate. Tapping immediately fails;
  tapping after a short wait works. This is inherent mobile-sync behavior, not a
  code bug, and is out of scope to "fix."

### Goals

- Tapping the notification link navigates to the note on the Pixel and on desktop.
- One link works across devices whose vaults are named differently.
- The link survives cortex's `inbox/ -> notes/` promotion.

### Non-Goals

- Reducing Obsidian Sync latency / changing the phone's sync mechanism.
- Editing Telegram messages after the fact (rejected 2026-03-26).
- A `sb borg|cortex notify` subcommand to push arbitrary messages to a channel
  (parked - see Addendum).
- Replaying Telegram messages missed during borg downtime (separate gap - see
  Addendum).

## Proposed Solution

### Overview

Drop the `vault=` parameter. `build_obsidian_url` emits
`obsidian://open?file=<url-encoded-stem>`. Omitting `vault` makes Obsidian open
the file in each device's **current** vault, which is correct on every device; a
hardcoded name matches at most one. The bare stem stays location-independent.

### Architecture

One-function change (`build_obsidian_url`) plus its two call sites
(`publish.rs:62`, `pipeline.rs:825`), which drop the now-unused `vault_name`
argument. The four notification sinks are pure formatters over
`IngestResult.obsidian_url` and are unaffected. The `scottidler/ob` redirect is
verified correct (single-decode round-trips borg's single `urlencoding::encode`)
and unchanged.

### Implementation Plan

#### Phase 0: On-device spike - which form navigates (DONE)
**Model:** operator
- Live Telegram reply emitted three variants; operator tapped each on the Pixel.
- **Result:** no-vault bare stem navigates; `vault=obsidian` was the bug; sync
  latency requires a short wait. Spike code fully removed.

#### Phase 1: Drop `vault=` from the deep link (DONE)
**Model:** sonnet
- `build_obsidian_url(note_path)` emits `obsidian://open?file=<stem>`; removed the
  `vault_name` param and updated both call sites; rewrote the doc comment.
- **Success criteria:** unit test asserts inbox-path and notes-path inputs produce
  the SAME URL (location-independence) and that the URL contains no `vault=`.

#### Phase 2: Tests + spike removal (DONE)
**Model:** sonnet
- Updated `pipeline/tests.rs` (no-vault expectations); added
  `test_build_obsidian_url_omits_vault_param` as a regression guard that bites if a
  vault name is re-hardcoded; removed the vault-name-with-spaces test.
- Reverted the `format_telegram_reply` spike to the single "Open in Obsidian" link.
- **Success criteria:** `otto ci` exits 0.

#### Phase 3: Ship + live-verify
**Model:** operator
- `bump && otto deploy` on desk.lan (restarts borg with the real, spike-free build).
- Ingest a URL, wait for sync, tap the link on the Pixel and on desktop.
- **Success criteria:** the live link opens the note on both, before and after
  cortex promotion.

## Acceptance Criteria

- [x] `build_obsidian_url` emits no `vault=` parameter (regression test asserts it).
- [x] Same URL for a note in `inbox/` and in `notes/` (location-independence test).
- [ ] `otto ci` exits 0 with all spike code removed.
- [ ] Live Telegram link opens the note on the Pixel (after sync) and on desktop.

## Resolved Decisions

- **2026-07-04 - Root cause is the vault-name mismatch, not the `file=` form.**
  On-device spike: `vault=obsidian` never navigated on the `obsidian-remote` phone
  vault; the vault-less bare stem did. (Owner: Scott; evidence: live tap test.)
- **2026-07-04 - Drop `vault=` rather than set a correct name.** The link is tapped
  on devices with different vault names, so no single name is correct; omitting it
  uses each device's current vault.
- **2026-07-04 - Mina's vault-relative-path proposal rejected.** Its premise
  ("publish_note runs post-move") was false (borg writes to `inbox/`, cortex
  promotes later), and the path form failed on-device anyway.

## Alternatives Considered

### Alternative 1: `vault=obsidian-remote` (correct phone name)
- Navigates on the phone (spike variant 3 proved it) but is device-specific - it
  would break the desktop, whose vault is `obsidian`. Rejected: not cross-device.

### Alternative 2: Vault-relative path (`file=notes/<stem>`) - Mina's proposal
- Move-fragile (borg builds the link in `inbox/`; cortex promotes to `notes/`
  within ~5 min) and failed to navigate on-device. Rejected on both counts.

### Alternative 3: `obsidian://search?query=<stem>` (the 2026-03-26 choice)
- Never navigates - only opens the search pane. Superseded.

## Technical Considerations

- **Dependencies:** none (`urlencoding` already used).
- **Security:** stem is URL-encoded; no vault name or secret in the link.
- **Testing:** location-independence + no-`vault=` regression guard; the old
  vaulted form's assertion is deleted so nothing pins the bug back.
- **Rollout:** `bump && otto deploy`; deploy restarts borg with the spike-free
  build. The `scottidler/ob` redirect needs no change.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| A device's current vault is the wrong one when tapped | Low | Med | Single primary vault per device; no-vault opens it correctly |
| User perceives failure by tapping before sync completes | Med | Low | Documented: wait a beat for Obsidian Sync; not a code bug |
| Stem collision (two notes same stem) | Low | Low | Obsidian's resolver picks one; unchanged from prior behavior |

## Open Questions

_None._

## References
- `borg/src/pipeline/publish.rs:16` - `build_obsidian_url` (the fix)
- `borg/src/pipeline/tests.rs` - `test_build_obsidian_url_omits_vault_param` (regression guard)
- `cortex/src/classify.rs:492`, `cortex/src/config.rs:670` - the promotion + 5-min cadence
- `borg/src/notify.rs:179` - `wrap_obsidian_redirect` (verified correct)
- Obsidian URI docs: https://help.obsidian.md/Extending+Obsidian/Obsidian+URI

## Addendum

### Incidental fix folded in: borg systemd secrets path
While spiking, a `systemctl --user restart borg` surfaced a latent bug: the unit's
`ExecStartPre` decrypts `~/repos/scottidler/secrets/.secrets`, a path that no
longer exists (the repo was renamed `secrets` -> `keep`). The decrypt silently
produced an empty env, borg started with no `TELEGRAM_BOT_TOKEN`, and skipped the
Telegram bot entirely (receiver + notifier). Fixed the live unit and the source
(`borg/src/service.rs:197` -> `repos/scottidler/keep/.secrets`) so a fresh
`sb borg daemon --install` writes the correct path. Rides this commit.

### Parked: `sb borg|cortex notify` subcommand
No way exists to send a message to the Telegram/Signal channel outside the ingest
path (the Phase 0 delivery had to go through a temporary formatter patch). A small
`sb borg notify --to telegram|signal <msg>` reusing the daemon's resolved creds
and the `notify::{Telegram,Signal}` sinks would serve spikes, manual pings, and
alerts. Its own design doc; not folded in.

### Parked: replay messages missed during downtime
On startup `claim_polling_session` (`telegram.rs:104`) acks the entire Telegram
backlog to claim the polling session, so messages sent while borg is down are
discarded, not processed. Reasonable expectation is that reconnect replays them;
this trades off against double-ingest and belongs in its own design.
