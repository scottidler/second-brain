# Design Document: Jev Tag Classifier

**Author:** Scott Idler
**Date:** 2026-10-16
**Status:** In Review
**Review Passes Completed:** 5/5

## Summary

Replace the `classifier-dev` tag classifier with a `jev` classifier that calls TypeSafe's API directly (`POST https://api.typesafe.ai/v1/systemone`) with our own key. Then excise every use and mention of classifier.dev from the repo. classifier.dev wraps jev, but it only takes its own workspace tokens, and its keyless tier now refuses most of our calls on cost.

## Problem Statement

### Background

- Tags are the only classification facet. Borg (at ingest) and cortex (classify and `--retag`) both assign them through `distillers::tags`. The `tags.classifier` block selects the implementation (`ClassifierKind`, `distillers/src/tags.rs:164-171`).
- Both ripr configs run `classifier: classifier-dev`, `threshold: 0.9`, `fallback: deterministic`, with no `token-env`:
  - `~/.config/sb/borg.yml:222-233`
  - `~/.config/sb/cortex.yml:272-277`
- Every call is keyless. The keep secret `classify-api-key.age` is a revoked classifier.dev workspace key: on 2026-10-16 it got `401 invalid_api_key` from classifier.dev and `401 authentication_error` from TypeSafe.
- classifier.dev is a separate Cloudflare-hosted service that runs jev underneath. There is no way to pass our TypeSafe key through it.
- Our TypeSafe key is `ESCOTE_TYPESAFE_API_KEY`:
  - keep: `escote-typesafe-api-key.age`, listed at `keep/manifest.yml:85`
  - verified working 2026-10-09 in session `3b34c398`
  - It already reaches `borg.env`, `cortex.env` and `sb-harvest.env`, because all three env-bootstraps run the same `manifest secrets env` command (checked by listing the variable names the command emits).

### Problem

classifier.dev's keyless tier refuses our calls, so tags fall back to `deterministic` and every affected ingest is degraded.

- Since 2026-10-15 (`journalctl --user -u borg.service -u cortex.service --since 2026-10-15 | rg -c 'tag classifier failed'`): 488 failures, 128 from borg and 360 from cortex.
  - 446 × `402 request_spending_limit`
  - 38 × `429 free_ip_concurrency`
  - 3 × `429 free_ip_daily_budget`
  - 1 request failure
- Example: trace `cl-30ee5800` (2026-10-16 16:38:19) needed $0.0027525 against a $0.0027487 per-request cap.
- 264 receipts are `degraded=1` since 2026-10-15 (`receipts.db`). Not all of those are classifier failures.
- Cortex classify and `--retag` fail on every tick, not only borg ingest.

### Goals

- `tags.classifier.classifier: jev` calls TypeSafe directly with `ESCOTE_TYPESAFE_API_KEY`. (Scott: "USE JEV DIRECTLY INSTEAD.")
- No use or mention of classifier.dev remains in the repo's code, tests, fixtures, templates or agent docs. (Scott: "excise, eradicate, annihilate all mention and use of classifier.dev.")
  - Scope: everything outside `docs/`. Point-in-time design docs stay as history (Resolved Decisions).
- Tag quality under jev is measured against gold before ship. If it misses the bar, the work stops.
- The jev classifier behaves the way `classifier-dev` did to its callers:
  - same `TagClassifier` trait (`distillers/src/tags.rs:151`)
  - same `finish` rules: author tags at tier 0, cap of 8, subset guard, High/Low confidence
  - same `fallback` → `degraded` path

### Non-Goals

- **Multi-note batching.** TypeSafe evaluates one `state` per request (the docs: "Each request evaluates one state against one or more questions"). No production code calls `classify_batch`: `rg -n classify_batch --glob '!docs/**'` finds only the trait default (`tags.rs:155`), a comment (`:666`) and the `ClassifierDev` override (`:747`).
- **Changing the `deterministic` or `fabric-closed` classifiers**, or the `tags_degraded` → `receipt_degraded` wiring.
- **Reclassifying the vault.** A full `--retag` is an operator decision after deploy. A `--retag` leaves a note untouched when the result is empty or `Low`, so it does not rewrite every old provenance value.
- **classifier.dev references outside this repo:**
  - the `classifier` MCP server (`scottidler/claude/manifest.yml:90`)
  - the `bulk-classify` skill
  - the npm `classifier-dev` package (`scottidler/dotfiles/manifest.yml:293`)

  Those are separate work in their own repos. The exception is the dotfiles-managed `cortex.yml` and the dotfiles `borg.yml`, which are part of this rollout (operator step 1).

## Phase 0 Results (run 2026-10-16, ripr, key in the header from the env var, never echoed)

Zero code. The probe scripts died with the scratchpad.

| Probe | Result |
|---|---|
| `GET /openapi.json` | 200, TypeSafe v0.2.0; request is `{model, state, questions, assets?}`; the spec names no rate-limit header |
| `GET /v1/models` | `jev-latest`, `jev-preview`; the pinned `jev-1.13.0` is also accepted and is what `jev-latest` serves today |
| One note, every canonical tag as a noul question, `"Is this text about {tag}?"`, ~240 chars | 200 in 0.083s and 0.106s; 1,922 input / 1,998 output tokens |
| Same, scores at ≥0.9 | `rust .99`, `programming .99`, `software-engineering .99`, `tech .99`, `tutorial .93`; just under: `cli .82-.84`, `api .72`, `books .61` |
| Questions per request | 117, 300, 1,000 and 3,000 all 200; 3,000 in 0.92s, 41,592 input tokens |
| Hyphenated question keys (`ai-safety`) | accepted |
| 111k-char `state` | 200 in 0.23s, 30,302 tokens |
| Array `state` | one answer set; no per-element batching |
| 2 bursts × 60 concurrent calls | all 200, no rate-limit headers, no 429 |
| Bad key | 401 `{"detail":{"error_type":"authentication_error",...}}` |
| Bad question type | 400 `{"detail":{"error_type":"api_usage_error","message":"Invalid request."}}` |
| Unknown model | 400 `api_usage_error`, `Unknown model: jev-1.12` |
| Missing `state` | 422 FastAPI body whose `input` echoes the whole request |
| Every response | `x-typesafe-request-id` header |

What this settles:

- **One request per note, every canonical tag in it.** There is no question ceiling to shard around, so `shard_labels` and `merge_shard_scores` go.
- **The 422 body echoes the request**, so error text keeps only `detail.error_type` and `message`, never `input`.
- **Bare tag names are ambiguous for some tags.** `books .61` on a Rust note, and football names like `defense` and `rpo` collide with tech meanings. Phase 0b measures alternatives.

### Phase 0b: calibration and the ship gate (run 2026-10-16, zero code; winner C at 0.90)

**Inputs.** Start from the existing harness at `~/Claude/tags-only-trial/p0/`:

- `gold.json` (the gold tags) and `score.py` (the scorer)
- `descriptions.json`: 117 per-tag descriptions, used by the earlier A2 arm
- Do not use `notes.json`: it holds metadata only, with no text.

**State text.** Build each note's `state` with the production rule, not `wire.json`'s 900-char clip:

- `format!("{title}\n\n{text}")` (`distillers/src/tags.rs:676`)
- `text` is the note's summary verbatim when it has one (`TagSources::from_summary`, `borg/src/pipeline/tags.rs:68-71`), else the 900-char body head (`from_body`)
- Read from the vault's current files. Count and drop gold notes that no longer exist.

**Arms.** Each is direct `/v1/systemone`, one request per note, two identical runs:

- A: `instructions = "Is this text about {tag}?"`
- B: `instructions = "Is this text about {tag} (in the sense of {group})?"`, using the tag's group key from `canonical-tags.yml`
- C: arm A plus `criteria.true = <descriptions.json[tag]>`

Thresholds: 0.80, 0.85, 0.90, 0.95.

**Record**, per arm and threshold, scored against gold: precision, recall, F1, and identical-run instability (the share of notes whose ≥threshold set changes between the two runs).

**Baseline**, gold-scored, from the same harness (`p0/analysis.txt`), measured through classifier.dev's wrapper:

- A0: F1 0.741 single call, instability 0.195
- A2 (described labels): P/R/F1 0.911 / 0.694 / 0.788 at 0.90

**Ship gate:**

- Pick the arm and threshold with the highest F1 whose precision is ≥ 0.80 and whose instability is ≤ 0.195.
- If none qualifies, **stop**. Record the table in this doc and take the result to Scott. Phases 1-3 do not start.

**What the winner decides:**

- A wins: no new data path. The question text is built from the tag name alone.
- B wins: Phase 1 carries tag groups to the classifier. `CanonicalSet` gains a `group: HashMap<String, String>` filled by `canonical_set()` (`vault/src/canonical.rs:16-17`, which flattens groups away today).
- C wins: Phase 1 adds an optional per-tag `description` to `canonical-tags.yml`, read into `CanonicalSet`. The text comes from `descriptions.json`, reviewed in the diff.

The chosen arm, threshold and data path are written into Data Model, API Design and Phase 1 before Phase 1 starts.

#### Phase 0b results (run 2026-10-16, ripr, key in the header from the env var, never echoed)

- Model served: `jev-1.13.0` on every response of all six runs (requested `jev-latest`).
- Gold notes: 53 in `gold.json`. 46 used, 7 dropped because the vault no longer has them (all deleted in vault commit `6ab13d50`, "cleaned up"):
  - `plugin-sync-removal-security-review-no-vuln`, `cargo-toml-dep-bump-security-review-no-findings`, `slack-cli-valet-client-extraction-and-pr496-babysit`, `export-fixtures-json-security-review-no-risk`, `marquee-release-yml-and-verify-cli-discovery`, `platform-service-helm-httproute-fixture-security-review`, `review-kms-key-rotation-security-policy`
- One gold note was renamed, not deleted, and is kept under its current path: `ex-snowflake-engineers-say-there-s-a-blind-spot-in-data-engineering-so-they-built-tower-to-fix-it.md` is now `...-so-they.md` (same title and source URL).
- `state`: 39 of the 46 notes had a `## Summary` and were scored on it verbatim; 7 used the 900-char body head. 210 gold positives, 4.57 per note.
- Canonical rename: `kubernetes` became `k8s` (`939470e`) after gold and `descriptions.json` were written. Gold's `kubernetes` is scored as `k8s`, and arm C's `k8s` criterion is `descriptions.json["kubernetes"]`.
- Scoring follows `p0/score.py`: the predicted set is the scores ≥ threshold, highest first, capped at 8. P, R and F1 are micro-averaged per run, then averaged over the two runs. Instability is the share of the 46 notes whose predicted set differs between run 1 and run 2.

| Arm | Threshold | P | R | F1 | Instability | Tags/note | Gate |
|---|---|---|---|---|---|---|---|
| A | 0.80 | 0.654 | 0.719 | 0.685 | 0.130 | 5.02 | fail (P) |
| A | 0.85 | 0.705 | 0.688 | 0.696 | 0.043 | 4.46 | fail (P) |
| A | 0.90 | 0.740 | 0.636 | 0.684 | 0.130 | 3.92 | fail (P) |
| A | 0.95 | 0.856 | 0.538 | 0.661 | 0.043 | 2.87 | pass |
| B | 0.80 | 0.657 | 0.669 | 0.663 | 0.174 | 4.65 | fail (P) |
| B | 0.85 | 0.684 | 0.633 | 0.658 | 0.152 | 4.23 | fail (P) |
| B | 0.90 | 0.733 | 0.595 | 0.657 | 0.109 | 3.71 | fail (P) |
| B | 0.95 | 0.838 | 0.505 | 0.630 | 0.022 | 2.75 | pass |
| C | 0.80 | 0.756 | 0.769 | 0.763 | 0.087 | 4.64 | fail (P) |
| C | 0.85 | 0.792 | 0.724 | 0.756 | 0.152 | 4.17 | fail (P) |
| **C** | **0.90** | **0.840** | **0.650** | **0.733** | **0.065** | **3.53** | **pass, winner** |
| C | 0.95 | 0.901 | 0.519 | 0.659 | 0.087 | 2.63 | pass |

**Gate verdict: C at 0.90.** Four rows qualify (P ≥ 0.80, instability ≤ 0.195). C at 0.90 has the highest F1 of them: 0.733, next is C at 0.95 with 0.659. Every row clears the 0.195 instability bar, so precision is the only thing ruling rows out.

Against the classifier.dev baseline: F1 0.733 sits under A0's 0.741 and A2's 0.788, at lower instability (0.065 vs 0.195). The two are not like for like (53 notes on `wire.json`'s 900-char clip there, 46 notes on the production `state` here), and the gate does not ask jev to beat them.

C winning sets three things, written into the sections below:

- Data Model: `threshold` 0.9; `CanonicalSet` carries per-tag descriptions.
- API Design: `instructions = "Is this text about {tag}?"`, `criteria.true = <the tag's description>`.
- Phase 1: the `description` data path in `canonical-tags.yml`.

## Proposed Solution

### Overview

```
classify(note)
  -> Jev::classify
       key = resolve_secret(api-key)          // empty -> Err, zero requests
       text empty -> finish(no scores)          // zero requests
       POST /v1/systemone {model, state, questions: one noul per canonical tag}
       answers -> scores -> select_above_threshold -> finish()
  Err -> fallback classifier, tags_degraded = true   (unchanged)
```

### Architecture

- `distillers/src/tags.rs`:
  - `ClassifierKind::ClassifierDev` → `ClassifierKind::Jev` (yml `jev`).
  - `TagMethod::ClassifierDev` → `TagMethod::Jev` (`"jev"`). That string is what gets written to `cortex-classified-by`.
  - New `Jev` struct replacing `ClassifierDev` (`:543-764`):
    - It holds the resolved key, not the env var name. Precedent: `GeminiClient` (`borg/src/youtube/gemini.rs:276-316`) resolving through `vault::config::resolve_secret` (`vault/src/config.rs:125-133`).
    - Blocking `ureq`, because the trait is sync and cortex has no runtime. Precedent: `cortex/src/llm.rs:45`. `ureq` is already a distillers dependency.
  - Keep `finish` (`:714-737`) and `select_above_threshold` (`:371-380`) unchanged.
  - Delete:
    - `MAX_LABELS_PER_CALL`, `MAX_TEXTS_PER_CALL`, `DEFAULT_ENDPOINT` (`:25-35`)
    - `shard_labels`, `merge_shard_scores` (`:342-369`)
    - the `ClassifierDev` client and its response types
  - Drop `classify_batch` from `TagClassifier`. With `ClassifierDev` gone it has no override and no caller.
- **Test seam.** Borg builds its classifier inside a `spawn_blocking` closure through `distillers::tags::build(&cfg, ...)` (`borg/src/pipeline/tags.rs:167`), so a test cannot hand it a pre-built client. The seam rides on the config instead:
  - `TagsClassifierConfig` gets a `base_url: Option<String>` field, marked `#[serde(skip)]` and `#[cfg(any(test, feature = "test-util"))]`. It is never in yml.
  - `build` passes it to `Jev`. Production builds have no such field, so the URL is fixed.
  - The `test-util` feature already exists in `distillers/Cargo.toml`.
- `TagsClassifierConfig`:
  - gains `api-key` (env var NAME, default `ESCOTE_TYPESAFE_API_KEY`) and `model` (default `jev-latest`)
  - loses `token-env` and `endpoint`
  - gains `deny_unknown_fields`, so a stale `token-env` or `endpoint` key fails loudly instead of being ignored. Precedent: `YoutubeGeminiConfig` (`borg/src/config/engine.rs:49`).
- Daemon start, in both borg and cortex: when `classifier` or `fallback` is `jev` and the key resolves empty, log a WARN. Ingest still works through the fallback. Precedent: `validate_daemon_start` (`borg/src/config/engine.rs:173-208`).
- `sb doctor` `classifier_findings` (`sb/src/cli/checks.rs:536-596`):
  - The auth line reports whether `api-key` resolves.
  - The hint names the keep secret instead of `classifier.dev/app/keys`.
  - The three-label live probe stays and now hits `/v1/systemone`.

### Data Model

```rust
// distillers/src/tags.rs
#[serde(rename_all = "kebab-case")]
pub enum ClassifierKind { #[default] Deterministic, FabricClosed, Jev }

#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct TagsClassifierConfig {
    pub classifier: ClassifierKind,
    pub threshold: f32,          // template default 0.9 (Phase 0b: arm C at 0.90)
    pub fallback: ClassifierKind,
    pub timeout_secs: u64,       // unchanged; now one total deadline per classify (API Design)
    pub api_key: String,         // env var NAME, default "ESCOTE_TYPESAFE_API_KEY"
    pub model: String,           // default "jev-latest"
    #[cfg(any(test, feature = "test-util"))]
    #[serde(skip)]
    pub base_url: Option<String>,
}
```

YAML, as ripr will run it:

```yaml
tags:
  classifier:
    classifier: jev
    threshold: 0.9   # Phase 0b winner; set explicitly even though both live ymls already pin 0.9
    fallback: deterministic
```

Per-tag descriptions (Phase 0b arm C won), in `vault`:

```rust
// vault/src/canonical.rs
pub struct CanonicalTagsFile {
    // ...existing fields...
    #[serde(default)]
    pub descriptions: HashMap<String, String>,   // yml `descriptions:`, tag -> text
}

pub struct CanonicalSet {
    // ...existing fields...
    pub description: HashMap<String, String>,    // filled by canonical_set()
}
```

```yaml
# config/canonical-tags.yml
descriptions:
  agents: "AI agents that take actions with tools: coding agents, ..."
  k8s: "..."
```

- The text is `~/Claude/tags-only-trial/p0/descriptions.json`, reviewed in the diff. `k8s` takes the `kubernetes` entry.
- A `descriptions:` key that is not a canonical tag is a load error (fail closed, same as an unknown top-level key).
- A tag with no description is sent with no `criteria` (arm A's question for that tag). The shipped file carries all 117, and a test pins that.

### API Design

Request, one per note:

```json
{"model": "<model>",
 "state": "<title>\n\n<text>",
 "questions": {"<tag>": {"type": "noul",
                         "instructions": "Is this text about <tag>?",
                         "criteria": {"true": "<CanonicalSet.description[tag]>"}}}}
```

- The question shape is Phase 0b arm C. `criteria` is omitted for a tag with no description.

- `state` is today's classifier input, unchanged:
  - `format!("{title}\n\n{text}")` (`distillers/src/tags.rs:676`)
  - `text` is the summary verbatim (`from_summary`) or the 900-char body head (`from_body`)
- Header `Authorization: Bearer <key>`. The key appears nowhere else.
- Empty text (empty after trim): no request. It goes to `finish` with no scores, so author tags only, `Low` if there are none. Without this rule, the request is a guaranteed 422 that degrades the receipt.
- Response: `answers.<tag>.noul` → score.
  - A tag missing from `answers`, a `noul` outside `[0, 1]`, or unparseable JSON → `Err` (malformed), so the fallback runs.
  - Keys we did not ask for are dropped.
  - `model` must start with `jev`, else `Err`. The served `model` is logged at DEBUG.
- Errors. Each is an `eyre` error, as today. It carries the status, `detail.error_type`, `detail.message` (if present) and `x-typesafe-request-id`, and never the request body or the key:
  - 401 → `authentication_error`
  - 400 → `api_usage_error`
  - 422 → `validation_error`; the echoed `input` is discarded
  - 5xx, transport error, timeout → `Err`
- **One deadline per `classify`.** `timeout-secs` bounds the whole call: request, any sleep, retry, and body read. Cortex already checks `timeout-secs` against `daemon.stop-timeout-secs` as a per-call bound (`cortex/src/config.rs:1122-1131`), and that check stays true.
- **429.** The OpenAPI spec names no rate-limit header, and Phase 0 could not trigger a 429. So:
  - The delay comes from `retry-after-ms` (integer milliseconds) or `Retry-After` (integer seconds), whichever parses first.
  - One retry, only if that delay plus a request still fits inside the deadline.
  - A missing or malformed header, or a second 429 → `Err`.
- Every `Err` goes to the existing fallback path. That path sets `tags_degraded`, which makes the receipt degraded.

### Implementation Plan

#### Phase 0: Prove the API from ripr
**Model:** none (done)
- Results table above.
- **Success criteria:** met. One request with every canonical tag returned 200 with an answer per tag in under 0.11s.

#### Phase 0b: Calibration and ship gate
**Model:** opus
- As specified above. Zero code. The table lands in this doc under Phase 0b.
- **Success criteria:** met 2026-10-16.
  - P, R, F1 and instability are recorded for arms A, B and C at all four thresholds, each scored against gold (46 notes, 7 dropped).
  - Winner C at 0.90 (P 0.840, R 0.650, F1 0.733, instability 0.065), written into Data Model, API Design and Phase 1.

#### Phase 1: Jev classifier, callers compiled green
**Model:** opus
- In `distillers`: `ClassifierKind::Jev`, `TagMethod::Jev`, the `Jev` struct, the config fields, `deny_unknown_fields`, and the `base_url` test seam.
- Plus the Phase 0b data path (C won): `descriptions:` in `config/canonical-tags.yml` (117 entries from `descriptions.json`, `k8s` from its `kubernetes` entry), `CanonicalTagsFile.descriptions`, `CanonicalSet.description` filled by `canonical_set()`, and `Jev` sending each as `criteria.true` (Data Model, API Design).
  - Tests: a non-canonical `descriptions:` key fails to load; the shipped file has a description for every canonical tag; the stub sees `criteria.true` on each question.
- Delete the classifier.dev client, sharding, constants, `classify_batch`, and `distillers/src/tags/fixtures/classifier-dev-2026-09-20.json`.
- Every change a caller needs to compile:
  - `sb/src/cli/checks.rs:563` (`token_env`)
  - `TagMethod::ClassifierDev` → `TagMethod::Jev` in `cortex/src/classify/tests.rs` and `borg/src/pipeline/tags/tests.rs`
- Record a live `/v1/systemone` response as the new fixture. The key goes only in the header; scrub the body before saving.
- Stub tests, one per documented branch:
  - 200
  - 401, 400, 422 (no `input` in the error)
  - malformed JSON, missing tag, `noul` outside `[0, 1]`, wrong `model`
  - 429 then 200, 429 then 429, 429 with no header
  - empty key, empty text
- **Success criteria:**
  - A stub test sees exactly 1 request per `classify`. Its body has `questions.len() == canon.all.len()`, and every question has `type == "noul"`.
  - The 401 test's error contains `authentication_error` and not the key. The empty-key and empty-text tests see 0 stub hits.
  - `rg -i 'classifier[.-]dev|ClassifierDev|token_env|shard_labels|MAX_TEXTS_PER_CALL' distillers/src` returns nothing, and `otto ci` exits 0.

#### Phase 2: Borg and cortex wiring, doctor, daemon-start check
**Model:** sonnet
- borg: a pipeline test drives `finalize_tags` through the `base_url` seam against a stub. One case is a 200 (`tags_degraded == false`, `method == Jev`); one is a 500 (fallback runs, `tags_degraded == true`).
- The existing borg degraded test switches to an api-key name that resolves empty, with no network.
- `sb doctor` `classifier_findings`, as specified.
- The daemon-start WARN in borg and in cortex.
- Comments in `borg/src/pipeline/tags.rs`, `handlers.rs` and `cortex/src/classify.rs`.
- **Success criteria:**
  - The borg stub 200 test passes and fails when the stub returns 500. The 500 test passes and fails when `degraded` is forced false.
  - A borg daemon-start test and a cortex daemon-start test, each with `classifier: jev` and an empty key, log the WARN and still start.
  - `otto ci` exits 0.

#### Phase 3: Templates and agent docs
**Model:** sonnet
- Rewrite the tag blocks in `config/templates/borg.yml.example` (`:186-209`) and `config/templates/cortex.yml.example` (`:224-247`) for `jev` and the Phase 0b threshold (0.9).
- Rewrite the comments in `distillers/AGENTS.md:57` and `distillers/Cargo.toml:24-27`. Name jev in the root `CLAUDE.md` Tags bullet.
- **Success criteria:**
  - `rg -i 'classifier[.-]dev|ClassifierDev|CLASSIFY_API_KEY|CLASSIFIER_DEV_TOKEN' --glob '!docs/**'` exits 1 (no matches).
  - `otto ci` exits 0.

**Operator steps** (not phases), on ripr after ship:
1. Edit the `tags.classifier` block in the three yml files on ripr:
   - `~/.config/sb/borg.yml` (a plain file on ripr)
   - `~/repos/scottidler/dotfiles/HOME/.config/sb/cortex.yml` (ripr's `cortex.yml` symlinks here)
   - the dotfiles `borg.yml` (`:221` says `classifier-dev`)

   In each: set `classifier: jev`, set `threshold` to the Phase 0b value (0.9), and delete the stale classifier.dev comments. Commit the dotfiles change, or the next manifest apply reverts it.
2. Run `otto deploy` right after, which restarts borg and cortex.
   - The yml and the binary must change together. The old binary rejects `jev`, and the new one rejects `classifier-dev`.
   - Running daemons ignore the yml until they restart. Only `sb` CLI calls in between fail, and they fail loudly, naming the field.
3. Check `sb doctor`: the classifier probe is OK, and there is no key WARN.
4. Reingest `SDLfjrUyBw4`, then run `sb cortex classify --apply --retag notes/dolt-this-makes-sql-feel-like-git.md`.
5. Run `sb borg replay` on the traces in `sb borg log --degraded --since 2026-10-15` whose log line says `tag classifier failed`. Replay refetches each source, so this is the operator's call on how many.
6. In keep, delete `classify-api-key` from `manifest.yml:65` and run `rkvr rmrf` on `.secrets/classify-api-key.age`, which is a revoked key.

**Rollback:** set `classifier: deterministic` in the three ymls and restart. That restores availability, not tag quality. The `classifier-dev` variant no longer exists, so going back to classifier.dev means reinstalling the previous release.

## Acceptance Criteria

- [ ] No classifier.dev reference remains outside `docs/`.
  - Command: `rg -i -c 'classifier[.-]dev|ClassifierDev|CLASSIFY_API_KEY|CLASSIFIER_DEV_TOKEN' --glob '!docs/**'`
  - Observed on main (v0.17.6): 88 matching lines across 13 files, the most in `distillers/src/tags.rs` (30), `distillers/src/tags/tests.rs` (17) and `cortex/src/classify/tests.rs` (11).
- [ ] `rg -c '^\s+Jev,$' distillers/src/tags.rs` returns 2 (the `TagMethod` and `ClassifierKind` variants), and `rg -c 'ClassifierDev' distillers/src/tags.rs` exits 1.
  - Observed on main: `rg -c '^\s+ClassifierDev,$'` returns 2 (`:116`, `:170`); `rg -c '^\s+Jev,$'` prints nothing and exits 1.
- [ ] A distillers stub test sends exactly 1 request per `classify`, and the request carries one noul question per canonical tag.
  - Observed on main: cannot run; depends on Phase 1.
- [ ] On ripr after operator step 4, the `SDLfjrUyBw4` reingest receipt is `succeeded|0`, and the note carries `cortex-classified-by: jev`.
  - Commands: `sqlite3 ~/.local/share/sb/borg/receipts.db "select status,degraded from receipts where raw_input like '%SDLfjrUyBw4%' order by received_at desc limit 1;"` and `rg '^cortex-classified-by' ~/repos/scottidler/obsidian/notes/dolt-this-makes-sql-feel-like-git.md`
  - Observed on main: `succeeded|1` (trace `cl-30ee5800`, `classifier.dev returned 402 ... request_spending_limit`); the note says `cortex-classified-by: deterministic`.
- [ ] For one hour after the ripr deploy, `journalctl --user -u borg.service -u cortex.service --since "<deploy time>" -o cat | rg -c 'tag classifier failed'` exits 1 (no matches), AND the same window holds at least one cortex classify tick or borg ingest that ran the classifier.
  - Observed on main: 488 matching lines since 2026-10-15.

## Resolved Decisions

- 2026-10-16, Scott: **jev direct, classifier.dev excised.** Quoted in Goals.
- 2026-10-16, author, from Phase 0: **one request per note, no sharding.** 3,000 questions in one request returned 200 in 0.92s.
- 2026-10-16, author: **`model` defaults to `jev-latest`.** `/v1/models` lists only `jev-latest` and `jev-preview`, so a pinned id can 400 once it is retired. The served model is logged, and `model` is config for anyone who wants a pin. Same shape as `youtube.gemini.model`.
- 2026-10-16, author: **no serde alias for `classifier-dev`.** An alias keeps the name alive, which contradicts the excise order. The old value fails loudly at config load, and operator steps 1-2 handle it.
- 2026-10-16, author: **`classify_batch` leaves the trait.** It has no callers once `ClassifierDev` is gone.
- 2026-10-16, panel round 1 (architect + staff-engineer, synthesis `~/.cache/review-panel/runs/06B08h48/synthesis.md`). All 9 must-fix and 5 cheap-win findings are folded in:
  - **M1, M2:** Phase 0b now uses the gold-scored baseline and the p0 harness (`gold.json`, `score.py`, `descriptions.json`), not `notes.json`, and it can stop the work.
  - **M3:** arms B and C name their data path.
  - **M4:** Phase 1 carries every change callers need to compile, and its grep is scoped to `distillers/src`.
  - **M5:** the test seam rides on `TagsClassifierConfig.base_url` so it reaches borg's `spawn_blocking` build.
  - **M6:** one deadline per `classify`; 429 handling is defined for missing headers and a second 429.
  - **M7:** operator step 1 sets the threshold explicitly.
  - **M8:** the live criterion runs a targeted `--retag`.
  - **M9:** `state` is described as the code builds it, and Phase 0b calibrates that exact input.
  - **C1-C5:** trait name, `rg` exit-1 wording plus positive evidence, dotfiles configs, per-branch stub tests, both daemons named.
  - **Answers to the caller's questions:**
    - Q1: historical docs stay.
    - Q2: the 715 `classifier-dev` provenance values stay. They are indexed and counted, never dispatched on (`vault/src/search/index.rs:213`, `stats.rs:616`).
    - Q3: outside-repo references are separate work, except the dotfiles sb configs.
    - Q4: dropping `classify_batch` is safe.
    - Q6: the stub mechanism works; the wiring is M5.
  - **Pushbacks, with the synthesis's own verdicts:**
    - architect's blocking-pool exhaustion: rejected. `GENERAL_PERMITS` caps concurrent traces at 8 (`borg/src/config.rs:337`).
    - architect's "drop the `jev` model-prefix check": kept. It is a one-line fail-closed guard.
    - architect's deploy race: covered by operator steps 1-2.
    - staff's "operator step 5 is unrequested": step 5 now names its selector and leaves the count to the operator. Step 6 deletes a revoked key.

## Alternatives Considered

### Keep classifier.dev with a paid workspace key
- **Description:** Buy a classifier.dev workspace key and set `token-env`.
- **Pros:** No code change.
- **Cons:** A second subscription for a wrapper over a model we already pay for. Our TypeSafe key can't pass through it.
- **Why not chosen:** Scott ruled it out.

### `fabric-closed` as the primary classifier
- **Description:** Use the existing LLM closed-vocabulary classifier.
- **Pros:** Already built, and the key is already present.
- **Cons:** One LLM call per note, with no calibrated per-tag probability to threshold.
- **Why not chosen:** jev is the model the threshold design was built around, and Scott asked for it.

### Shard questions as classifier.dev did
- **Why not chosen:** Phase 0 found no ceiling up to 3,000 questions. Sharding only adds requests.

## Technical Considerations

### Dependencies
- No new crates. `ureq` and `serde_json` are already distillers dependencies.
- Secret: `ESCOTE_TYPESAFE_API_KEY`, already in keep and already reaching all three daemon env files through env-bootstrap.

### Performance
- ~0.1s and ~1.9k input tokens per note for every canonical tag on short text (Phase 0). Output tokens are free per TypeSafe's `usage` doc.
- TypeSafe's pricing page returned 404. A third-party gateway doc (Zuplo) quotes $0.042 per million input tokens, which puts a full vault retag (~1,561 notes) under a cent. That figure is unverified.

### Security
- The key travels only in the `Authorization` header to `api.typesafe.ai`.
- Error text never carries the request body. The 422 `input` echo is discarded.
- Note text leaves the host, the same as with classifier.dev today.

### Testing Strategy
- A loopback stub and a recorded fixture, with no network in `otto ci` (borg convention).
- Borg reaches the stub through the `base_url` config seam. Sync `ureq` runs on the `spawn_blocking` thread while the test runtime serves the stub. Never call sync `classify` directly on a current-thread runtime that is also serving the stub.

### Rollout Plan
- Ship. Then run operator steps 1-6 on ripr.
- desk and lappy configs are unchecked. Operator step 1 applies to any host with a `tags.classifier` block.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Direct noul scores calibrate differently from classifier.dev's wrapper | High | Precision or recall shifts | Phase 0b measures it against gold, and its gate can stop the work |
| Ambiguous bare tag names (`defense`, `books`) | Seen in Phase 0 | False positives | Phase 0b arms B and C |
| A deployed yml still says `classifier-dev` | Certain on ripr until step 1 | Config load fails, daemons don't start | Operator steps 1 and 2 run back to back; the error names the field |
| The dotfiles `cortex.yml` reverts the edit | Certain without the dotfiles commit | cortex back on a removed variant | Operator step 1 edits and commits the dotfiles copies |
| TypeSafe outage or rate limit | Low | Tags degrade | The fallback runs, and the receipt is degraded |
| `jev-latest` changes model under us | Medium | Scores shift | The served model is logged; `model` is config |

## Open Questions

- None.

## References

- TypeSafe OpenAPI: `https://api.typesafe.ai/openapi.json`; docs index `https://docs.typesafe.ai/llms.txt`
- `docs/design/2026-09-19-tags-only-classification.md` (the gold corpus)
- `~/Claude/tags-only-trial/p0/analysis.txt` (the gold-scored baseline)
- `docs/design/2026-10-16-youtube-gemini-engine.md` (the client and secret precedent)
- clyde session `3b34c398` (the key, verified 2026-10-09)
