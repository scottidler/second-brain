# Design Document: Codebase Review Remediation

**Author:** Claude (Opus 4.8), reconciled with Gemini Architect audit and Codex review
**Date:** 2026-06-09
**Status:** Implemented
**Review Passes Completed:** 5/5 (revised 2026-06-09: re-ranked per Codex; Finding 6 reframed as add-auth; auth wired through existing secret resolution)

## Summary

A handoff review (`docs/design/2026-06-09-codebase-review-handoff.md`) surfaced ten verifiable defects in the second-brain workspace, ranging from a daemon-crashing UTF-8 panic to stale operator docs and a misleading SQLite transaction pattern. This document specs the remediation for every finding that survived an independent code audit, with two findings re-scoped where the original recommendation was wrong for this system's topology or invariants.

## Problem Statement

### Background

`docs/design/2026-06-09-codebase-review-handoff.md` was produced by a prior review agent. Its findings were independently verified twice: once against the code by this author, once by the Gemini Architect persona with read access to both the workspace and the live vault. Every finding empirically exists. Two of the original recommended fixes do not fit this system and are corrected here.

### Problem

The workspace carries a small set of concrete defects that erode correctness and operator trust:

1. Production code truncates user-controlled strings by **byte** index behind a **byte-length** guard, so any multi-byte codepoint straddling the cut boundary panics the daemon.
2. Operator-facing docs (repo `CLAUDE.md`, live vault `CLAUDE.md`, `home.md`) and an in-vault sweep link describe a retired ledger/dashboard/binary model.
3. A vector-DB write path opens a `DEFERRED` transaction, then swallows a failed nested `BEGIN IMMEDIATE;`, so the write lock is never acquired up front despite comments claiming otherwise.
4. The oracle DB path is defined twice: configurable in `oracle`, hardcoded in `cortex`. A custom `oracle.yml` `db-path` silently desyncs the two.
5. Borg's Rust `ServerConfig` default port (`8080`) disagrees with every other surface (templates, CLI, extension all use `8181`).
6. The default bind is `0.0.0.0` and `/ingest` has no authentication.
7. `borg/src/pipeline.rs` and `vault/src/search.rs` are large enough to have forced `BLOAT_MAX_LINES=3600`.
8. Terminal receipt failure-stage classification still uses substring matching on free-form reason text.
9. Stale pipeline comments describe a removed dual-write markdown DLQ.
10. The cold-note sweep surfaces daily/journal notes, not just ingested knowledge.

### Goals

- Eliminate the UTF-8 panic class across all production truncation sites.
- Make every operator-facing doc and in-vault link match current reality.
- Make the vector-DB transaction acquire its write lock honestly.
- Establish a single source of truth for the oracle DB path that both crates share.
- Make the daemon's default port match the rest of the system.
- Add opt-in authentication to `/ingest` without breaking the existing LAN ingest topology.
- Replace substring-based failure classification with typed stages.
- Remove stale pipeline comments.
- Scope the cold-note sweep to the work it is meant to surface.
- Decompose the two oversized modules once the correctness fixes above have landed.

### Non-Goals

- No rewrite. The architecture is sound; this is correctness and hygiene.
- No change to the one-way data-flow invariant (borg writes the vault + receipts; cortex is the only embeddings writer; oracle owns its read index).
- No schema migration. None of these touch the on-disk note or DB schema.
- No new transport or ingestion source.
- No change to the vault-as-source-of-truth model.
- The `crashed: 55` receipts from the review snapshot are **operational context, not remediation**. Given the watchdog's `received`-past-timeout promotion and the 2026-05-12 fan-out incident, they are plausibly stale incident residue. One `sb borg log --status failed --stage crashed` pass is sufficient; this becomes work only if recent rows show recurrence.

## Proposed Solution

### Overview

Nine fixes executed back-to-back in a single sequence, ranked by operator-facing impact (this ordering is endorsed by the Codex review). The UTF-8 helper is the highest-value crash fix and lands first. The SQLite transaction fix is hygiene - the code claims `IMMEDIATE` semantics it does not have, but cortex is the sole writer so there is no real concurrency hazard - so it ranks below the user-facing correctness, config, docs, and auth work. Module decomposition lands last because it is safest to split a module after its correctness is settled. There is no soak period or evidence gate between phases.

### Architecture

The changes touch five crates but introduce only one new shared primitive:

- **`vault::text`** (new module) - the single home for UTF-8-safe string truncation. Both `borg` and `sb` depend on it. Modeled on the existing safe truncation patterns in `cortex::fabric::truncate_input` (byte-budgeted with `floor_char_boundary`) and `distillers::validate::truncate_at_sentence_boundary` (character-budgeted with `chars()` / `char_indices()`). The helper follows the character-budgeted form.
- **`vault::paths::oracle_db_path()`** (new helper) - the single source of truth for the oracle DB path. `oracle::config` and `cortex::config` both call it; neither hardcodes nor independently defaults the path.

Everything else is in-place edits to existing functions, comments, config defaults, and docs.

### Data Model

No schema changes. The only new types are:

- `vault::text` free functions (no state).
- An optional `auth_token: Option<String>` field on `borg`'s `ServerConfig` (serde key `auth-token`), defaulting to `None`.

### API Design

New shared truncation API in `vault/src/text.rs`:

```rust
/// Truncate `input` to at most `max_chars` characters, appending the ASCII
/// ellipsis "..." only when a cut occurred. Always returns a valid UTF-8
/// string; never panics on a multi-byte boundary. `max_chars` counts
/// characters, not bytes. `max_chars == 0` yields "..." for non-empty input,
/// "" for empty input.
pub fn truncate_with_ellipsis(input: &str, max_chars: usize) -> String;

/// Truncate `input` to at most `max_chars` characters without an ellipsis.
/// Borrows when `input` already fits; otherwise borrows the head slice cut at
/// the byte index of the `max_chars`-th character.
pub fn truncate(input: &str, max_chars: usize) -> &str;
```

The char-accurate, panic-free primitive is `char_indices().nth(max_chars)` (the
byte index of the `max_chars`-th character, or `None` when the string is
shorter), not `floor_char_boundary(max_chars)` - the latter is a *byte* budget.
The existing ASCII `...` suffix is preserved so log/preview output is unchanged.

Borg `ServerConfig` gains:

```rust
#[serde(default, rename = "auth-token")]
pub auth_token: Option<String>,
```

Shared path helper in `vault/src/paths.rs`:

```rust
/// The single source of truth for the oracle SQLite DB path.
/// Both oracle (reader) and cortex (embeddings writer) resolve here.
pub fn oracle_db_path() -> PathBuf;
```

### Implementation Plan

#### Phase 1: UTF-8-safe truncation helper and replacements
**Model:** sonnet
- Create `vault/src/text.rs`; declare `pub mod text;` in `vault/src/lib.rs`.
- Implement `truncate_with_ellipsis` and `truncate` char-accurately via `char_indices().nth(max_chars)` (matching the char-budgeted prior art at `distillers/src/validate.rs:54`). Do not copy `cortex::fabric::truncate_input`'s `floor_char_boundary(max_chars)` form here - that is a byte budget, correct for its token-estimate use but wrong for a character count.
- Write unit tests in `vault/src/text.rs`: ASCII; Spanish accents (`á`, `ñ`); emoji; string exactly at the limit; string one char over; `max_chars == 0`; empty input; a cut that lands mid-codepoint in the byte-equivalent position (the exact case that panics today).
- Replace every byte-slice truncation in production code with the helper:
  - `borg/src/routes.rs:132-135`
  - `borg/src/intake.rs:47-49` (`preview_text`)
  - `borg/src/telegram.rs:543-546`
  - `borg/src/discord.rs:258-260`
  - `borg/src/ntfy.rs:207-213`
  - `borg/src/pipeline.rs:1798-1809`, `:2062-2065`, `:2288-2291`
  - `borg/src/signal.rs:677-679`
  - `sb/src/cli/borg.rs:588-590`
- Grep the workspace for residual `[..` byte slices on string types in production paths; convert any stragglers.

#### Phase 2: Oracle DB path single source of truth
**Model:** opus
- Add `vault::paths::oracle_db_path()` resolving to `~/.local/share/oracle/oracle.db` via `dirs::data_local_dir()` (panic-on-`None` per the established `vault::paths` pattern; never fabricate a `~/`-prefixed fallback).
- `cortex/src/config.rs:771-776`: replace the hardcoded body of `oracle_db_path()` with a call to `vault::paths::oracle_db_path()`.
- `oracle/src/config.rs`: remove the configurable `db-path` field and its `default_db_path`; `db_path()` calls `vault::paths::oracle_db_path()`.
- Update `config/templates/oracle.yml.example` to drop `db-path` (with a comment that the path is fixed and owned by `vault::paths`).
- Search the codebase and design docs for other readers of the oracle DB path; point them at the helper.

#### Phase 3: Borg port default
**Model:** sonnet
- `borg/src/config.rs:879`: change `ServerConfig::default()` port from `8080` to `8181`. Update the test assertion at `:980` and the test fixture YAML at `:1171`.

#### Phase 4: Authentication on HTTP write routes (opt-in, topology-preserving)
**Model:** opus
- Add `auth_token: Option<String>` to `borg`'s `ServerConfig` (serde `auth-token`), default `None`. The field holds a **secret reference** (env-var name or file path), resolved at startup via `vault::config::resolve_secret` - the same mechanism `telegram.bot-token` and `ntfy.token` use. It does **not** hold a literal token in YAML.
- Gate all three write routes - `/ingest` (`routes::ingest`), `/ingest/file` (`routes::ingest_multipart`), and `/note` (`routes::note`) - behind the check. The `/health` and `/health/audit` GET routes stay open. When the resolved token is `Some`, require a matching `Authorization: Bearer <token>` header and reject with `401` on mismatch; when `None`, behavior is unchanged.
- The auth check must run **before any intake write** (before `record_received_with_sidecar`), so an unauthenticated request never creates a receipt or sidecar. This is a deliberate, principled exception to borg's durable-capture-at-the-door invariant: a `401` is a *refused* request, not a *dropped* input. The invariant guarantees that anything borg accepts is durably recorded; it does not require recording requests rejected at the HTTP boundary. Do not "fix" this by moving the check after the receipt write - that would let an unauthenticated caller fill the receipts DB and intake sidecars with junk.
- At daemon startup, if the bind host is non-loopback and the resolved token is `None`, emit a `WARN` naming the exposure.
- Thread an optional token field through the browser extension (`options.js`, `popup.js`) so it sends `Authorization` when configured.
- Document the LAN/Tailscale posture: default bind stays `0.0.0.0` to preserve the laptop-to-desk ingest path; the token is the supported way to lock it down.

#### Phase 5: Docs, comments, and vault reconciliation
**Model:** sonnet
- Repo `CLAUDE.md:82`: correct the claim that successful ingests append to `system/views/borg-ledger.md`; describe the receipts-SQLite-authoritative + `.base` view model.
- `borg/src/pipeline.rs:266-280`: remove the stale dual-write / markdown-DLQ comments.
- `cortex/src/sweep.rs:428`: remove or repoint the broken `[[borg-dashboard]]` link.
- Live vault `~/repos/scottidler/obsidian/CLAUDE.md`: update separate-binary language, config roots (`~/.config/sb/`), ledger location, retired dashboard, and the top-level directory contract (add `entities/`).
- Live vault `~/repos/scottidler/obsidian/home.md:11-14`: repoint `[[borg-ledger]]` to the current `.base` view; remove the dead `[[borg-dashboard]]` link.
- Add a short note in the vault documenting the current model: receipts SQLite is authoritative; `~/.local/share/sb/borg/borg-ledger.md` is operational history; `system/views/borg-ledger.base` is the vault-facing view; the dashboard markdown is retired.

#### Phase 6: Typed failure-stage classification
**Model:** opus
- Replace the substring matching in `borg/src/pipeline.rs:310-326` (`classify_terminal_failure`) with the typed `PipelineError` / `FailureStage` already defined in `borg/src/pipeline/error.rs`.
- Thread the typed stage to terminal receipt write; keep free-form reason text as a detail field, never as the classifier.
- Add a test asserting each `PipelineError` variant maps to its intended `FailureStage`.

#### Phase 7: Cold-note sweep scoping
**Model:** sonnet
- In the cold-sweep candidate selection (`cortex/src/sweep.rs`), exclude `type: daily` notes and the `journal/` subtree from the default report, consistent with the ingested-only governance posture.
- Regenerate `system/views/cold-notes.md` and confirm journal entries no longer appear.

#### Phase 8: SQLite immediate-transaction honesty
**Model:** sonnet
- `vault/src/search/vector.rs`: in `upsert_embeddings_batch` (`:361-363`) and `swap_transcript_chunks` (`:404-405`), replace `let tx = self.conn.transaction()?; tx.execute_batch("BEGIN IMMEDIATE;").ok();` with `let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;`. Add `use rusqlite::TransactionBehavior;`.
- Update the surrounding comments so they describe what the code does (acquires the write lock at `BEGIN`).
- This is hygiene, not a concurrency fix: cortex is the sole writer, so the deferred lock never deadlocks today. The value is that the code stops claiming `IMMEDIATE` semantics it does not have and stops swallowing a SQL error.

#### Phase 9: Module decomposition
**Model:** opus
- Split `borg/src/pipeline.rs` into a `pipeline/` module: stage orchestration, fetch/extract adapters, kind routing, quality gates, note publication, receipt terminal-state handling, notification/reporting.
- Split `vault/src/search.rs` into a `search/` module (it already has a `search/` dir with `vector.rs`): schema/migrations, note indexing, FTS query, cold-note selection, governance stats, vector ops, graph ops.
- After both splits, lower `BLOAT_MAX_LINES` in `.otto.yml` to the new ceiling.
- Run `otto ci` after each split.

## Alternatives Considered

### Alternative 1: Bind `/ingest` to `127.0.0.1` by default (the handoff doc's recommendation)
- **Description:** Change `ServerConfig` default host from `0.0.0.0` to `127.0.0.1`.
- **Pros:** Closes the unauthenticated-exposure surface by default.
- **Cons:** Breaks the operator's actual topology. The laptop runs the Firefox extension POSTing over the LAN to `desk.lan:8181`; loopback-only would silently kill that ingest path the moment it shipped.
- **Why not chosen:** A default that breaks the running deployment is worse than the exposure it closes. Opt-in token auth (Phase 4) closes the gap without breaking LAN ingest.

### Alternative 2: Keep `oracle.yml` `db-path` configurable; teach cortex to read oracle's config
- **Description:** Retain the override; have cortex load `oracle.yml` to discover the path.
- **Pros:** Operator can relocate the DB (e.g., onto a different disk).
- **Cons:** Couples cortex to oracle's config loader; the desync risk remains if either loader changes; more code for a capability no current install uses (live config uses the default).
- **Why not chosen:** Removing the override (Phase 2) eliminates the desync by construction. If relocation is ever needed, it should live in a shared config location both crates read, not in `oracle.yml` alone.

### Alternative 3: Hard-fail the daemon on non-loopback bind without a token
- **Description:** Refuse to start when host is non-loopback and `auth_token` is `None`.
- **Pros:** Strongest default posture.
- **Cons:** A config-sync hiccup that drops the token bricks ingest until corrected; operationally brittle for a self-hosted daemon.
- **Why not chosen:** A startup `WARN` (Phase 4) surfaces the risk without a failure mode that silently stops ingestion. Hard-fail can be revisited once the token is universally deployed.

## Technical Considerations

### Dependencies

- No new external crates. `str::floor_char_boundary` is stable and already used. `rusqlite::TransactionBehavior` is already a dependency.
- New internal coupling: `borg` and `sb` gain a dependency on `vault::text` (both already depend on `vault`). `cortex` and `oracle` both call `vault::paths::oracle_db_path()` (both already depend on `vault`).

### Performance

- `truncate_with_ellipsis` allocates only when a cut occurs; `truncate` borrows when no cut is needed. Truncation sites are previews/log lines, not hot loops.
- `transaction_with_behavior(Immediate)` acquires the write lock at `BEGIN` instead of first write. Inference still runs outside the transaction, so the held-lock window is unchanged (still under the 200 ms budget asserted by the Phase A5 regression test).

### Security

- Phase 4 is the security-relevant change: opt-in bearer-token auth on write routes, a startup warning on unauthenticated public bind, and extension support for sending the token. The default remains backward-compatible (no token, no auth) to preserve the running deployment; the operator opts in.

### Testing Strategy

- Phase 1: unit tests in `vault/src/text.rs` covering the exact multi-byte cases that panic today, plus boundary cases (at-limit, one-over, zero, empty).
- Phase 4: a test per write route (`/ingest`, `/ingest/file`, `/note`) asserting `401` on a missing/wrong token when a token is configured, and that a rejected request creates **no** receipt and **no** sidecar (the check runs before intake). Confirm the no-token path is unchanged.
- Phase 6: a table test mapping each `PipelineError` variant to its `FailureStage`.
- Phase 8: the existing 200 ms vector-batch regression test must still pass; add a comment-level assertion that the transaction is `Immediate`.
- All phases: `otto ci` (`whitespace -r`, bloat, `cargo check/clippy/fmt/test --workspace --features vec`) is the gate after each phase.

### Rollout Plan

- Standard `bump` + `otto deploy`. Phase 4's extension change means that phase's release is the one extension re-sign in this set; the rest are daemon/CLI-only.
- Phase 5's live-vault edits are outside the repo writable root and may need elevated filesystem permission; they propagate to other hosts via Syncthing.
- Phase 2's oracle-config change: confirm the live `oracle.yml` does not set a custom `db-path` before removing the field (it does not, per the review snapshot), so no data move is required.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Phase 4 token misconfiguration drops ingest | Low | Med | Default stays no-auth/backward-compatible; token is opt-in; startup WARN, not hard-fail |
| Phase 2 removes a `db-path` an install actually uses | Low | High | Verify live `oracle.yml` has no custom `db-path` before removing the field; it does not in the reviewed snapshot |
| Phase 9 decomposition introduces a regression | Med | Med | Large test suite; `otto ci` after each split; decomposition lands last, after correctness fixes |
| Phase 5 live-vault edit blocked by sandbox permissions | Med | Low | Acknowledge the path is outside the writable root; request elevated permission for those specific edits |
| A truncation site is missed | Low | Med | Workspace grep for residual string `[..` slices after Phase 1 replacements |

## Open Questions

- [ ] Phase 4: should the token live in `borg.yml` only, or also be surfaced by `sb doctor` as a posture check (linked/unlinked, token-set/unset on public bind)?
- [ ] Phase 7: exclude journal/daily entirely, or produce a separate cold-review report per cohort (ingested knowledge, journal/daily, entities, domain-specific)? The handoff doc offered both; this doc picks exclusion as the default and leaves the multi-report option open.
- [ ] Phase 2: is DB relocation a real operator need worth a shared-config field later, or is the fixed path sufficient indefinitely?

## References

- `docs/design/2026-06-09-codebase-review-handoff.md` (the source review)
- `docs/design/2026-06-03-receipts-log-legacy-markdown-excision.md` (why the markdown DLQ comments are stale)
- `docs/design/2026-03-21-oracle-mcp.md`, `docs/design/2026-06-05-graph-augmented-memory-implementation-notes.md` (oracle DB path intent)
- `distillers/src/validate.rs:54` (char-budgeted safe truncation, the model for the helper); `cortex/src/fabric.rs:47` (byte-budgeted safe truncation, correct for its token-estimate use but not a char count)
