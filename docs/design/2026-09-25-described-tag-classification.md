# Design Document: Described Tag Classification

**Author:** Scott Idler (via agent)
**Date:** 2026-09-25
**Status:** Implemented (2026-09-27, branch `described-tag-classification`, phases 0-8). Pending before descriptions reach classifier.dev: Scott labels `gold-2.md`, approves the P4 backfill drafts, and P5's confirmation run passes.
**Review Passes Completed:** 5/5. Pass 2 (correctness) measured the free spend cap: a 159-note call returns 402, so batch sizing, a spend guard, and a vector screen for `tag-apply` were added. Pass 3 (clarity) pinned the embed and vector APIs by path:line and the slug-sorted shard rule. Pass 4 (edge cases) excluded gold notes from description samples, banned `"` in descriptions, failed loud on unmapped score keys, and added the in-place describe rewrite. Pass 5 (voice) lint clean; acceptance criteria run on main with output recorded.

## Summary

Borg and cortex tag every note through classifier.dev (Jev), but each tag reaches Jev as a bare slug with no definition, so vague tags (`work`, `life`, `homelab`) are near-invisible and borderline scores flip between calls. This doc measures the fixes the Jev literature documents against a gold set Scott labels (Phase 0), then gives tags first-class descriptions in `canonical-tags.yml`, sends them to Jev, and wires description drafting and new-tag backfill into the vocabulary growth loop so a newly promoted interest classifies as well as an old one.

## Problem Statement

### Background

- Tags-only (`docs/design/2026-09-19-tags-only-classification.md`) made `tags` the sole classification facet and routed ingest (borg `finalize_tags`) and `--retag` (cortex) through `distillers::tags::ClassifierDev`: multi-label over the 117-tag vocabulary, two even shards of 59/58 (100-label API cap), max-merged, kept at `>= 0.9`, capped at `max-per-note: 8`.
- Staged tag proposals (`docs/design/2026-09-21-staged-tag-proposals.md`) reopened vocabulary growth: raw fabric candidates on 3+ notes land in `tag-proposals.yml`, `sb cortex tag-promote` adds a slug to `config/canonical-tags.yml` through a dry-run diff. That doc listed "any change to jev / classifier.dev" and "changing how ingest assigns tags" as Non-Goals. This doc owns both.
- 89e5ad2 (v0.15.4, 2026-09-25) fixed two request bugs: `multi_label` -> `multi` (`request_body`, `distillers/src/tags.rs:571`) and `ensure_jev` (`:584`) refusing any answer whose `model` is not `jev*`, since classifier.dev answers from `ling-3.0-flash` | `mercury-2.5` when Jev fails.
- How classifier.dev builds a multi-label call (`mrmps/classifier-dev` `src/jev.ts:283-297,722-727`, read 2026-09-25): one Jev yes/no question per label, `Does the category "<label>" apply to \`i0\` under its \`rubric\`?`, over a fixed rubric `A category applies if the text meaningfully touches it, even briefly.` with the request's `instructions` **appended** after it. The label string is the only per-tag signal.
- `canonical-tags.yml` today: `tags: {group: [slug, ...]}`, 117 slugs, zero comments, `deny_unknown_fields` (`vault/src/canonical.rs:9-27`).

### Problem

- **Tags reach Jev with no definition.** TypeSafe's jaggedness notes say Jev reads literally and needs "the exact condition, criteria for each available option" (docs.typesafe.ai/model-jaggedness/jev-1.13); classifier.dev's own docs say descriptive labels classify better. Measured elsewhere: criteria text moved a yes/no from 85% to 94% (`youtube.com/watch?v=ScvXFi4MUSc` 02:54); decomposing one broad question into 8 concrete indicators moved accuracy 60% -> 94% (`youtube.com/watch?v=FVzI3QAxwNs` 12:55); OpenRouter's tagging cookbook shows a broad overlapping tag stuck at 0.33 precision at every cutoff until its wording changed.
- **The default rubric is generous.** "even briefly" fires a tag on a passing mention. A community probe scored `work` 0.76-0.80 on a pure homelab note.
- **Borderline scores flip.** Repeat calls drift about 0.005 on average (jev-fanout-bench, 2,976 requests); scores arrive rounded to 2 decimals on the Vercel path; a tag at 0.89-0.90 crosses `>= 0.9` on some calls (probe: `homelab` 0.90, 0.89, 0.89, 0.89).
- **New tags are born bare.** `promote_tags` (`cortex/src/proposals.rs:452`) inserts `    - <slug>` and nothing else, so every interest Scott grows into classifies worse than the vocabulary it joins.
- **Promotion tags nothing that already exists.** After `tag-promote --apply`, the notes whose raw candidates produced the proposal (`ci-cd` on 46 traces) never get the tag; only future ingests can.
- **The 900-char data boundary does not hold on the summary path.** `CLASSIFIER_TEXT_CHARS` (`distillers/src/tags.rs:85`) documents a data boundary, but `TagSources::from_summary` (`borg/src/pipeline/tags.rs:70`) and `cortex::classify::classifier_text` (`cortex/src/classify.rs:217`) send the summary unclipped. Measured on oracle.db 2026-09-25: 3,832 notes with a summary, mean 630 chars, 497 over 900, 19 over 4,000, 1 over 32,000 (`notes/claude-usage-report-2026-02-25-to-2026-03-27.md`, 129,928 chars). Over 32,000 chars classifier.dev routes to paid long context, which the keyless tier refuses, so that note falls back every time.
- **Batch calls break the free spend cap.** classifier.dev caps free spend at $0.01 per request and $0.50 per IP per UTC day (`llms.txt`). Measured 2026-09-25, keyless, 59 bare labels, title + summary: 53 notes cost `estimated_usd` 0.0042 (100,418 input tokens, about 1,895 per note per shard); 159 notes returned `HTTP 402 request_spending_limit` ("Split the batch, shorten input text, labels or instructions"). So a full `MAX_TEXTS_PER_CALL = 200` chunk (`distillers/src/tags.rs:33`) cannot succeed (a smaller tail chunk can), and a vault-wide pass (3,836 notes x 2 shards, about $0.61 per pass; about $1.82 once P6's `repeats: 3` posts each shard three times) exceeds a free day. No production path batches today (cortex classifies one note per call, `classify_batch` has no production caller), which is why nothing has failed. Descriptions add label text to every one of the 59 questions per note, so they raise the per-note cost.
- **Every number we have measured a non-production input against non-human labels.** The 2026-09-20 trial (`~/Claude/tags-only-trial/trial.py`) sent title + first 1,500 body chars, production sends the summary; it ran on a Pro key, production is keyless; and it scored against existing tags, whose provenance is `classified_by`: '' 1,505, deterministic 1,213, llm 1,041, classifier-dev 65 (oracle.db). So 0.340 / 0.447 P/R, 12/53 unstable, and 0/17 blind-tag recovery describe neither production nor truth. First sign: on gold-set note 1, `work` scored 0.90 on its summary.

### Goals

- **G1** Measure before building: every change below ships only if Phase 0 shows it wins on a gold set. (Scott, 2026-09-25: "if you dont understand jev|classifier.dev any better than me, maybe we should scour blogs and youtubes and figure out how to take advanage"; "targetted fix or design doc?" -> design doc with the trial as Phase 0.)
- **G2** A gold set Scott labels himself, used as the scoring target instead of the old tags. (Scott, 2026-09-25: "A hit me", on the gold-set vs proxy decision.) Provenance as built: Claude labeled `gold.md` on 2026-09-26 and Scott reviewed and accepted it ("ok looks good. lets go with it"), so the gold set is Claude-labeled and Scott-reviewed, not hand-ticked by Scott.
- **G3** Tags carry descriptions in the vocabulary, drafted by an LLM from the notes that carry the tag, approved by Scott. (Scott, 2026-09-25: "so we need to make a mappying (yaml?) of tagname: description for each of them yes? we can use claude opus for that"; "we should also scour our current obsidian note corpus for what we have".)
- **G4** The classifier sends those descriptions to Jev, plus whatever Phase 0 selects among rubric instructions and a repeat-call mean. (Scott, 2026-09-25: "are we taking full advantage of this different way of using ai (jev) ... if not, what could we do to improve it?")
- **G5** The growth loop keeps new tags first-class: promotion drafts a description from the proposal's sources, and a promoted tag can be applied to existing notes. (Scott, 2026-09-25: "part of what we were doing was building a way for our system to organically/dynamically add new tags as I morph and evolve my interests".)
- **G6** The classifier input respects one stated size bound on every path. (Author, from the measurement above: a documented data boundary the code does not enforce is a truth-in-naming defect, and the 129,928-char note is a standing fallback.)

### Non-Goals

- **Replacing Jev with a local model.** Phase 0 measures a per-tag logistic regression on the existing bge-small embeddings as a comparison arm only. Building it is **parked**; revisit condition: it beats the best Jev arm on the gold set on both blind-tag recall and instability. It would need a new ML dependency and a retraining story, neither of which Scott asked for.
- **Calling Jev's native `/v1/systemone` API** (per-tag `criteria` with examples). Parked; revisit condition: Phase 0 shows descriptions in the label string leave the blind tags under 50% recall and the per-question quota (117 per note) fits the keyless budget.
- **Changing the 0.9 cut's shared role.** Selection and promotion keep one threshold (tags-only r3 F3/F4).
- **Per-tag thresholds.** Parked; P0 reports A5 for information. Revisit condition: a gold set with at least 10 positives per tag, disjoint from the notes an arm is chosen on. 53 notes cannot fit and validate a threshold per tag without leakage (panel r1).
- **Decomposed indicator labels for broad tags.** Parked; P0 measures A4. Revisit condition: A4 beats A3 on protect-list recall per the P0 rule, and Scott asks for it. It adds a list-valued schema key nobody requested.
- **A grey band.** Still rejected (tags-only). The 0.70-0.89 near-misses in the gold checklist are a labeling aid, not a product surface.
- **Auto-promotion or auto-approved descriptions.** Both stay human-gated.
- **The `smart` tier.** It does nothing in multi-label mode (classifier.dev `src/index.ts:1043`).
- **Re-labeling the whole vault.** The gold sets are 53 notes (selection) and 30 (confirmation, P5).

## Proposed Solution

### Overview

1. **Phase 0 measures** (zero repo code): production-input baseline, then each candidate change as an arm, scored on Scott's gold set.
2. **Classifier infrastructure that holds whatever P0 says** (P1-P2, not gated): one clip on the exact outbound text, every slug's score exposed per note, batches sized by a fitted token model with per-note failure, and a spend ledger that lets a bulk job run across days and resume.
3. **Descriptions, if P0 says they win** (P3-P5): schema, drafting and backfill, then send them, after a confirmation run on a second gold set with the exact descriptions that ship.
4. **Repeat mean, if P0 says it wins** (P6).
5. **Growth loop** (P7-P8): `tag-promote` drafts the new tag's description; `sb cortex tag-apply` adds a deployed tag to existing notes, screened when P0 shows the screen's recall, resumable across days either way.

### Architecture

```
config/canonical-tags.yml
  tags:
    homelab:
      - homelab: "self-hosted servers and home network: NAS, Proxmox, k3s at home; not work infra"
      - home-automation
        |
vault::canonical  CanonicalSet { all, no_segment, no_classifier, max_per_note, descriptions }      <- P3
        |
distillers::tags::ClassifierDev
  wire text   = clip(title + "\n\n" + summary | body head)        <- P1, the assembled string
  labels      = slug-sorted vocabulary -> "slug: description" | "slug"   <- P5
  chunks      = fitted token model; 402 -> split to 1 note         <- P1
  post(multi, max_labels, instructions?) x repeats                 <- P5, P6
  score_batch -> Vec<Result<{slug: score}>>  (complete or error, per note)   <- P1
  finish: select >= threshold, author tags, cap                    <- unchanged rule
        |
borg finalize_tags (ingest)        cortex classify / --retag

spend ledger  ~/.local/share/sb/classifier-spend.jsonl  (utc day, estimated_usd per bulk call)   <- P2
  bulk paths spend min(--max-usd, day allowance - ingest reserve), then stop and say so

growth loop
  tag-proposals.yml (sources: <= 5)
     -> sb cortex tag-promote <tag> --group G --canonical config/canonical-tags.yml   (drafts, caches)   <- P7
     -> ... --apply   (writes the cached draft) -> commit + otto deploy
     -> sb cortex tag-apply <tag>            (scores candidates, caches scores in a cursor)          <- P8
     -> sb cortex tag-apply <tag> --apply    (writes from the cursor; resumes next UTC day if short)
  backfill: sb cortex tag-describe <tag>... then --apply                                            <- P4
```

### Data Model

`canonical-tags.yml`, one entry per list item, both forms legal in one file:

```yaml
tags:
  homelab:
    - homelab: "self-hosted servers and home network: NAS, Proxmox, k3s at home; not work infrastructure"
    - home-automation
```

- Entry = bare string (slug) or a one-key map (slug -> description string).
- Description optional until backfill completes; `sb doctor` reports the count of undescribed tags. An undescribed tag still classifies (as a bare slug).
- Descriptions live beside the slug they describe, so no second structure can drift (no separate `descriptions:` map).
- Description format (from the Jev guides: contrastive, concrete): what the tag covers, in words that appear in notes; then `; not <the neighbor it collides with>` when a collision exists. Max 200 chars (the documented long-context label cap, applied everywhere so one rule holds). No `"`: classifier.dev wraps the label in double quotes inside the Jev question (`src/jev.ts:290`). One-key, 200-char, and no-`"` are enforced inside `TagEntry`'s `Deserialize` impl (`#[serde(try_from = ...)]`), because `promote_tags` parses the file with raw `serde_yaml::from_str` twice (`cortex/src/proposals.rs:465-468,530-534`) and would bypass a `load()` wrapper.
- The entry value is a string, full stop. Per-tag thresholds and indicator lists are parked (Non-Goals); whichever doc un-parks one owns that schema change.

Spend ledger, `~/.local/share/sb/classifier-spend.jsonl`, one line per bulk call: `{"day": "2026-09-26", "usd": 0.0042, "tokens": 100418, "path": "tag-apply", "tag": "ci-cd"}`. `usd` is the response's `pricing.estimated_usd`, not our estimate.

`tag-apply` cursor, `~/.local/share/sb/cortex/tag-apply/<tag>.json`: `{tag, scoring_config_hash, candidate_mode, candidates: [path], scores: {path: {score, wire_text_hash}}, failed: {path: error}}`.
- `scoring_config_hash` covers everything that changes a score: endpoint, the labels as sent (vocabulary plus descriptions), `instructions`, `repeats`, `CLASSIFIER_TEXT_CHARS`. A different hash discards every score.
- `candidate_mode` is `screen:N` or `all`. Widening (`screen:300` -> `screen:600` -> `all`) extends `candidates` and keeps still-valid scores; narrowing keeps the wider list.
- A score is reused only while its `wire_text_hash` matches the note's current assembled wire text, so a note edited between days is rescored.

### API Design

- `distillers::tags`:
  - `clip_classifier_text(title, text) -> String`: assembles `title + "\n\n" + text`, then clips the assembled string at `CLASSIFIER_TEXT_CHARS`. The only builder of wire text; `classify_texts` (`distillers/src/tags.rs:675`) calls it instead of formatting its own.
  - `ClassifierDev::score_batch(&[TagInput]) -> ScoredBatch`, where `ScoredBatch { notes: Vec<Result<HashMap<String /*slug*/, f32>>>, calls: Vec<CallUsage { input_tokens, estimated_usd, ok }> }`: one result per input, one usage row per HTTP call. A note is `Ok` only when every shard (and every repeat) returned a score for every label sent; a missing label, an unknown returned key, or a failed call for that note is that note's `Err`. `finish` is rebuilt on it, and `classify_batch` maps each `Err` to that note's fallback (today one shard error fails every input, `tags.rs:747-756`).
  - `labels_for_shard(&[String]) -> Vec<(String /*wire label*/, String /*slug*/)>` (P5).
  - `ClassifyResponse` reads `usage` and `pricing` into `ScoredBatch.calls`, so bulk callers write the ledger for every call that succeeded, including when a later call fails.
- `TagsClassifierConfig` gains `instructions: String` (P5), `repeats: u8` (P6), `daily-usd-budget: f32` and `ingest-reserve-usd: f32` (P2). Code defaults are the P0 values (`borg.yml` / `cortex.yml` are written only when missing, even under `sb bootstrap --force`, `sb/src/cli/bootstrap.rs:254-272`). `sb doctor` prints the effective classifier settings from **both** `borg.yml` and `cortex.yml` (today it loads only borg's, `sb/src/cli/checks.rs:446`), plus today's ledger total.
- `sb cortex tag-describe <TAG>... --canonical <repo file> [--samples N] [--exclude-from FILE] [--set TAG=TEXT] [--allow-protected] [--apply]` (P4).
- `sb cortex tag-promote` (existing, P7): drafts and caches; `--apply` writes the cached draft; `--no-describe` keeps today's bare insert.
- `sb cortex tag-apply <TAG> [--candidates N | --all] [--max-usd X] [--apply]` (P8).

### Implementation Plan

#### Phase 0: Measure on production input against a gold set
**Model:** opus
- Zero repo code. Harness in `~/Claude/tags-only-trial/` (scratch, never `docs/`), keyless, `"multi": true`, every result's `model` checked for `jev`, wire text built exactly as P1's `clip_classifier_text` will build it (title + `\n\n` + summary, clip applied to the assembled string).
- Gold set: Scott ticks `~/Claude/tags-only-trial/gold.md` (53 notes; current tags + classifier `>= 0.9` pre-checked, 0.70-0.89 unchecked, `add:` line). Parsed to `gold.json`.
- Arms, 3 byte-identical repeats unless noted. A0 and A1 run first; A2-A4 run on whichever of their inputs wins, so what P5 would ship is what was measured.
  - A0 baseline: bare slugs, title + summary unclipped (production today).
  - A1 clipped: A0 with the assembled text clipped at 900 chars. Whichever of A0 | A1 wins is run 6 times (two triples), so P6 has a like-for-like measurement even if descriptions lose. A1b (2,000 chars) runs only if A1 loses to A0 by more than 0.02 F1 (see P1).
  - A2 described: `slug: description` labels. Trial descriptions drafted by Opus from 5-10 sample notes per tag, samples excluding the 53 gold notes.
  - A3 described + instructions: A2 plus `instructions: "Apply a category only when the text is substantially about it; a passing mention does not count."` Run 6 times (two independent triples).
  - A4 decomposed (measurement only; build parked): A3, each protect-list tag sent as 2-4 concrete indicator labels, tag = max of its indicators.
  - A5 per-tag thresholds (information only; build parked): leave-one-out on the selected arm's scores. Gates nothing: fitting on the notes the arm was chosen on overstates it.
  - A6 local (information only; build parked): per-tag logistic regression on bge-small summary vectors (oracle.db `note_embeddings`, read-only), trained on the vault minus the 53 using the old tags (so it inherits their provenance), scored on the 53.
  - Shard check: A2 labels interleaved, one call, against a byte-identical repeat, as tags-only P0b did.
- Metrics per arm, against `gold.json`: micro precision | recall | F1; recall on gold positives of each protect-list tag; tag-set instability = pairwise disagreement rate of the `>= 0.9` sets across repeats; tokens and `estimated_usd` per call.
- **Repeat mean, like for like** (from A3's 6 calls): single-call instability = mean disagreement over the pairs (1,2), (3,4), (5,6); mean instability = disagreement between mean(1-3) and mean(4-6). Also F1 of the averaged predictions against F1 of a single call.
- **Cost model:** fit `tokens per note per shard = a + text_tokens + questions x (b + label_tokens)` (rubric and instructions sit in per-note state once, `src/jev.ts:722-727`; each question carries the template plus the label, `:283-297`) on every recorded call; report `a`, `b`, and the largest chunk that passed per arm. Calls start at 53 texts and split to 1 on a 402.
- **Screen recall** (for P8): for N in 100 | 300 | 600 | 1,000, the fraction of positives inside the top N of a summary-kind vector search on `"<tag>: <description>"` unioned with a BM25 phrase match on the slug over `title`, `body`, `summary` only (`notes_fts` also indexes `tags` and `claims`, `vault/src/search/schema.rs:496`, which would find a note through the very tag being measured; hyphenated slugs are quoted, since `ci-cd` unquoted parses as a column filter). Positives: (a) gold positives, for tags with at least 3; (b) every vault note carrying the tag today, a larger, biased proxy. 5 notes have no embedding; they count as misses.
- **"Beats baseline"**, where baseline is the better of A0 and A1: micro-F1 up at least 0.05 absolute with a paired bootstrap over notes (1,000 resamples) whose 95% lower bound is above 0; instability no worse; no protect-list tag with at least 3 gold positives losing recall. Per-tag results under 3 gold positives are listed with a Clopper-Pearson interval and decide nothing (3 of 3 only proves recall is somewhere in 29%-100%). This is a pilot screen; P5 carries the confirmation.
- Results table pasted verbatim into Phase 0 Results.
- **Success criteria:**
  - every arm row is filled, every call's `model` starts with `jev`, and summed `estimated_usd` stays under $0.50 per UTC day (the run spans days if it must)
  - the doc records the winner under "beats baseline", the cost-model constants, the screen recall table, and go | no-go for each gated phase: P3, P4, P5 on A2 or A3 winning; P6 on the repeat-mean comparison; P8's screen on screen recall

#### Phase 1: Classifier infrastructure (not gated)
**Model:** opus
- `clip_classifier_text` on the assembled string; `TagSources::from_summary` (`borg/src/pipeline/tags.rs:70`), `from_body`, and `cortex::classify::classifier_text` (`cortex/src/classify.rs:217`) stop clipping on their own and pass title and text through it. `CLASSIFIER_TEXT_CHARS` is always finite: 900 ships if A1's F1 is within 0.02 of A0's; if A1 loses by more, P0 runs A1b at 2,000 chars and the smaller of 900 | 2,000 within 0.02 of A0 ships, else 2,000 (the bound is a data boundary per G6, so "unclipped" is never an outcome).
- `score_batch` with per-note results as in API Design; `finish` and `classify_batch` rebuilt on it.
- Batch sizing from P0's fitted token model against a per-request token budget (a code constant from P0's largest passing call, not a knob); a 402 `request_spending_limit` splits the chunk in half and retries down to one note; a single note that still 402s is that note's `Err`.
- `ClassifyResponse` reads `usage` and `pricing`; each call logs `input_tokens` and `estimated_usd` at debug.
- `sb doctor` prints effective classifier settings for borg and cortex.
- **Success criteria:**
  - a test asserts the string handed to `post` for a 129,928-char summary and a 200-char title is at most `CLASSIFIER_TEXT_CHARS` chars
  - a test with a fake responder that 402s above 10 notes asserts a 40-note batch completes in chunks of at most 10, and one that 402s on a single named note returns `Err` for that note and `Ok` for the other 39
  - a test asserts a response missing one sent label yields `Err` for that note only, and a note whose first shard succeeded but second shard (or a later repeat) failed is `Err`, not `Ok` with partial scores

#### Phase 2: Spend ledger and resumable bulk runs (not gated)
**Model:** opus
- Ledger as in Data Model, written by every bulk path (more than one note) from the response's `pricing.estimated_usd`.
- Day allowance = `daily-usd-budget` (default 0.50, the keyless free day) - ledger total for the UTC day - `ingest-reserve-usd` (default 0.10, headroom for borg's live ingest, which does not write the ledger). A run spends at most `min(--max-usd, allowance)`.
- `sb cortex classify --retag` over more than one note: prints the estimate; above the allowance it refuses and prints how many notes fit.
- The cursor mechanism `tag-apply` (P8) uses lives here: score until the allowance is spent, persist scores, exit 0 with `N remaining, resumes after 00:00 UTC`.
- classifier.dev's own 402/429 on the day cap is the backstop: the run stops, persists, and reports it; it never retries into the cap.
- **Success criteria:**
  - a test asserts a run with $0.05 of allowance and a $0.02-per-chunk fake scores two chunks, persists them, and exits 0 reporting the remainder
  - a test asserts a second run the same UTC day with the ledger at the budget makes zero HTTP calls, and a next-day run scores only the notes the cursor lacks
  - a test asserts a run whose third chunk fails still leaves ledger rows for the first two

#### Phase 3: Descriptions in the vocabulary schema
**Model:** opus
- Gate: A2 or A3 beat baseline in P0.
- `TagEntry` untagged enum, validation in `Deserialize`; `CanonicalSet.descriptions`; every reader keeps compiling on `all_tags()` / `canonical_set()`.
- `insert_tags_into_group` (`cortex/src/proposals.rs:598`) writes `    - slug` or `    - slug: "desc"`; new `describe_entry_in_group` rewrites one entry line in place, same bail-on-unrecognized-layout rule.
- `tag-values.md` (`cortex/src/schema_docs.rs:425-463`) becomes `| Tag | Description |`; `sb doctor` reports `N of M tags have no description`.
- Tests: bare-only file parses (today's shipped file); mixed file parses; two-key entry errors; round-trip insert and in-place describe leave every other line byte-identical; shipped-file tests updated (`cortex/src/proposals/tests.rs:410`, `vault/src/canonical/tests.rs:350`).
- **Success criteria:**
  - `cargo test -p vault canonical` passes, including `mixed_entries_parse`, `two_key_entry_is_an_error`, and `raw_serde_parse_enforces_entry_rules` (a quote-bearing description fails through plain `serde_yaml::from_str`)
  - after `sb cortex schema --render`, the generated `tag-values.md` contains `| Tag | Description |` (the command's stdout prints status and path, `sb/src/cli/cortex.rs:1034-1042`)

#### Phase 4: `sb cortex tag-describe` (drafting and backfill)
**Model:** opus
- Gate: as P3.
- Fabric pattern `describe-tag` in `borg/patterns/` (shipped through `PATTERNS`, `sb/src/cli/bootstrap.rs:72`): input = tag, group, group siblings, 5-10 sample notes (title + summary through `clip_classifier_text`); output JSON `{description}`, under 200 chars, contrastive against the sibling it most overlaps.
- Samples: notes carrying the tag, newest first, minus any path listed in `--exclude-from <file>` (one vault path per line). The backfill run passes both gold sets' paths, and `gold-2.md` is generated and frozen before the first P4 run, so no confirmation note drafts the description it confirms.
- Dry-run prints `+ tag: "desc"` and caches each draft at `~/.cache/sb/tag-describe/<tag>.json` with the sample paths it was drafted from; the P5 confirmation harness asserts those paths are disjoint from `gold.json` and `gold-2.json`; `--apply` writes the cached draft through `describe_entry_in_group` (never a fresh LLM call, so what Scott approved is what lands); `--apply` with no cached draft refuses. `--set TAG=TEXT` writes Scott's own wording with no LLM call.
- Protect-list tags refuse without `--allow-protected` (author decision 2026-09-25, Scott may override: the classifier is weakest on those five, so their wording should be his).
- **Success criteria:**
  - `sb cortex tag-describe rust --canonical config/canonical-tags.yml` exits 0, prints one `+ rust:` line, and leaves the file byte-identical
  - `sb cortex tag-describe work --canonical config/canonical-tags.yml` exits non-zero naming the protect list

#### Phase 5: Send described labels, after confirmation
**Model:** sonnet
- Gate: A2 or A3 beat baseline in P0, **and** a confirmation run passes: Scott labels a second gold set (`gold-2.md`, 30 notes outside the 53, oversampling `writing` and `diy`), and the frozen shipping configuration (the descriptions approved through P4, the instructions string if A3 won, P1's clip) beats baseline on it under the P0 rule. Selection and confirmation then use different labels.
- `labels_for_shard`: sort by **slug** (shards stay the pinned 59/58 partition; sorting described strings would move `ai:` after `ai-safety`), emit `slug: description` or `slug`, map scores back through the wire-label -> slug table; `score_batch`'s missing/unknown-key rule covers the mapping.
- `instructions` code default = the A3 string if A3 won, else empty.
- New fixture recorded from a described-label call; the bare fixture kept for undescribed tags.
- **Success criteria:**
  - a test asserts the request body for a described tag carries the label `homelab: <description>` and the returned score lands on slug `homelab`
  - the confirmation table (baseline vs shipping configuration on `gold-2.json`) is pasted in Phase 0 Results
- The confirmation gate is operator-held, not code-held: any described entry in the deployed `canonical-tags.yml` is sent, and `tag-promote --apply` writes a drafted description by default, so until confirmation passes use `tag-promote --no-describe` and do not `tag-describe --apply`.

#### Phase 6: Repeat mean
**Model:** sonnet
- Gate: on the configuration that ships (A3's six calls if P5 goes; the baseline winner's six calls if it does not), mean instability below single-call instability and averaged F1 not below single-call F1.
- `repeats: N` (code default from P0), N calls per shard, scores averaged per slug in `score_batch` before selection; P2's estimates multiply by N.
- **Success criteria:**
  - a test asserts `repeats: 3` issues 3 calls per shard and selects on the mean

#### Phase 7: `tag-promote` drafts the promoted tag's description
**Model:** sonnet
- Gate: as P3.
- `promote_tags` calls the P4 drafter over the proposal's `sources`: vault paths read as notes; `trace:<id>` sources read the staged `distilled.yml` summary, which needs a new `summary` field on `StagedDistilled` (today it reads only `tags` and `meta`, `cortex/src/proposals.rs:73-78`).
- Dry-run shows and caches the draft; `--apply` writes the cached draft; `--no-describe` keeps the bare insert.
- **Success criteria:**
  - a test with a fake fabric runner asserts `tag-promote --apply` writes `    - <tag>: "<drafted>"` into the named group
  - a test asserts `--no-describe` writes `    - <tag>`

#### Phase 8: `sb cortex tag-apply` (deployed tag -> existing notes)
**Model:** opus
- Not gated as a whole; its screen is. Refuses a tag missing from the deployed vocabulary, naming `otto deploy`.
- Candidates: with the screen (go if P0's screen recall reaches 90% on gold positives at some N, proxy reported beside it), top `--candidates N` (default: that N) by summary-embedding cosine to `"<tag>: <description>"` (`vault::embedding::embed_query`, `vault/src/embedding.rs:215`; ranked on the summary kind only, since `search_vector` today takes each note's best match across every kind, `vault/src/search/vector.rs:245`, and 17,895 transcript-chunk rows over 897 notes would get extra chances), unioned with a BM25 phrase match on the slug over `title`, `body`, `summary`. Without the screen (no-go, or `--all`): every note.
- Scoring through `score_batch` with the production call (full vocabulary, both shards), reading the target slug's uncapped score, so a ninth-ranked tag is visible and the co-label context matches ingest. Scores persist in the P2 cursor; a run stops at the day allowance and resumes on the next run. The whole vault at bare cost is about $1.82 under P6's `repeats: 3`, five days at the $0.40 a bulk run may spend (`daily-usd-budget` $0.50 - `ingest-reserve-usd` $0.10), four at the bare $0.50 cap. Arithmetic from Phase 0 Results' per-call figure (bare $0.0042 per 53-note call per shard): 3,836 / 53 = 72.4 calls per shard, x 2 shards = 144.8 calls, x $0.0042 = $0.61 per pass, x 3 repeats = $1.82; $1.82 / $0.40 = 4.6, so five days. Described labels ($0.0063 per call) make it $0.91 per pass, $2.74 at three repeats, seven days.
- Dry-run scores and caches; `--apply` writes from the cursor without re-scoring, and scores any remainder the day allows first.
- Write: the tag is added where score `>= threshold` and the note has fewer than `max-per-note` tags; never removes a tag. The union and cap are recomputed against the note's current frontmatter at write time (today `write_fields` rereads the file but writes precomputed fields, `cortex/src/classify.rs:634-654`), so a concurrent edit is merged, not overwritten. Notes at the cap and notes whose scoring failed are listed, not changed.
- **Success criteria:**
  - `sb cortex tag-apply <tag>` on a tag missing from the deployed vocabulary exits non-zero naming `otto deploy`
  - a test asserts a note at the cap is reported as skipped and its file is byte-identical after `--apply`
  - a test asserts a target tag scoring 0.91 but ranked ninth is added to a note with room under the cap
  - a test asserts `--all` after a `screen:300` run scores only the notes outside the 300, and a changed `instructions` value discards the cursor's scores

### Phase 0 Results

Run 2026-09-26 (UTC 21:48 onward), keyless, `"multi": true`, every result answered by `jev-1.13.0`, 0 escalations, 0 HTTP 402s. Summed logged `estimated_usd` $0.2535 over 47 calls, plus one call whose response was truncated in transit and never logged (about $0.0065). Harness and raw output: `~/Claude/tags-only-trial/p0/` (`arms.py`, `score.py`, `analyze.py`, `calls.jsonl`, `runs/`, `analysis.txt`).

Gold set: `gold.json`, parsed from `gold.md`: 53 notes, 252 positives (4.75 per note). Claude labeled it and Scott reviewed it (see G2). Two biases to read the numbers through: `gold.md` pre-checked every tag the keyless classifier scored `>= 0.9` on the same summaries A0 sends, which flatters A0's precision; and the A2-A4 descriptions were drafted by the same model that labeled the gold set, after labeling it (samples excluded the 53, but the drafter had seen their labels), which flatters A2-A4. P5's confirmation run on `gold-2` is the check on the second.

Wire text: title + `\n\n` + summary for 31 notes, body head (900 chars) for 22; assembled length min 72, median 746, max 969; 18 notes over 900.

| arm | calls | P | R | F1 | instability (note) | instability (label) | tags/note | protect recall, mean hits / gold |
|---|---|---|---|---|---|---|---|---|
| A0 bare, unclipped | 6 | 0.842 | 0.661 | 0.741 | 0.189 | 0.067 | 3.74 | work 0.7/7, life 0.0/4, homelab 1.7/5, diy 0.0/3, writing 0.0/2 |
| A1 bare, clip 900 | 3 | 0.830 | 0.660 | 0.735 | 0.208 | 0.077 | 3.78 | work 0.7/7, life 0.0/4, homelab 2.0/5, diy 0.0/3, writing 0.0/2 |
| A2 described | 3 | 0.895 | 0.702 | 0.787 | 0.189 | 0.068 | 3.73 | work 3.7/7, life 2.0/4, homelab 2.0/5, diy 1.0/3, writing 1.0/2 |
| A3 described + instructions | 6 | 0.952 | 0.575 | 0.717 | 0.111 | 0.042 | 2.87 | work 4.0/7, life 2.0/4, homelab 1.7/5, diy 1.0/3, writing 1.0/2 |
| A4 decomposed (on A3) | 3 | 0.947 | 0.590 | 0.727 | 0.138 | 0.054 | 2.96 | work 6.0/7, life 2.0/4, homelab 2.0/5, diy 1.0/3, writing 2.0/2 |
| A5 per-tag thresholds, LOO on A2 mean | - | 0.804 | 0.813 | 0.809 | - | - | - | information only |
| A6 local LR, p >= 0.3 | - | 0.724 | 0.333 | 0.457 | - | - | - | work 1/7, life 1/4, homelab 1/5, diy 0/3, writing 0/2 |
| SH shard check (A2 labels interleaved) | 2 | 0.899 | 0.702 | 0.788 | 0.189 | 0.050 | 3.72 | work 4.0/7, life 2.0/4, homelab 2.0/5, diy 1.0/3, writing 1.0/2 |

Instability (note) is the share of (note, repeat pair) cells whose `>= 0.9` set differs; (label) is symmetric difference over union. A1b not run: A1 lost to A0 by 0.006 F1, inside 0.02.

**Beats baseline** (baseline A0, the better of A0 | A1; paired bootstrap over notes, 1,000 resamples, mean-over-repeats F1):

```
A1: dF1 -0.005 95% CI [-0.016, +0.004]; instab 0.208 vs 0.189 -> does not beat
A2: dF1 +0.046 95% CI [-0.006, +0.098]; instab 0.189 vs 0.189; no protect tag (>=3 pos) losing recall -> does not beat
A3: dF1 -0.024 95% CI [-0.103, +0.050]; instab 0.111 vs 0.189 -> does not beat
A4: dF1 -0.014 95% CI [-0.080, +0.049]; instab 0.138 vs 0.189 -> does not beat
```

Winner under the rule: none. A2 is the closest: +0.046 F1 (bar 0.05) with a lower bound of -0.006 (bar > 0), instability unchanged, and every protect-list tag's recall up or flat.

Protect-list recall, Clopper-Pearson 95% (hits rounded over repeats); under 3 positives decides nothing:

```
A0: work 1/7 [0.00-0.58]; life 0/4 [0.00-0.60]; homelab 2/5 [0.05-0.85]; diy 0/3 [0.00-0.71]; writing 0/2 [0.00-0.84]
A2: work 4/7 [0.18-0.90]; life 2/4 [0.07-0.93]; homelab 2/5 [0.05-0.85]; diy 1/3 [0.01-0.91]; writing 1/2 [0.01-0.99]
A4: work 6/7 [0.42-1.00]; life 2/4 [0.07-0.93]; homelab 2/5 [0.05-0.85]; diy 1/3 [0.01-0.91]; writing 2/2 [0.16-1.00]
```

Shard check (same A2 labels, 6,201 score cells): repartition A2-1 vs SH-1 p50 0.000, p95 0.020, max 0.130; identical repeat A2-1 vs A2-2 p50 0.000, p95 0.020, max 0.150. Repartitioning moves scores no more than a repeat does, so the slug-sorted 59/58 shard rule holds with descriptions.

Repeat mean (six calls, pairs (1,2), (3,4), (5,6); mean(1-3) vs mean(4-6)):

```
A3: single-call instability 0.119, mean-of-3 instability 0.038; F1 single 0.717, F1 mean-of-3 0.713
A0: single-call instability 0.195, mean-of-3 instability 0.170; F1 single 0.741, F1 mean-of-3 0.743
```

Cost model, least squares on 46 successful calls (`tokens per call = n*a + t*text_chars + n*Q*b + l*n*label_chars`): a = 26.9 tokens per note, t = 0.43 tokens per text char, b = 24.7 tokens per question, l = 0.21 tokens per label char per note; max relative error 1.7%. Every call had 53 notes and 58-64 questions, so a and b are not separately identified (a per-question text term fits equally well, a = 336, b = 19.4); the per-call numbers are the dependable ones. Largest passing chunk, every arm: 53 notes. Mean per call: bare 99,952 tokens / $0.0042; described 149,070 / $0.0063; described + instructions 150,076 / $0.0063; decomposed 157,708 / $0.0066. Descriptions raise per-note cost 1.5x.

Screen recall (top N by summary cosine to `"<tag>: <description>"`, bge-small, unioned with every BM25 phrase hit on the slug over `title`, `body`, `summary`), 30 tags with at least 3 gold positives:

| N | 100 | 300 | 600 | 1,000 |
|---|---|---|---|---|
| gold positives | 0.476 | 0.529 | 0.594 | 0.690 |
| proxy (every note carrying the tag today) | 0.566 | 0.616 | 0.675 | 0.735 |

Worst tags at N = 1,000: `tech` 6/26, `software-engineering` 5/18, `open-source` 3/7, `work` 4/7, `homelab` 2/5. Per-tag table in `analysis.txt`.

Go | no-go:
- P1: go (not gated). `CLASSIFIER_TEXT_CHARS` stays 900 (A1 within 0.02 of A0). Per-request budget from the largest passing call: 157,708 tokens / $0.0066 at 53 notes.
- P3, P4, P5: **no-go under the rule** (neither A2 nor A3 beats baseline). A2 misses by 0.004 F1 and a CI touching zero, while lifting `work` recall from 1/7 to 4/7. Whether that near-miss justifies P3-P5 is Scott's call, not the rule's.
- P6: go. P5 is no-go, so the gate reads the baseline winner's six calls: mean-of-3 instability 0.170 below single-call 0.195, and averaged F1 0.743 not below single-call 0.741. The margin is small; A3's six calls show the larger effect (0.119 -> 0.038).
- P8 screen: no-go. Gold-positive recall peaks at 0.690 (N = 1,000), under the 90% bar, so `tag-apply` scores every note (`--all` behavior) and the resumable ledger carries it across days.
- A6 local model: revisit condition not met (F1 0.457 vs 0.787 for A2; protect recall 3/21 vs 10/21). Stays parked.

## Acceptance Criteria

- [ ] Phase 0 Results holds every arm's row against `gold.json`, the cost-model constants, the screen recall table, and a go | no-go for P3, P4, P5, P6 and P8's screen
  - Observed on main (89e5ad2, 2026-09-26): cannot run yet, Phase 0 waits on the gold set.
  - Observed 2026-09-26 after P0: Phase 0 Results holds every arm row (A0-A6, SH), the cost-model constants, the screen recall table, and go | no-go for P3, P4, P5, P6 and P8's screen.
- [ ] no classifier wire text exceeds `CLASSIFIER_TEXT_CHARS`: P1's test feeding a 129,928-char summary with a 200-char title passes
  - Observed on main: cannot run yet, the test is added in P1. The defect it guards is measured in Problem.
  - Observed on b61897e: `the_string_posted_for_a_huge_summary_is_clipped` passes in `otto ci`.
- [ ] `sb doctor` output reports the effective classifier settings for both borg and cortex, and (after P3) how many tags have no description
  - Observed on main: `sb doctor | grep -ic descri` -> `0`; the classifier line reports borg only.
  - Observed on b61897e: `target/debug/sb doctor` prints `borg classifier settings: ...` and `cortex classifier settings: ...` lines and `117 of 117 tags have no description`.
- [ ] if P3-P5 went: `grep -c ': "' config/canonical-tags.yml` (counts lines, one per described entry) equals the number of descriptions Scott approved through P4, and is `>= 1`
  - Observed on main: `0` (no descriptions yet).
  - Observed on b61897e: `0`. UNVERIFIED until Scott approves backfill drafts through P4 `--apply`.
- [ ] `sb cortex tag-apply --help` exits 0
  - Observed on main: exit `2` (subcommand does not exist; P8 adds it).
  - Observed on b61897e: `target/debug/sb cortex tag-apply --help` exit `0`.

## Resolved Decisions

- 2026-09-25, Scott: gold set labeled by Scott (option A) over the old tags as a proxy.
- 2026-09-26, Scott: the gold set is Claude-labeled and Scott-reviewed ("ok looks good. lets go with it"); G2 records the provenance.
- 2026-09-26, Scott: override the Phase 0 rule and build descriptions (P3-P5) on A2's near-miss (+0.046 F1, CI [-0.006, +0.098], work recall 1/7 -> 4/7): "build it". P5's confirmation run on `gold-2` remains the gate before descriptions ship.
- 2026-09-25, author (verified): classifier.dev `instructions` appends to the "even briefly" rubric, it does not replace it (`src/jev.ts:724-725`); rubric and instructions travel once per note in state, each question carries only the template and label (`:283-297,722-727`).
- 2026-09-25, author: descriptions live inside each vocabulary entry, not in a parallel map.
- 2026-09-25, author: `tag-apply` is a separate post-deploy verb, because promotion writes the repo copy and the daemons read the deployed copy only after `otto deploy`.
- 2026-09-25, author: `tag-apply` never evicts a tag to make room at the cap.
- 2026-09-25, author: the protect list (`no-classifier-tags`) is not changed by this doc; P0 reports per-tag recall for its five tags and removing one is Scott's call.
- 2026-09-25, author, panel r1 (architect pushback, partly adopted, accepted by both seats in r2): no full BM25 + vector RRF in the screen; `vault/src/search/AGENTS.md` describes oracle's pattern and does not mandate it. A BM25 phrase hit on the slug is unioned in.
- 2026-09-25, author, panel r1 (accepted by both seats in r2): new classifier keys take their P0 values as code defaults, because existing hosts never receive new yml keys; one source of the value, overridable per host, visible through `sb doctor`.
- 2026-09-25, author, panel r1 (accepted by both seats in r2): A4 and A5 are measurements only, builds parked.
- 2026-09-26, author, panel r2: description-independent infrastructure (clip, `score_batch`, batch sizing, ledger) is split into ungated P1-P2, so a no-go on descriptions still ships the clip (G6), the spend guard, and `tag-apply`.
- 2026-09-26, author, panel r4: the clip always ships finite (900, or 2,000 if 900 costs more than 0.02 F1); P4 samples exclude both gold sets and the confirmation harness asserts it; the `tag-apply` cursor is keyed on the full scoring config, candidate mode, and each note's wire-text hash.
- 2026-09-26, author, panel r4 (staff defer, accepted): no end-to-end multi-day `tag-apply` assertion beyond the P2 and P8 unit tests; the resume mechanism is fully exercised by those tests with a fake clock and fake responder, and a live multi-day run is an operator check after P8 ships, recorded in its implementation notes.
- 2026-09-26, author, panel r2: P0 is a pilot screen; described labels ship only after a confirmation run on a second gold set Scott labels, with the exact descriptions and config that ship.
- 2026-09-27, author, implementation audit r1: P5's confirmation gate is operator-held, not code-held. Any described entry in the deployed `canonical-tags.yml` is sent, and `tag-promote --apply` writes a drafted description by default, so until confirmation passes use `tag-promote --no-describe` and do not `tag-describe --apply`.

## Alternatives Considered

### Separate `tag-descriptions.yml`
- **Pros:** zero schema change to `canonical-tags.yml`.
- **Cons:** two files describe one vocabulary; a promoted tag can land in one and not the other.
- **Why not chosen:** derived data never diverges if it has one home.

### Native `/v1/systemone` with per-tag criteria and examples
- **Pros:** the richest per-tag signal Jev accepts; no 100-label cap.
- **Cons:** quota counts every question: 117 per note, about 444k for a vault retag (about 23 keyless days).
- **Why not chosen:** parked (Non-Goals) until P0 shows the label-string route falls short.

### Local per-tag model on existing embeddings
- **Pros:** deterministic, local, trained on data already present; one presenter measured 93.2 vs Jev 80.1 on Banking77 with this shape (`FVzI3QAxwNs` 05:46).
- **Cons:** new ML dependency; thin positives for many tags (1-2 notes for `creative-writing`, `cooking`); cannot score a tag promoted yesterday; not requested.
- **Why not chosen:** measured in P0 (A6) only; parked with a revisit condition.

### Refuse whole-vault `tag-apply` instead of resuming
- **Pros:** no cursor, no ledger.
- **Cons:** a whole-vault pass costs about $1.82 at `repeats: 3` ($0.61 per pass, see P8) and the free day is $0.50, so a no-go on the screen would leave `tag-apply` unable to run at all (panel r2).
- **Why not chosen:** resuming is the smallest mechanism that makes the fallback run.

## Technical Considerations

### Dependencies
- classifier.dev keyless tier (3,000/min and 20,000/day per IP; 1,000 inputs per fast call; $0.01 per request and $0.50 per IP per day free spend; `llms.txt`).
- Fabric, for `describe-tag` (P4, P7), same runner as the entities extractor (`cortex/src/entities.rs:62-71`).

### Performance
- Ingest: 2 calls per note today, `2 x repeats` after P6; 100-800ms per call measured, shards sequential.
- `tag-apply` with a 300-note screen: about $0.05 at bare cost (measured $0.00008 per note per shard); described labels raise it by the ratio P0 measures.
- Daily ingest: about 20 notes x 2 shards, well inside `ingest-reserve-usd`.

### Security
- Classifier wire text leaves the machine: P1 makes the stated bound true for the exact string sent. Descriptions leave as label text; they are Scott's words about his interests, the same class of data as the slugs.

### Testing Strategy
- Unit tests per phase as listed; every new test broken once by hand to prove it bites.
- Live checks after each deploy: `sb doctor` classifier probe; one reingest with `sb borg log --degraded --since 10m` empty.

### Rollout Plan
- Per host the binary and the vocabulary ship as one artifact: `sb bootstrap --force` writes `canonical-tags.yml` from the binary's `include_str!` copy (`sb/src/cli/bootstrap.rs:61,287-295`). P3's parser ships in its own deploy while the shipped file is still bare; the first described entry lands in a later P4 `--apply` commit and deploy, so no running process meets a described entry before its parser exists.
- `borg.yml` and `cortex.yml` are never rewritten on existing hosts (`bootstrap.rs:254-272`); new behavior reaches them through code defaults.
- Descriptions land through P4 `--apply` on the repo copy -> commit -> `otto deploy`.
- `tag-apply` runs per promoted tag, after deploy, dry-run first.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Gold set anchors to pre-checked boxes | Med | Med | Near-misses shown unchecked; P0 reports gold-vs-current disagreement |
| 53 notes pick a winner by chance | Med | Med | Paired bootstrap lower bound; confirmation on a second gold set before P5 ships |
| Described labels change co-label context, shard invariance no longer holds | Low | Med | P0 shard check on A2 labels |
| A drafted description is wrong and skews a tag | Med | Med | Cached draft is what lands; human approval on every `--apply` |
| Rolling back to a pre-P3 binary and running `otto deploy` rewrites `canonical-tags.yml` from that binary, dropping approved descriptions and tags promoted since | Low | High | Existing behavior, now carrying Scott's prose: the repo copy is the source of truth, so a rollback is re-deployed forward from `main`, never from an old tag |
| New config keys never reach existing hosts | High (structural) | Med | Code defaults carry the P0 values; `sb doctor` prints effective settings for borg and cortex |
| `tag-apply` screen misses valid notes | Med | Med | Screen gated on P0 recall; `--all` always available and resumable |
| Live ingest and a bulk run share the IP's $0.50 day | Med | Low | `ingest-reserve-usd` headroom; classifier.dev's own day-cap 402 stops the bulk run, which persists and resumes |
| classifier.dev (a third-party wrapper) changes behavior or starts billing | Med | Med | `ensure_jev` fails closed to the deterministic fallback with a degraded receipt; the ledger records `pricing` on every bulk call |

## Open Questions

None.

## References

- `docs/design/2026-09-19-tags-only-classification.md`, `docs/design/2026-09-21-staged-tag-proposals.md`
- classifier.dev `llms.txt`, `agents.md`; `github.com/mrmps/classifier-dev` `src/jev.ts`, `src/index.ts`
- docs.typesafe.ai: model-jaggedness/jev-1.13, primitives/noul, cookbooks/consistency_noul_cookbook, cookbooks/skill_suggestion
- openrouter.ai/docs/cookbook/evaluate-and-optimize/jev-classification
- github.com/blowxian/jev-fanout-bench; github.com/KantaHayashiAI/jev-does-not-play-dice
- YouTube: ScvXFi4MUSc (Ray Amjad), FVzI3QAxwNs, SNJ3yuJ_QwY (AICodeKing), X117w2Rark8
- marquee: `~scott-idler/tagging-in-sb-two-jobs-not-one`, `~scott-idler/classify-dev-starter-pack-spec-lab-squad`
