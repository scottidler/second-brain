# Sessions -> Vault Loop (the "dreaming" loop)

- **Date**: 2026-07-03
- **Status**: DRAFT (for review, not yet approved to build)
- **Owner**: Scott Idler
- **Origin**: Unlock #4 of the 2026-07-02 second-brain review. The vault->sessions half (`/vault-recall`, user-scoped oracle) shipped; this doc specs the missing sessions->vault half.

## Problem

Every day of Claude Code work produces hard-won knowledge - root causes, decisions, gotchas, working patterns - and today it evaporates. `clyde` catalogs it (~930+ sessions, FTS-searchable), but nothing distills it back into the vault as first-class knowledge alongside YouTube/blog ingests. The only return path built so far is a **manual step inside `/closeday`**, which (a) has never run because the ritual is not yet a habit, and (b) is bounded to "today's 1-2 most substantive sessions," so anything not caught that evening is lost.

The 2026-07-02 daily work (documented in `obsidian/notes/ai/2026-07-02-claude-sessions-summary.md`) had to be reconstructed by hand from `clyde session ls`. That reconstruction IS the target output; this loop produces it automatically.

## Goal

A background job that reads clyde's session catalog, selects sessions worth remembering, distills each into a vault note, and drops it through the normal cortex inbox so it is classified, embedded, and oracle-searchable like any other source. Claude's engineering days become a capture channel, exactly as YouTube and Pixel Discover are today.

This is explicitly the pattern from the Anthropic "memory and dreaming" talk already in the vault: background analysis of transcripts to enrich a memory store.

## Non-goals

- Not real-time. Nightly (or on-demand) is fine; this is reflection, not a hook in the hot path.
- Not a replacement for `/closeday`'s interactive recap - that stays for Scott's own words. This is the automated floor under it.
- Not summarizing trivial/chat sessions (tmux fixes, one-line lookups, auto-fired security reviews).

## Design

### Selection (what earns a note)
Pull `clyde session ls --since <last-run>` (JSON). Score each session; distill only those above a bar. Signals:
- `n-msgs` above a threshold (substantive, not a one-shot).
- cwd is a real repo (not a throwaway) and/or a design-doc / implementation thread.
- Exclude by title/first-prompt pattern: the auto `"Review this change for security vulnerabilities"` sub-sessions, bare `"sure"`/empty prompts, and pure navigational lookups (`clyde session search ...`).
- Optional: cluster sessions that share a cwd + time window into one "thread" note rather than N per-session notes (the 07-02 token-broker arc was 4 sessions = 1 thread). Thread-clustering is the higher-value shape and matches how the hand-written summary reads.

### Distillation
For each selected session/thread, feed the transcript (assistant text + user prompts, tools elided) to an LLM with a fixed "what was decided / what was learned / what is reusable" prompt - a new fabric pattern `distill-session`, sibling to `distill-thread`. Output: 5-15 lines, tight, no narration. Cost control: cap transcript tokens (head+tail windowing for very long sessions), use a cheap-but-capable model for the distill pass.

### Landing in the vault
Write each distillation to `inbox/YYYY-MM-DD-<slug>.md` with:
```
type: note
domain: <let cortex classify, or infer from repo>
origin: generated
status: unread
```
cortex then classifies, links, embeds, and promotes it - no special-casing. Generated content already lands in `notes/ai/` per the origin-values schema. A daily roll-up note (like the hand-written 07-02 summary) is optional on top.

### Trigger
Two entry points, same core:
- **On-demand**: `sb <cmd> --since 1d` (name TBD - `recall`, `dream`, or fold into an existing subcommand). This is what `/closeday` calls instead of doing its own inline distill.
- **Nightly cron**: the durable version, so the loop survives a skipped ritual. Runs `--since <last-successful-run>` so nothing is missed and nothing is double-processed (persist a watermark).

### Idempotency / dedup
Persist last-processed watermark (timestamp or last session-id set) so re-runs don't re-distill. Content-hash the distillation to avoid duplicate inbox notes if a session is re-selected.

## Build shape (Rust)

Lives in the second-brain workspace. Rough decomposition (no estimates):
- **`clyde` catalog reader**: shell out to `clyde session ls/search --db ... ` JSON, or read `sessions.db` directly. Shelling out is the lower-coupling start.
- **Selector**: scoring + thread-clustering over the catalog rows.
- **Transcript loader**: read the `.jsonl`, extract user+assistant text, windowing.
- **Distiller**: fabric `distill-session` pattern via the existing fabric plumbing (Groq/Anthropic per the model-routing already in borg).
- **Inbox writer**: frontmatter + body -> `inbox/`, hand off to cortex.
- **Watermark store**: small state file or a row in an existing DB.

## Open questions (need Scott)
1. **Per-session notes vs. thread-clustered notes vs. one daily roll-up?** (Recommendation: thread-clustered inbox notes + an optional daily roll-up.)
2. **Which model for the distill pass**, given cost at ~10 substantive sessions/day?
3. **Confidentiality**: work-repo sessions (tatari-tv) distilled into the Syncthing'd personal vault - same decision as the meeting-audio one. Scope-tag `work`, or exclude tatari-tv cwds from distillation?
4. **New `sb` subcommand name**, and does `/closeday` delegate to it?

## Success criteria
- A nightly run turns a day like 2026-07-02 into the same set of notes the hand-written summary captured, with zero manual effort.
- `reviewed`/oracle-hit counts on generated session notes are non-zero a month in (i.e. they actually get consumed, not just written - the exact write-only trap this whole effort is fighting).
