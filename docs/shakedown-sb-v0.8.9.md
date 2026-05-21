# CLI Shakedown Report: sb v0.8.9

**Date:** 2026-05-20
**Binary:** `/home/saidler/.cargo/bin/sb` (61.9 MB, built via `otto deploy`)
**Vault under test:** `~/repos/scottidler/obsidian/` (1365 notes, 99.3% embedding coverage)
**Config layout:** `~/.config/sb/` (unified, post-Phase-1a)
**Commits under test (v0.8.6 → v0.8.9):** D1 location-line restore (2d7a5ca), D2 not-found split (de08799), D3 doctor exit-on-error (f0dda02), D4 `results` key unification (476e60e), O3 startup-log demotion (67ff7a2), `ReingestReport` typed return (0662860), version bump (52ef09b).

## Summary

| Metric | Count |
|--------|-------|
| Top-level subcommands discovered | 7 (`borg`, `cortex`, `oracle`, `status`, `doctor`, `bootstrap`, `help`) |
| Distinct commands tested | ~50 |
| Commands passed | 49 |
| Commands flagged (defect or inconsistency) | 2 |
| Commands skipped (mutating, long-running, or auth-bound) | many - see "Skipped" below |
| Pipelines tested | 6 |
| Edge cases tested | 8 |

D1, D2, D3, D4 from the v0.8.6 shakedown all verify clean against v0.8.9. O3 (boot-time noise) is **partially** addressed: the borg::startup pair and the sb::cli::cortex version/vault-root pair are gone from one-shot invocations, but two foundational lines (`vault::logging: Logging initialized`, `borg::config: Loaded config from`) still emit at info on every short read-only command. The new findings are a dry-run vocabulary inconsistency in `cortex sweep --cold` (D5), and an `ingest_history` ergonomic gap where the tool silently accepts (and ignores) an unknown `limit` parameter despite returning 1122 rows by default (D6).

## Command Results

### Top-level health

| Command | Exit | Result |
|---|---|---|
| `sb --version` | 0 | `sb v0.8.9` |
| `sb status` | 0 | All systemd / config / shared-config / patterns / embedding-cache / vault sections render. 1 warn (11 pending DLQ rows) is real vault state. |
| `sb doctor` | 0 | Same data sorted by severity. D3 verified: would exit 1 only on Error-severity findings; current vault has none. |
| `sb borg --version` | 2 | clap rejects with `error: unexpected argument '--version' found` (top-level `--version` is the canonical path). Cosmetic. |

### borg (read-only)

| Command | Exit | Sample |
|---|---|---|
| `sb borg daemon --status` | 0 | Full systemctl block. Daemon binary path now: `/home/saidler/.cargo/bin/sb borg daemon --start` (post-unification - O1 from v0.8.6 resolved). |
| `sb borg intake list` | 0 | Markdown-style table; 50 default rows; clean column alignment with mixed origin widths (`telegram` chat IDs vs `http`). |
| `sb borg intake list --limit 3 --method telegram` | 0 | Filter combinations work. |
| `sb borg intake show tg-319a18` | 0 | Row body + 71-byte sidecar content. |
| `sb borg dlq list` | 0 | 11 watchdog-orphan rows, all `pending`. |
| `sb borg dlq list --status pending --limit 3` | 0 | Filter + limit honored. |
| `sb borg dlq show ht-575bd1` | 0 | DLQ row + intake row + sidecar + cross-ref note `(ledger has a completed row for this source: ...)`. |
| `sb borg blocklist list` | 0 | `(blocklist empty)`. |
| `sb borg retention status` | 0 | `Staging root: ~/.local/share/borg/stages`, 0 traces, 0 bytes. |
| `sb borg audit` | 0 | Tally: 49 `[DUPLICATE]`, 6 `[RAW-TITLE]`, 4 `[ORPHAN-REPLACE]`, 2 `[INFO]`, 1 `[BLOCKED]` - all real vault state. |
| `sb borg reingest --dry-run --type article` | 0 | Lists 195 articles it would reingest. |
| `sb borg backfill-ingested --dry-run` | 0 | Summary: scanned 1238, would backfill 115, skipped 477 (already had `ingested:`), skipped 646 (origin != assisted). |
| `sb borg migrate --dry-run` | 0 | Scanned 1238 files, 0 would change. |

### cortex (read-only)

| Command | Exit | Sample |
|---|---|---|
| `sb cortex daemon --status` | 0 | systemctl block. RSS 1.3 GB (known candle memory issue, unrelated). |
| `sb cortex lint` | 0 | Total: 533 errors, 733 warnings, 3919 info. |
| `sb cortex lint --format json` | 0 | Valid JSON array of 5185 items; each has `severity`, `rule`, `path`, `message`. Tallies match human format exactly. |
| `sb cortex link` | 0 | 0 errors, 0 warnings, 44 info (concept-link suggestions). |
| `sb cortex link --scan people` | 0 | 0 violations. `--scan` enum validation works. |
| `sb cortex state` | 0 | `Last scan: 2026-05-21 00:45:02 UTC (1365 files)`. |
| `sb cortex state --diff` | 0 | `0 added, 0 removed, 20 modified` with per-path list. |
| `sb cortex sweep --dry-run` | 0 | `No new tag proposals.` |
| `sb cortex sweep --proposals` | 0 | Same (no proposals in queue). |
| `sb cortex sweep --cold --dry-run` | 0 | `scanned=1365 surfaced=0 pinned_excluded=0` - see Defects D5. |
| `sb cortex summarize --backfill --dry-run --domain tech` | 0 | Lists 116 `[would-distill]` candidates with kind annotations. |

### oracle (read-only via `sb oracle call`)

| Tool | Exit | Sample |
|---|---|---|
| `sb oracle stats` | 0 | 1365 notes, full domain/type/status breakdown. |
| `sb oracle call --list` | 0 | 18 tools listed with descriptions. |
| `sb oracle call vault_overview` | 0 | JSON with `by_domain`, `by_status`, `by_type`, `schema_gaps`, `total_notes`. |
| `sb oracle call schema_info` | 0 | Enums: 12 domains, 21 note_types, 3 origins, 4 statuses, 7 methods. |
| `sb oracle call domain_brief --json '{"domain":"tech"}'` | 0 | 168 notes, 76 unread, by_type breakdown. |
| `sb oracle call knowledge_search --json '{"query":"rust async","limit":3}'` | 0 | Hybrid mode (default), 3 results, full note metadata. |
| `sb oracle call knowledge_search --json '{...,"mode":"bm25"}'` | 0 | BM25-only: 3 results. |
| `sb oracle call knowledge_search --json '{...,"mode":"vector"}'` | 0 | Vector-only: 3 results. |
| `sb oracle call tag_search` | 0 | 50 tags, top: `llm=395`, `claude=252`, `agents=231`. |
| `sb oracle call list_notes --json '{"domain":"tech","limit":3}'` | 0 | 3 results, full metadata. |
| `sb oracle call recent_activity --json '{"days":7}'` | 0 | 20 rows in the 7-day window. |
| `sb oracle call inbox_status` | 0 | Has `results` key (D4 ✅). Keys: `classified`, `inbox_count`, `needs_review`, `results`, `review_candidates`. |
| `sb oracle call ingest_history --json '{"limit":3}'` | 0 | Has `results` key (D4 ✅). `limit` is silently accepted but absent from the input struct - see D6. |
| `sb oracle call duplicate_groups` | 0 | 10 groups. |
| `sb oracle call quality_report` | 0 | Keys: `distribution`, `results`. |
| `sb oracle call classify_status` | 0 | `by_confidence`, `by_domain`, `by_method`, `inbox_count`, `pending_review`, `total_classified`, `unclassified`. |
| `sb oracle call source_browse` | 0 | Top: `youtube.com=805`, `github.com=33`, `xda-developers.com=32`. |
| `sb oracle call creator_browse` | 0 | Top: `AI News & Strategy Daily | Nate B Jones=64`, `Chase AI=28`. |
| `sb oracle call note_read --json '{"path":"...","detail":"tldr"}'` | 0 | tldr-shaped response (D2 contract verified). |
| `sb oracle call find_links --json '{"path":"..."}'` | 0 | Returns `inbound`, `outbound`, `note`, `orphan`. |
| `sb oracle call find_similar --json '{"path":"..."}'` | 0 | 2 results. |
| `sb oracle call find_similar --json '{"content":"..."}'` | 0 | 3 results. Response shape is `{count, results: [{path, title, domain, ...}]}` with no `score` field - see O.find-similar-score. |

## Defects & Bugs

### D5. `cortex sweep --cold --dry-run` uses real-run vocabulary

**Severity:** cosmetic
**Command:** `sb cortex sweep --cold --dry-run`
**Actual output line:**
```
2026-05-20 18:26:31.773 [INFO] cortex::sweep: run_cold: scanned=1365 surfaced=0 pinned_excluded=0 report=/home/saidler/repos/scottidler/obsidian/system/views/cold-notes.md
Cold sweep: scanned=1365 surfaced=0 pinned_excluded=0
```
Other dry-run paths use `would-distill`, `WOULD set`, `would backfill` - clear past-tense conditional language. Cold-sweep's dry-run says `Cold sweep: scanned=...` (same as a real run) and mentions the `report=` path as if it were written. Even with `surfaced=0` there's nothing to write so the difference is invisible here, but a non-empty cold run in `--dry-run` would be ambiguous. Align the log/output vocabulary.

### D6. `ingest_history` silently accepts (and ignores) a `limit` parameter

**Severity:** ergonomic gap (not a bug per spec, but a sharp edge)
**Command:** `sb oracle call ingest_history --json '{"limit": 3}' 2>/dev/null | jq '{count, results_len: (.results | length)}'`
**Result:**
```json
{ "count": 1122, "results_len": 1122 }
```

Verified against source (`oracle/src/tools.rs:133`): `IngestHistoryRequest` has fields `source`, `domain`, `after`, `before` - and no `limit`. The tool description matches this (*"Filter by source URL, domain, or date range"*). Serde discards unknown fields by default, so `limit: 3` is accepted without complaint and quietly dropped. By contrast, `knowledge_search`, `list_notes`, `find_similar`, `recent_activity` all have a `limit` field and honor it.

This is a tool-shape inconsistency: the user's intuition is "every list-returning tool takes `limit`," and `ingest_history` returns a list. The fix is either:

1. **Add a `limit` field** to `IngestHistoryRequest` with a sensible default (e.g. 50), matching the rest of the surface. Recommended - 1122 rows is a lot of JSON for one call.
2. **Or add `#[serde(deny_unknown_fields)]`** so the user gets `unknown field 'limit'` and learns the right shape.

Option 1 is the better fix; option 2 is the minimum to prevent silent failure.

## Observations (not defects)

### O.find-similar-score. `find_similar` returns no similarity score

**Source:** `oracle/src/server.rs:557` builds the response as `{count, results: [...]}` from `Self::format_note(n, &detail_level)`. No per-result score is emitted - only note metadata. The underlying `db.find_similar(...)` necessarily computes a similarity ranking to order the rows, but the score is discarded before serialization.

For a tool whose entire purpose is similarity, exposing the score would let consumers threshold ("only show similarity > 0.7") or visualize ranking confidence. Cheap to add - one more field per row in `format_note` or alongside it. Not blocking; the rows are already in similarity-ranked order.

### O3-followup. Two foundational startup-log lines still emit at info

The v0.8.7 commit `67ff7a2` ("quiet one-shot CLI startup logs (O3)") demoted four lines to debug:

- `borg::startup` pipeline permits initialized
- `borg::startup` ffmpeg thread caps
- `sb::cli::cortex` cortex starting (version=...)
- `sb::cli::cortex` resolved vault root

That part verifies clean. But the original v0.8.6 finding listed **four** lines, of which two were not in scope of that commit:

```
2026-05-20 18:23:59.129 [INFO] vault::logging: Logging initialized (level=info), writing to: ...
2026-05-20 18:23:59.155 [INFO] borg::config: Loaded config from: /home/saidler/.config/sb/borg.yml
2026-05-20 18:23:59.156 [INFO] cortex::config: loaded config: /home/saidler/.config/sb/cortex.yml
2026-05-20 18:23:59.158 [INFO] borg::config: Loaded config from: /home/saidler/.config/sb/borg.yml
```

These four lines emit on every `sb status`, `sb doctor`, `sb borg ...`, `sb cortex ...` invocation. They're the "foundational" lines (logger init, config load) that O3's fix didn't touch. The cleanest second pass would demote these to `debug!` too, and have one-shot CLI paths surface a single summary line on demand. A separate follow-up.

### O. `sb status` has no `--json` output mode

Useful for ops automation (monitoring, alerting). All sections in the human-readable output map cleanly to a structured shape (`systemd.borg`, `vault.notes`, `embedding.coverage`, etc). Not blocking - `sb oracle call vault_overview --json` covers part of the same ground.

### O. Per-domain pipeline targeting is the right primitive

`sb cortex summarize --backfill --dry-run --domain tech` returned 116 candidates - small enough to inspect, scoped to a domain. Same shape for `--type`, `--source`, `--before`, `--after`. The lookup-then-targeting flow is genuinely useful and feels well-thought-out.

### O. JSON shape across `oracle call` tools is consistent

Every tool that returns a list uses the `results` key (post-D4). Single-item / metadata tools use top-level keys (e.g. `total_notes`, `by_domain`). Error responses use `Error:` on stderr with non-zero exit. This is good MCP-shape consistency.

## Pipeline Recipes (working, copy-pasteable)

```bash
# Top 5 vault domains by note count
sb oracle call vault_overview 2>/dev/null \
  | jq -r '.by_domain[] | "\(.[1])\t\(.[0])"' | sort -rn | head -5

# Top 5 tags by frequency
sb oracle call tag_search 2>/dev/null \
  | jq -r '.results[] | "\(.count)\t\(.tag)"' | sort -rn | head -5

# Top 5 source domains
sb oracle call source_browse 2>/dev/null \
  | jq -r '.results[] | "\(.count)\t\(.host)"' | sort -rn | head -5

# Lint Error breakdown by rule
sb cortex lint --format json 2>/dev/null \
  | jq -r '.[] | select(.severity == "Error") | .rule' | sort | uniq -c | sort -rn

# DLQ pending traces aggregated by stage
sb borg dlq list --status pending --limit 50 2>/dev/null \
  | awk 'NR>1 {print $4}' | sort | uniq -c

# Chained: latest recent_activity domain -> domain_brief
domain=$(sb oracle call recent_activity --json '{"days":7}' 2>/dev/null \
  | jq -r '.results[0].domain')
sb oracle call domain_brief --json "{\"domain\":\"$domain\"}" 2>/dev/null \
  | jq '{domain, total: .total_notes, unread, starred}'

# Chained: top source host -> notes from that host
host=$(sb oracle call source_browse 2>/dev/null | jq -r '.results[0].host')
sb oracle call source_browse --json "{\"host\":\"$host\"}" 2>/dev/null \
  | jq '.results[0:5] | map(.title)'

# Chained: knowledge_search top hit -> note_read tldr
path=$(sb oracle call knowledge_search --json '{"query":"second brain","limit":1}' \
  2>/dev/null | jq -r '.results[0].path')
sb oracle call note_read --json "{\"path\":\"$path\",\"detail\":\"tldr\"}" 2>/dev/null \
  | jq '.tldr'
```

## Edge Cases (all handle gracefully)

| Input | Exit | Behavior |
|---|---|---|
| `sb borg intake show nonexistent-trace` | 1 | `Error: trace_id nonexistent-trace not found in intake log` |
| `sb borg dlq show` (missing arg) | 2 | clap usage error with `<TRACE_ID>` hint |
| `sb borg ingest` (no URL or `--clipboard`) | 1 | `Error: No URL provided. Use a URL argument or --clipboard` |
| `sb cortex summarize --domain tech` (no `--backfill`) | 1 | `Error: cortex summarize requires --backfill; no other modes are implemented yet` |
| `sb oracle call unknown-tool` | 1 | `Error: unknown tool: unknown-tool (use oracle call --list)` |
| `sb oracle call domain_brief --json '{"domain":"nonexistent-domain"}'` | 1 | `Error: domain_brief: unknown variant 'nonexistent-domain', expected one of 'ai', 'tech', ...` (enum hint) |
| `sb oracle call knowledge_search --json '{"limit": 0}'` (missing query) | 1 | `Error: knowledge_search: missing field 'query'` |
| `sb oracle call knowledge_search --json '{"query":"asdfghjkl-nothing-matches-this-xyz","limit":3,"mode":"bm25"}'` | 0 | Empty results: `{count:0, results_len:0}` (correct - BM25 mode returns 0 when no terms match) |
| Same query in hybrid mode | 0 | Returns 3 vector-similarity-based results - hybrid never truly "empties" because the vector branch always scores something. **Worth documenting in the tool description.** |

Exit-code conventions verified: 0 success, 1 application error, 2 clap usage error.

## Release Validation

| Check | Status |
|---|---|
| Tag `v0.8.9` exists locally | yes |
| Tag `v0.8.9` exists on origin | yes (`refs/tags/v0.8.9` listed by `gh api`) |
| Tag is annotated | yes (`git cat-file -t v0.8.9` → `tag`) |
| Tag points to current HEAD | yes (`git rev-list -n 1 v0.8.9` = `52ef09b...`) |
| GitHub release for v0.8.9 | **none** - `gh release view v0.8.9` returns `release not found`; `gh api repos/.../releases` returns `[]` |
| Release pipeline (`.github/workflows/`) | **none** - directory does not exist in repo |
| Local `sb --version` | `sb v0.8.9` (matches tag) |
| Binary ELF | x86-64, dynamically linked, not stripped |

The repo has no release automation. Tags are published; binaries are not. For a personal-use tool deployed via `otto deploy` from the local workspace, this is acceptable. The matrix below is what *would* be needed if a release workflow were added later:

| Target | Present? |
|---|---|
| `sb-x86_64-unknown-linux-gnu` | absent (no workflow) |
| `sb-aarch64-unknown-linux-gnu` | absent |
| `sb-x86_64-apple-darwin` | absent |
| `sb-aarch64-apple-darwin` | absent |

## Skipped (intentional)

- **Mutating verbs:** `borg ingest`, `borg note`, `borg hotkey --install`, `borg sign`, `borg migrate --apply`, `borg dlq replay/archive`, `borg reingest` (without `--dry-run`), `borg replay` (without `--dry-run`), `borg retention sweep` (without `--dry-run`), `borg reingest-failed` (without `--dry-run`), `borg blocklist remove/clear`, `borg backfill-ingested` (without `--dry-run`), `borg dashboard refresh`, `cortex classify --apply`, `cortex lint --apply`, `cortex link --apply`, `cortex sweep --migrate`, `cortex migrate --apply`, `cortex summarize --backfill` (real, not dry-run), `cortex daemon --install/uninstall/start/stop`, `cortex embed --backfill`, `cortex intel --daily/--weekly`, `bootstrap`
- **Long-running:** `oracle serve` (blocks on stdio MCP), `borg/cortex daemon --start`
- **Interactive / device-bound:** `borg sign` (browser AMO upload), `bootstrap` (downloads ~100 MB embedding model)
- **Heavy:** `oracle index`, `oracle call reindex` (rewrite SQLite index; the running cortex daemon already does this incrementally)

## Verdict

v0.8.9 ships clean. All four v0.8.6 defects (D1-D4) verify in production. The O3 startup-log fix lands on the lines the commit message named; two upstream lines (logger init + config load) remain at info and are tracked as O3-followup. The new findings are small: D5 (dry-run vocabulary inconsistency in `cortex sweep --cold`) and D6 (`ingest_history` accepts but ignores `limit` because the field isn't in the input struct), plus one observation on `find_similar` not exposing its similarity score. The 50+ tested commands all behave as documented, JSON shapes are consistent across the oracle surface, and exit codes are correct.

Recommended follow-up scope (one small design doc):

1. **D6** - add a `limit` field to `IngestHistoryRequest` (default 50) so `ingest_history` matches the rest of the oracle surface; 1122 rows is too many for one call.
2. **O.find-similar-score** - expose the similarity score in `find_similar` results so consumers can threshold/rank.
3. **O3-followup** - demote `vault::logging` init and `*::config` load lines to debug for one-shot paths.

**D5** is small enough to fix in a one-line vocabulary change inside `cortex::sweep` without a design doc.
