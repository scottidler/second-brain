# Design Document: Cortex Linker URL Corruption — Guard + Repair

**Author:** Scott Idler
**Date:** 2026-06-11
**Status:** Implemented
**Review Passes Completed:** 5/5
**Shipped in:** v0.8.69 + the structure-aware-linker fix (commits 59babcf, 7f753b8, d495fee)

## Summary

The cortex daemon's link sweep (`cortex/src/linking.rs`) auto-inserts `[[wikilinks]]` at the first body mention of any known target (note titles, glossary concepts/aliases, entity notes). It has no guard against matching text **inside a URL, a markdown link/embed target, an HTML tag/attribute, or an inline-code span**. After the `feat(entities)` + `feat(link)` features landed 2026-06-06, 137 domain "entity" notes (`youtube-com.md`, `github-com.md`, …) became link targets, so the next sweep rewrote `youtube.com` *inside iframe `src` URLs* into `[[youtube-com|youtube.com]]` — corrupting the URL and blanking the embed. ~926 YouTube embeds and **1201 notes** total are affected. This doc covers (1) a context guard at the mutation point with tests, (2) a one-time bash migration to repair corrupted URLs, scoped strictly to wikilinks inside URL/link/tag context, and (3) the daemon stop → fix → ci → install → migrate → restart lifecycle.

## Problem Statement

### Background

`cortex/src/linking.rs` provides two entry points:

- `lint_linking(notes, config) -> Report` — detects mentions via `LoweredBody::find_mention` and emits `Fix::AddWikilink { target, surface, context }` violations.
- `apply_linking(vault_root, notes, config) -> usize` — runs the lint, groups fixes per file, then calls `insert_first_wikilink(content, target, surface)` which **independently** re-finds the first `\b{surface}\b` in the body and wraps it. Writes are atomic (`vault::note::write_atomic`).

The cortex **daemon auto-applies** link fixes on its periodic tick (`daemon.rs`, "link: applied wikilink fixes"). So the corruption is not a one-shot CLI mistake — it recurs every sweep, which is why a clean manual edit to a note is re-corrupted within minutes (observed live during diagnosis).

On **2026-06-06** two features shipped together:

- `1392d75 feat(entities): LLM entity discovery into entity-proposals.yml` — produced 137 domain notes under `entities/` (`youtube-com.md`, `github-com.md`, `instagram-com.md`, `arxiv-org.md`, …). Their stems become link targets in `lint_linking`'s `note_titles` list.
- `00687ce feat(link): glossary concepts + piped alias wikilinks` — added piped `[[slug|surface]]` emission.

Once a target named `youtube-com` existed, the linker matched the literal `youtube.com` everywhere it appeared — including inside `<iframe src="https://www.youtube.com/embed/…">` — and rewrote it to `https://www.[[youtube-com|youtube.com]]/embed/…`. Obsidian cannot parse that as a URL, so the iframe loads `about:blank` and the embed is a blank box.

This matches the operational history exactly: embeds "worked earlier this year" and broke ~5 days ago, the first time the link sweep ran after 2026-06-06. (Obsidian was ruled out: a clean raw iframe renders fine in reading view; the changelog has nothing about iframe sandboxing.)

### Problem

The linker corrupts any URL/link/code/HTML-attribute occurrence of a known target's surface form. The existing guards in `find_mention` (lines 351–362) and `insert_first_wikilink` (lines 414–417) only skip text *already inside `[[ ]]`* — there is no notion of "this position is inside a URL / markdown link target / HTML tag / inline code, leave it alone."

### Goals

- Make the linker structure-aware: never insert a wikilink where the match falls inside a Markdown/HTML structural construct (URL, autolink, link/image destination, HTML tag/attribute, code span, reference-style link definition).
- Repair the 1201 already-corrupted notes losslessly (restore the original surface text).
- Sequence the deploy so the running daemon cannot re-corrupt the repaired vault.
- Cover the fix with tests that pin the exact failure (`youtube.com` inside `https://…` and inside `![](…)` must not be linked).

### Non-Goals

- **No iframe → native-embed conversion.** A clean raw `<iframe>` renders fine; the format was never the problem. Borg's `generate_embed_code` is unchanged.
- **No change to borg.** The borg daemon writes raw iframes with clean URLs; it is unaffected and keeps running.
- Not redesigning the entity/glossary linking feature itself (whether bare domains *should* be link targets at all is an Open Question, not this doc's mandate).
- (Revised after review: the migration DOES get `--dry-run` + a manifest + a dirty-tree guard — see Phase 2. The destructive-default carve-out in `cli.md` applies, and the already-dirty obsidian worktree makes git-only recovery insufficient.)

## Proposed Solution

### Design Principle: structure-aware linking

The root flaw is that the linker treats a note body as **flat prose**. It is not — it is Markdown with embedded structure: URLs, HTML tags/attributes, code spans, link/image destinations. A target's surface form appearing inside one of those constructs is **syntax, not a prose mention**, and must never be wrapped in `[[ ]]`. The fix is to make the linker structure-aware: detect when a candidate match falls inside a structural construct and decline to link it. This is a single general predicate, not a YouTube/URL special-case — it protects every construct kind, so a concept slug or a person's name appearing in a `src=`, an autolink, or a `` `code` `` span is equally safe.

### Overview

Two code/data changes plus an ordered deploy:

1. **Guard** — a single shared predicate `inside_structure(content, start, end) -> bool` consulted at both the detection point (`find_mention`) and the mutation point (`insert_first_wikilink`). The mutation-point check is load-bearing; the detection-point check just suppresses noise violations. Both call sites must **iterate occurrences** and link the first *non-structural* one (today they single-find the first match via `body_lower.find` / `re.find`; that must become an occurrence loop that skips structural hits).
2. **Repair migration** — `bin/repair-url-wikilinks` (bash + python3 for the regex pass) that de-links `[[slug|surface]]` → `surface` (and `[[slug]]` → `slug`) **only** where the wikilink sits in a structural span. Lossless because the linker preserved the original surface form as the pipe display text.
3. **Lifecycle** — cortex is already stopped; land fix → `otto ci` → `otto install` → run migration once on the daemon host (Syncthing propagates) → `systemctl --user restart cortex`.

### Architecture

```
cortex/src/linking.rs
  inside_structure(content, start, end) -> bool        [NEW shared helper]
    ├─ inside inline code (`...`), fenced block (```), or indented (4-space/tab) code block
    ├─ inside an HTML tag / attribute value   (between < and >, incl. src="...", href="...")
    ├─ inside an HTML comment        (<!-- ... -->)
    ├─ inside math                    ($...$ inline, $$...$$ block)
    ├─ inside a URL token            (see URL heuristic below)
    ├─ inside an autolink             (<https://...>)
    ├─ inside a markdown link/image destination or title  ("](" .. matching ")", SAME line)
    └─ inside a reference-style link definition  ([id]: https://...)

  URL heuristic — a match is "in a URL token" when ANY holds (Architect finding: the
  original ":// or www." test missed bare paths and other schemes):
    - the surrounding non-space run contains "://"             (https://, http://)
    - the run starts with "www."
    - the run contains a "<scheme>:" prefix                     (mailto:, xmpp:, …)
    - the matched domain is immediately followed by "/" or "?"  (bare path: youtube.com/watch?v=, github.com/x)

  GUARD and MIGRATION span sets DIFFER (Architect finding):
    - GUARD (Rust, prevents NEW links): ALL of the above, incl. code & math — never link syntax.
    - MIGRATION (repair, removes corruption): URL / autolink / link-destination / HTML-tag spans ONLY.
      It must NOT evaluate code spans: a user may have intentionally authored a [[wikilink]] inside a
      code fence in a tutorial note; the migration preserves that, while the guard still blocks NEW ones.

  find_mention(...)        -> ITERATE mentions; skip any where inside_structure is true; return first clean one
  insert_first_wikilink(...) -> ITERATE \b{surface}\b matches; wrap the first where inside_structure is false (else None)

bin/repair-url-wikilinks   [NEW one-time migration]
  for each *.md in vault:
    rewrite [[slug|surface]] -> surface, [[slug]] -> slug   when match is inside_structure
  report files changed + total replacements
```

### Data Model

No schema changes. The corruption is purely textual, but the corrupted forms are **more varied than first assumed** (Staff Engineer scan of the live vault):

- **Piped** (common): `https://www.[[youtube-com|youtube.com]]/embed/mT1tg6SQ_Ag` → `[[slug|surface]]` carries the original text as `surface`.
- **Non-piped** (164 files / 179 occurrences): `https://[[american-football-academy]].com/...` — `[[slug]]` with matched==slug, so the slug *is* the original text.
- **Two-per-URL** (51): `…/r?u=http://[[bar-com|bar.com]]` after a first corrupted segment.
- **Nested** (11): `https://[[american-[[football]]-academy]].com/...` and `[[blog-[[langchain]]-com|blog.langchain.com]]` — the sweep matched an inner term first, then an outer term wrapped the already-linked text.

So the earlier "always piped / single-regex / lossless" claim is **wrong**. The repair is an **iterative innermost de-link, scoped to structural spans**:

```
within each structural span (URL / autolink / link-destination / HTML-tag):
  repeat until no `[[ ]]` remains in the span:
    find the INNERMOST wikilink  ( \[\[[^\[\]]+\]\] — no brackets inside )
    replace [[slug|surface]] -> surface   (piped)
    replace [[slug]]         -> slug       (non-piped; matched==slug, so slug is the original text)
```

Innermost-first makes nesting lossless: `[[american-[[football]]-academy]]` → resolve `[[football]]`→`football` → `[[american-football-academy]]` → `american-football-academy` → original `https://american-football-academy.com`. Operating **only inside structural spans** (not whole lines) preserves legitimate `[[note|display]]` links elsewhere on the same line (Staff Engineer: real notes mix both on one line).

### API Design

New private helper in `cortex/src/linking.rs`:

```rust
/// True if the byte range [start, end) in `content` is inside a Markdown/HTML
/// structural construct where a wikilink must never be inserted: a code span
/// (inline or fenced), an HTML tag / attribute value, a URL token, an autolink,
/// a link/image destination or title, or a reference-style link definition.
fn inside_structure(content: &str, start: usize, end: usize) -> bool;
```

- `find_mention` (detection): iterate body occurrences of the term; skip any where `inside_structure` is true; return the first clean `(context, surface)`, or `None` if every occurrence is structural. (Today it single-finds via `body_lower.find`.)
- `insert_first_wikilink` (mutation): iterate `\b{surface}\b` matches; wrap the first where `inside_structure` is false; return `None` if none are clean. (Today it single-finds via `re.find`.) **This is the load-bearing change** — detection and mutation find positions independently, so a guard only in detection would still let mutation wrap a structural match.

**Offset convention (Staff Engineer finding — detection and mutation use different base offsets):** `inside_structure(text, start, end)` treats `text` as whatever string the caller is scanning, with `[start, end)` offsets into THAT string. `find_mention` passes `self.body` + body offsets. `insert_first_wikilink` searches `body = &content[body_start..]` and passes that same `body` slice + the regex match's `mat.start()/mat.end()` (offsets into `body`), NOT the `content`-absolute `abs_start/abs_end`. One slice, one offset base, no footgun. All byte-offset work uses char-boundary-safe operations (`floor_char_boundary`/`get`), per the repo's UTF-8 footgun rule — never raw `&s[a..b]` on computed offsets. Frontmatter is excluded because both callers scan only the post-frontmatter body.

### Implementation Plan

#### Phase 1: Guard predicate + wiring + tests
**Model:** opus
- Add `inside_structure` to `cortex/src/linking.rs` covering the full taxonomy + URL heuristic above (code/fenced/indented, HTML tag/attr, HTML comment, math, URL token, autolink, link destination/title, reference def). Localized intra-line bounding for `](...)` so unrelated `]` + `(` on the same/other lines don't misfire (Architect finding).
- Wire it into `find_mention` (iterate, skip-to-clean) and `insert_first_wikilink` (iterate, wrap-first-clean / `None`).
- Tests in `cortex/src/linking/tests.rs` (sibling file per repo convention) — the full positive/negative matrix is in **Testing Strategy** below; Phase 1 is not done until every row passes.

#### Phase 2: Repair migration
**Model:** opus
- `bin/repair-url-wikilinks` (bash wrapper + python3 transform; per the no-Rust-migrations rule). Models on `bin/migrate-receipts` (the existing migration precedent: it has `--dry-run` and refuses active daemons).
- **Span-based, iterative-innermost repair** (see Data Model): locate structural spans (URL token / autolink / link-destination / HTML-tag — NOT code/math), and within each span collapse the innermost `[[ ]]` repeatedly (`[[s|surf]]`→`surf`, `[[s]]`→`s`) until none remain. This is **not** a flat regex — it must handle nested (11) and two-per-URL (51) forms without swallowing the URL middle. Everything outside a structural span is untouched, so same-line legitimate wikilinks survive (Staff Engineer finding).
- **Safety flags** (Staff Engineer finding — the obsidian repo is already dirty, so "recoverability is git" alone is too weak):
  - `--dry-run` (DEFAULT): write nothing; emit a machine-readable manifest (`path`, line, before→after) of every intended change. The destructive run requires explicit `--apply`. (This is the `cli.md` destructive-default carve-out, not a rule violation.)
  - Refuse to run with a **dirty obsidian worktree** unless `--force`; print the manifest path so the user reviews before `--apply`.
  - Refuse to `--apply` while `cortex.service` is active on this host.
- Emit a summary: files scanned, files changed, total replacements, breakdown by domain, count of nested/non-piped/two-per-URL forms repaired, and any span the parser could not confidently resolve (left untouched, reported).
- Atomic per-file write (temp-in-dir + rename) since the vault is Syncthing'd.

#### Phase 3: Deploy + verify
**Model:** sonnet
- **Stop the daemons via systemd, not `sb cortex daemon --stop`** (Staff Engineer finding: `--stop` only prints instructions, it does not stop the service). On EVERY mesh node (Architect finding — else a still-sweeping node re-corrupts and syncs back), run `systemctl --user stop cortex` and verify `systemctl --user is-active cortex` → `inactive/failed`. Per known topology only desk runs cortex (laptop is a borg client), but verify each node.
- **Quiesce borg during the repair** (Staff Engineer finding): `systemctl --user stop borg` on desk too — atomic writes prevent torn files, not a lost update if borg reingests a target file mid-migration. Restart it after.
- `otto ci` (clippy+fmt+test) green.
- `otto install` (builds + installs `sb` with the guard). Note `otto install` does NOT restart units — the systemctl steps are manual.
- **Dry run first:** `bin/repair-url-wikilinks --dry-run` on desk; review the manifest — confirm intended-change count is in the expected range (~1200+ files), every change is inside a structural span, the nested/non-piped/two-per-URL counts match the scan, and zero changes touch a prose-context `[[...]]`.
- **Apply:** `bin/repair-url-wikilinks --apply` (refuses if cortex active or worktree dirty without `--force`).
- `systemctl --user restart cortex && systemctl --user restart borg` (only after the guarded `sb` is installed); confirm a subsequent cortex sweep does NOT re-corrupt (spot-check tuxedo + a scratch note with embeds in URL, code, and prose).
- `git commit` the vault repair in the obsidian repo (review the diff against the dry-run manifest first).

## Alternatives Considered

### Alternative 1: Convert all embeds to native `![](url)` and change borg's generator
- **Description:** Switch the 953 iframe notes to Obsidian native embeds and update `generate_embed_code`.
- **Pros:** Native embeds are slightly terser.
- **Cons:** Solves the wrong problem — the iframe format was never broken (proven: a clean raw iframe renders). Leaves the linker still corrupting URLs (and now `![](…)` targets too). Large, unnecessary migration.
- **Why not chosen:** Disconfounded by the clean-iframe isolation test; the corruption is the sole cause.

### Alternative 2: Exclude `entities/` (and bare domains) from being link targets
- **Description:** Add `entities/*` to `config.targets.paths.exclude`, or stop auto-linking bare domains entirely.
- **Pros:** Removes the noisiest target class; arguably "github.com" mentions shouldn't be links anyway.
- **Cons:** Doesn't fix the underlying bug — any target whose surface appears in a URL (a concept slug, a person's name in a URL slug) would still corrupt. Policy choice orthogonal to correctness.
- **Why not chosen:** Necessary-but-insufficient; tracked as an Open Question. The guard is the correctness fix; target curation is a separate tuning decision.

### Alternative 3: Block all HTML/`<iframe>` blocks from linking, leave plain URLs alone
- **Description:** Only guard inside `<...>` tags.
- **Pros:** Smallest change; fixes the iframe case.
- **Cons:** Misses bare-URL corruption (`https://github.com/...` in prose) and `![](…)`/`[](…)` targets, both present in the 1201 affected notes.
- **Why not chosen:** Incomplete; the guard must cover the full structural taxonomy (code, math, HTML tags/comments, URLs incl. bare paths, autolinks, link destinations, reference defs), not just `<...>`.

## Technical Considerations

### Dependencies
- No new crates. python3 for the migration transform (already a system dep).
- Shares `vault::note::write_atomic` semantics for the migration's file writes.

### Performance
- `inside_structure` is O(span scan) per candidate match, bounded by line/token length; negligible vs the existing per-note lowercase+offset-map pass.

### Security
- None. Purely local text transforms on a user-owned vault.

### Testing Strategy

The whole point of this work is to never silently corrupt the vault again, so the filters are proven by an explicit positive/negative matrix, not spot checks. **Both suites must be green before Phase 3.** Every row is a named test. Targets used in fixtures: domain entity `youtube-com`/`github-com` (surface `youtube.com`/`github.com`), a concept `rust`, and an alias `Retrieval-Augmented Generation -> rag`.

#### Suite A — Guard (`cortex/src/linking/tests.rs`)

Drives the full pipeline (`lint_linking` + `apply_linking`) over a fixture note and asserts the resulting body.

**Negative — match is inside structure, MUST NOT be wikilinked:**

| ID | Body fixture | Assert |
|----|--------------|--------|
| A-N1 | `<iframe src="https://www.youtube.com/embed/abcdefghijk"></iframe>` | `youtube.com` unchanged |
| A-N2 | `![](https://www.youtube.com/watch?v=abcdefghijk)` | unchanged |
| A-N3 | `[docs](https://github.com/torvalds/linux)` | `github.com` unchanged |
| A-N4 | `<https://youtube.com/x>` (autolink) | unchanged |
| A-N5 | `prose then https://youtube.com/watch?v=x end` (bare scheme URL) | unchanged |
| A-N6 | `youtube.com/watch?v=x` (bare path, no scheme/www) | unchanged |
| A-N7 | `see github.com/torvalds for code` (domain + `/`) | unchanged |
| A-N8 | `mailto:hi@youtube.com` (URI scheme) | unchanged |
| A-N9 | `[ref]: https://youtube.com/x` (reference def) | unchanged |
| A-N10 | inline code `` `youtube.com` `` | unchanged |
| A-N11 | fenced ```` ```\nyoutube.com\n``` ```` | unchanged |
| A-N12 | 4-space indented code block containing `github.com` | unchanged |
| A-N13 | inline math `$rust = 1$` | `rust` unchanged |
| A-N14 | `<a href="https://github.com">x</a>` | unchanged |
| A-N15 | `<!-- youtube.com note -->` (HTML comment) | unchanged |

**Positive — genuine prose mention, MUST still be linked:**

| ID | Body fixture | Assert |
|----|--------------|--------|
| A-P1 | `I prefer rust for systems work` | `[[rust]]` inserted |
| A-P2 | `I prefer Rust for systems work` (case differs) | `[[rust\|Rust]]` inserted |
| A-P3 | `uses Retrieval-Augmented Generation here` | `[[rag\|Retrieval-Augmented Generation]]` inserted |
| A-P4 | iterate-to-clean: `https://example.com/rust then later I use rust daily` | URL `rust` untouched; prose `rust` → `[[rust]]` |
| A-P5 | misfire guard: `array [1, 2] (rust is great)` | `[1, 2]` untouched; prose `rust` → `[[rust]]` |

**Edge / safety:**

| ID | Body fixture | Assert |
|----|--------------|--------|
| A-E1 | multibyte before URL: `café — https://youtube.com/x` | no panic; `youtube.com` unchanged |
| A-E2 | idempotency: run pipeline twice on a repaired note | 2nd run is a no-op (byte-identical) |

#### Suite B — Migration (`bin/repair-url-wikilinks`, fixture vault in a tempdir)

**Must-repair — corrupted inside a URL/destination/tag span, de-link to clean:**

| ID | Input | Expected |
|----|-------|----------|
| B-R1 | `https://www.[[youtube-com\|youtube.com]]/embed/abc` | `https://www.youtube.com/embed/abc` |
| B-R2 | `[t](https://[[github-com\|github.com]]/x)` | `[t](https://github.com/x)` |
| B-R3 | two per URL: `https://[[foo-com\|foo.com]]/r?u=http://[[bar-com\|bar.com]]` | both de-linked (non-greedy) |
| B-R4 | `<iframe src="https://www.[[youtube-com\|youtube.com]]/embed/abc">` | clean src |
| B-R5 | bare path: `[[youtube-com\|youtube.com]]/watch?v=x` (no scheme) | `youtube.com/watch?v=x` |
| B-R6 | nested: `https://[[american-[[football]]-academy]].com/x` | `https://american-football-academy.com/x` (innermost-first) |
| B-R7 | nested piped: `[[blog-[[langchain]]-com\|blog.langchain.com]]` in a URL | `blog.langchain.com` |
| B-R8 | non-piped: `https://[[american-football-academy]].com` | `https://american-football-academy.com` |
| B-R9 | corrupted URL + legit wikilink SAME line: `see https://[[github-com\|github.com]]/x on [[2026-06-10]]` | URL repaired; `[[2026-06-10]]` untouched (span-based, not line-based) |
| B-R10 | unresolvable span (parser not confident) | left untouched, reported in summary — never half-rewritten |

**Must-preserve — legitimate or out-of-scope, untouched:**

| ID | Input | Expected |
|----|-------|----------|
| B-P1 | prose `See [[some-note\|Some Note]] for details` | unchanged |
| B-P2 | prose `[[rust]]` | unchanged |
| B-P3 | user-authored inside inline code `` `[[youtube-com\|youtube.com]]` `` | unchanged (migration ignores code spans) |
| B-P4 | prose-context domain link `I use [[github-com\|github.com]] daily` (sentence, not a URL) | unchanged (default policy: URL-context only; see Open Questions) |

**Non-reversible guard:**

| ID | Input | Expected |
|----|-------|----------|
| B-X1 | non-piped `https://[[github-com]]/x` inside a URL | NOT de-linked to `github-com` (wrong domain); reported to `needs-manual-review`, left untouched |

**Real-data grounding (`--harvest`):** the migration buckets every real vault wikilink by structural shape (`HARVEST=1 bin/repair-url-wikilinks`); one verified representative per shape is pinned as an `H-*` selftest case (inputs lifted verbatim from notes). This surfaced two shapes the synthetic cases missed and now cover: a corrupted URL in an **indented list item** (repaired — indented lines are not protected; only fenced/inline code/math are), and **nested wikilinks in prose/embeds** (`![[a-[[claude-code]]-b.jpg]]`, `[[atavus-[[football]]|Atavus Football]]`) which are always corruption and are repaired by flattening the inner nesting while keeping the outer link/embed intact.

**Whole-vault verification (per the verify-by-discriminating-count rule):**
- `git` snapshot of the obsidian repo before running.
- Assert files-changed count ≈ 1201 (not exit-0), and `git diff` shows changes ONLY inside URL/destination/tag spans — zero hunks touching a prose-context `[[...]]`.
- Re-run the migration: second pass changes 0 files (idempotent).

### Rollout Plan
- cortex is stopped now (since diagnosis). It MUST stay down — **on every mesh node, not just desk** — until the guarded `sb` is installed, or a still-sweeping node re-corrupts the repaired vault and Syncthing replicates it back.
- Run the repair **once** on the daemon host (desk); Syncthing propagates the repaired markdown to other devices. Do NOT run per-host.
- **Dry-run + manifest review precede `--apply`** (the obsidian worktree is already dirty, so git-diff-after is not a clean safety net). The migration refuses to `--apply` with a dirty tree (without `--force`) or while cortex is active. Recovery of last resort is still git, but the manifest is the primary review gate.
- Quiesce borg during the apply; restart cortex + borg only after the guarded `sb` is installed.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Migration de-links a legitimate `[[note\|display]]` in prose | Low | High (data loss) | Scope strictly to structural spans (URL/`](...)`/HTML); never touch prose-context links. Verify via `git diff`. |
| Guard too aggressive, suppresses valid prose linking | Low | Med | Positive tests (A-P1..A-P5) assert genuine prose mentions still link; `inside_structure` only fires inside the enumerated structural constructs. |
| Daemon restarted before fix installed → re-corruption | Med | High | Lifecycle ordering is explicit; keep cortex stopped until `otto install` completes. |
| Rust guard and python migration diverge in span logic | Med | Med | Specify one span definition in this doc; mirror it in both; share the same test cases. |
| Corrupted URL breaks token detection in migration | Low | Med | The `https://` scheme + `www.` prefix survive corruption, so the URL token is still detectable around the `[[…]]`. Test against the real corrupted form. |
| URL heuristic misses bare paths / non-`://` schemes → keeps corrupting | Med | High | Heuristic extended to bare-path (`domain/` or `domain?`) and `scheme:` prefixes; tests A-N6/A-N7/A-N8 pin these. (Architect) |
| Migration strips a user-authored wikilink inside a code span | Low | High (intent loss) | Migration span set EXCLUDES code/math; only URL/dest/tag spans. Test B-P3. (Architect) |
| `](...)` heuristic misfires on unrelated `]`+`(` | Med | Med | Require `]` immediately followed by `(`, bound the scan to the line. Test A-P5. (Architect) |
| A non-desk mesh node keeps sweeping during migration | Med | High | Verify `cortex` inactive on every node before migrating; topology says only desk runs it but confirm. (Architect) |
| Non-piped `[[slug]]` in a URL de-linked to the wrong (slug) text | Low | Med | Non-piped means matched==slug, so the slug IS the original text — de-link is lossless. Tests B-R8/B-X1. |
| Nested wikilinks (`[[a-[[b]]-c]]`) mangled by a flat regex | Med | High | Iterative innermost de-link, not a single regex; 11 real cases. Tests B-R6/B-R7. (Staff Engineer) |
| Line-based rewrite destroys a legit same-line wikilink | Med | High | Repair is span-scoped, never line-scoped. Test B-R9. (Staff Engineer) |
| Dirty obsidian worktree makes post-hoc git-diff review unreliable | High | Med | `--dry-run` default + manifest reviewed before `--apply`; dirty-tree refusal without `--force`. (Staff Engineer) |
| `sb cortex daemon --stop` doesn't actually stop the daemon | High | High | Rollout uses `systemctl --user stop cortex` + verify inactive, not `sb cortex daemon --stop`. (Staff Engineer) |
| borg reingests a target file mid-migration (lost update) | Low | Med | Stop borg for the apply window; restart after. (Staff Engineer) |

## Open Questions
- [ ] Should `entities/` domain notes be link targets at all? (Alternative 2 — separate tuning.)
- [ ] Should the migration also strip prose-context domain wikilinks the sweep created (e.g. "I use [[github-com|github.com]] daily" in a sentence), or only URL-context ones? Default: URL/link/tag context only (unambiguous, lossless). Prose-context cleanup deferred pending the target-curation decision.
- [ ] Does any non-domain target (a concept slug, a person name) also appear inside URLs in the corpus? Quick grep before migration to confirm the repair regex catches non-domain piped links in URLs too.

## References
- `cortex/src/linking.rs` — `lint_linking`, `apply_linking`, `find_mention`, `insert_first_wikilink`
- `cortex/src/daemon.rs` — link-sweep tick that auto-applies fixes
- Commits `1392d75` (feat entities), `00687ce` (feat link glossary), both 2026-06-06 — the regression origin
- `docs/design/2026-06-06-configurable-retrieval-pipeline.md` — adjacent cortex/oracle config conventions

## Review Log
- **2026-06-11 — Architect (Gemini), Design Review.** Verified the independent detection/mutation claim against `linking.rs`. Surfaced: URL heuristic too weak (bare paths, `mailto:`); missing constructs (indented code, math, `](...)` misfire); migration must NOT strip code-span wikilinks (guard vs migration span sets differ); non-greedy regex for multi-corruption URLs; **stop cortex on ALL mesh nodes**. All folded into the taxonomy, migration scope, lifecycle, test matrix, and Risks above.
- **2026-06-11 — Staff Engineer (Codex), Design Review.** Scanned the live vault and **disproved the "always piped / lossless single-regex" claim**: 164 files non-piped, 51 two-per-URL, 11 nested forms. Drove: iterative-innermost span-scoped repair (not flat regex); **migration must be span-based, not line-based** (real lines mix corrupted URLs with legit wikilinks); **add `--dry-run` + manifest + dirty-tree refusal** (worktree already dirty; `bin/migrate-receipts` precedent); fix `inside_structure` offset convention (detection=body offsets, mutation=full-content offsets → pass the body slice to both); rollout must `systemctl --user stop cortex` (the `sb` `--stop` is a no-op) and quiesce borg. All folded into Data Model, Phase 2/3, API offset note, Rollout, test matrix (B-R6..B-R10), and Risks above.
