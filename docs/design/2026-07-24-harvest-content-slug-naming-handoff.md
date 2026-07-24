# Handoff: content-derived slug naming for harvest notes

**Author:** Scott Idler (via agent)
**Date:** 2026-07-24
**Status:** Ready to build (decision made with Scott; not yet implemented)
**Relates to:**
- `2026-07-17-harvest-clyde-sessions.md` (harvest pathway #1; produces these notes)
- `2026-07-20-harvest-completion.md` (shipped v0.12.0-0.12.1; the run that generated the notes below)

## Problem

Harvest note filenames are slugified from clyde's generic, haiku-generated
session titles (`session.rs:206`: `format!("{}.md", sanitize_filename(&title))`).
Those titles are generic and repeat constantly, so filenames collide immediately.

Evidence from the live 60d backfill (121 harvest notes as of 2026-07-24):

- Collision suffixes already firing hard: `review-okta-auth-security-changes-7.md`,
  `-8.md`, `review-slack-thread-2.md`, `review-github-workflows-for-security-vulnerabilities-2.md`,
  `review-plugin-sync-script-for-security-vulnerabilities-2.md`.
- The stem `review-security-vulnerabilities` appears 6 times; `review-slack-thread` 3 times.
- The `-N` suffix (applied in `super::atomic::resolve_publish_path`) is
  **order-dependent, hence nondeterministic**: a re-harvest can renumber notes,
  breaking the idempotency the harvest watermark design requires.

The band-aid is the bug. The fix is a deterministic, collision-scarce scheme
whose name reflects what the note is actually about.

## Decision (made with Scott, 2026-07-24)

1. **Filename = a pure content-derived slug.** No date prefix, no repo prefix.
   The repo and date already live in frontmatter and in the repo hubs; the
   filename should carry the *subject*. Scott: "based more on what the contents
   of the note are about."

2. **The distiller emits the slug.** The Stage-2 session distiller already reads
   the full transcript to produce Summary/Claims/tags. The same LLM pass emits a
   4-7 word kebab-case slug naming the real subject/outcome. Examples produced by
   hand from three real notes:

   | clyde title (today's filename) | content-derived slug |
   |---|---|
   | Review Slack thread | `slack-cli-idcache-groups-list-vs-string-bug` |
   | Review CI workflow changes for security | `gha-uv-sync-workdir-inputs-injection-review` |
   | Review security changes in CI workflows | `gha-project-layout-detection-fail-closed-hardening` |

3. **A collision means the notes are associated, not that we need a number.**
   Scott: "if they collide, they are probably associated." Two sessions distilling
   to the same content-slug are almost certainly about the same subject, so a
   collision is an **association signal**: fold/cluster them into one note (the
   way thread-clustering already merges same-cwd sessions) or cross-link, and
   record both `cortex-session-ids`. Do NOT mint a `-N` sibling. The `-N` path in
   `resolve_publish_path` should be removed for harvest notes in favor of this
   association behavior. (The exact mechanism -- merge into the existing note vs.
   emit a cross-linked follow-up -- is the one open design call below.)

## Implementation plan

### Phase 1: slug in the contract + pattern
- Add `pub slug: Option<String>` to `Distilled` (`vault/src/distilled.rs:13`).
- Add a `slug:` field to the distill-session pattern SCHEMA and RULES
  (`borg/patterns/distill-session.md`) AND the reduce variant
  (`borg/patterns/distill-session-reduce.md`, which produces the final merged
  `Distilled` for multi-chunk sessions). The chunk variant
  (`distill-session-chunk.md`) does not need it -- the reduce pass names the whole.
  Pattern rules for the slug: lowercase, hyphenated, 4-7 significant words, name
  the concrete subject/outcome (the specific bug, the specific decision, the
  system + what happened), never generic filler ("review", "session", "changes"
  alone). Deterministic phrasing instructions so re-runs are stable in practice.
- Deploy the patterns (`~/.config/sb/patterns/` via `otto deploy`).

### Phase 2: publish path uses the slug
- `borg/src/pipeline/session.rs:206`: derive the filename from
  `distilled.slug`, falling back to the title-slug only when the LLM omitted a
  slug (log a WARN on fallback). Keep `hygiene::sanitize_filename`.
- Persist the chosen slug in frontmatter (a `slug:` key) so it is stable across
  re-harvest and so the collision-association check has something to match on.

### Phase 3: collision = association
- Replace the `resolve_publish_path` `-N` behavior for harvest with the
  association path: on an existing note with the same content-slug, merge the new
  session into it (append to `cortex-session-ids` / the `## Sessions` block,
  union claims) OR emit a cross-linked follow-up -- see open question. Never `-N`.
- Determinism: same session -> same slug. Because the slug is LLM-generated
  (nondeterministic), it is generated ONCE at first distill and stored in
  frontmatter; re-harvest reuses the stored slug. The load-bearing identity
  anchor stays the **input-body-hash** from the harvest watermark design, NOT the
  slug -- the slug is display/addressing only.

### Phase 4: regenerate the 121 existing notes
- The existing notes carry generic-title filenames. Regenerate them under the new
  scheme: `rkvr` the old files, then re-distill (a bounded `sb borg harvest
  --force` over the window, or a targeted migration). Confirm no `-N` names
  remain and that former collisions became associations.

## Acceptance criteria
- [ ] `Distilled.slug` exists; distill-session + reduce patterns emit it.
- [ ] A harvest note's filename is its content-slug; `slug:` is in frontmatter.
- [ ] The three sample sessions above produce distinctive, non-generic slugs.
- [ ] Zero `-N`-suffixed harvest filenames exist after regeneration.
- [ ] Two sessions that would collide on slug associate into one note (extra
      `cortex-session-ids`), not two numbered files.
- [ ] Re-harvesting an already-published session reuses its stored slug (stable
      filename; no churn).
- [ ] `otto ci` green; tests cover slug fallback and the association path.

## Code pointers
- Filename derivation: `borg/src/pipeline/session.rs:206` (`title` -> filename)
  and `:211` (`super::atomic::resolve_publish_path`, the `-N` suffixer).
- Contract: `vault/src/distilled.rs:13` (`Distilled`).
- Patterns: `borg/patterns/distill-session.md`, `distill-session-reduce.md`
  (source of truth; synced to `~/.config/sb/patterns/`).
- Harvest publish/cluster: `borg/src/harvest/` (thread-clustering is the existing
  precedent for merging associated sessions).

## Open questions for the next agent
1. **Collision mechanism:** merge the second session into the existing note
   (grow `cortex-session-ids`, union claims -- matches thread-clustering), or emit
   a distinct cross-linked follow-up note? Merge is simpler and matches Scott's
   "they are probably associated"; confirm with him if the two sessions look
   genuinely distinct despite the shared slug.
2. **Slug length / shape:** 4-7 words is a starting point; tune against the real
   corpus so slugs stay filesystem- and Obsidian-friendly.
3. **Regeneration blast radius:** full 60d re-distill (LLM cost) vs. a
   rename-only migration that reuses the already-distilled bodies (cheaper, no new
   LLM calls -- the bodies are fine, only the filenames are wrong). Prefer the
   rename-only migration if the distilled content is already good; only re-distill
   to obtain slugs the old notes never had.
