# CLI Shakedown Report: sb v0.15.1

Date: 2026-09-20. Host: desk. Focus: verify the `domain` classification facet is
fully excised and `tags` is the sole classification facet, across borg ingest,
cortex, harvest, oracle, and the doctor/status/schema surfaces.

Read-only run. Every mutating verb was discovered and documented, none executed.

## Summary

| Metric | Count |
|--------|-------|
| Help nodes discovered | 58 |
| Commands / tool calls executed | 38 |
| Passed | 36 |
| Failed | 0 (2 blocked, see below) |
| Skipped (mutating) | 24 verbs |
| MCP tools enumerated | 19 |
| Pipelines tested | 9 |
| Edge cases tested | 6 |

## Verdict on the domain excision

**PASS.** Six independent probes, no hit anywhere the facet would matter.

| Probe | Result |
|---|---|
| Full `--help` tree (58 nodes) grepped for `domain` | 7 hits, all `sb borg blocklist` (Gate-0 URL host blocklist). Zero classification hits. |
| All 19 MCP tool schemas, dumped via `tools/list` over stdio | `occurrences of 'domain': 0`. Full param vocabulary is 25 names; `tag`/`tags` are the only classification facet. |
| `sb oracle call schema_info` | Exactly 5 keys: `methods`, `note_types`, `origins`, `statuses`, `tags`. 117 canonical tags. No `domains`. |
| Vault frontmatter, every key across 3809 notes with frontmatter | `domain` appears **0** times. `tags` on 3758. |
| `sb cortex migrate --only v6-drop-domain` (dry run, 3799 notes) | `No violations found.` The drop migration is idempotent-empty. |
| Returned note records from `list_notes` | Fields are `creator, date, origin, path, source, status, tags, title, trace, type`. No `domain`. |

Prose mentions of the word survive in 243 vault markdown files (note bodies, old
design docs) plus one deliberate migration record at
`system/schemas/frontmatter.md:117`. Those are correct and expected.

`tag-mapping.yml` holds six entries whose *tag names* contain the word
(`domain-blocking: networking`, `domain-expertise: null`, ...). Unrelated to the
retired facet; they are candidate-tag mappings.

## Resolution

Every finding below is closed in **v0.15.2**. Details and the break-the-code
evidence are in Addendum C7 of
`docs/design/2026-09-19-tags-only-classification.md`.

| # | Finding | Fix |
|---|---|---|
| 1 | Four schema value docs drifted | `sb cortex schema --render`; `--check` now exits 0 |
| 2 | `deprecated-drops` absent from the deployed cortex.yml | `deprecated-drops: [domain]` added; guard proven to fire |
| 3 | Four stale `domain` entries in exempt lists | removed |
| 4 | Unknown request keys silently dropped | `deny_unknown_fields` on all 19 request structs |
| 5 | `tag_brief` had no not-found signal | `known` + `message` fields |
| 6 | AND-mode tag filtering unreachable | `tags_mode: any \| all` on all 8 tags-bearing tools |
| 7 | `tag_search` page size indistinguishable from the total | `total_tags` + `truncated` |
| 8 | 1075 notes carry no tags | no change: reconciles exactly with lint policy, see the table |
| Obs | 57 clap flags had no help text | all documented |

## Findings

### 1. Open operator step: schema value docs drifted (known, C4)

`sb cortex schema --check` exits 1:

```
drifted  system/schemas/type-values.md
drifted  system/schemas/origin-values.md
drifted  system/schemas/status-values.md
drifted  system/schemas/tag-values.md
4 file(s) differ from the binary. Run `sb cortex schema --render` to update them.
```

`sb doctor` and `sb status` both surface it. Fix: `sb cortex schema --render`.
Note the list has four entries, not five: `domain-values.md` is gone from the
vault, which is itself excision evidence.

### 2. Open operator step: `deprecated-drops: [domain]` missing (known, C4)

The deployed `~/.config/sb/cortex.yml` has no `deprecated-drops` key at all.
Live evidence that this matters: the lint rule histogram fires
`frontmatter.deprecated.author`, `.day`, `.time`, `.url`, and **no**
`frontmatter.deprecated.domain`. If a pre-P11 daemon on the laptop or mini
writes `domain:` back into a synced note, nothing flags it.

### 3. New: four stale `domain` entries in the deployed cortex.yml

```yaml
frontmatter:
  exempt:
    daily: [domain, tags]
  path-exempt:
    "inbox/**":    [domain, origin, tags]
    "notes/ai/**": [domain, origin, tags]
    "entities/**": [domain, origin, tags]
```

These exempt a key that no longer exists. Harmless at runtime, but they are the
last configuration references to the retired facet, and they read as though the
facet is still live. Severity: cosmetic / config drift.

### 4. Confirmed open: MCP request structs accept unknown keys (C3)

A stale client's retired facet is silently dropped rather than rejected:

```
sb oracle call list_notes --json '{"domain":"tech","limit":3}'   -> 3 unfiltered notes, exit 0
```

Worse, the same gap turns a plausible-looking AND filter into a wrong answer:

```
sb oracle call list_notes --json '{"tags_all":["rust","llm"],"limit":5}'
-> returns "Tag Values", "Type Values", "Status Values" ... notes carrying NEITHER tag
```

`tags_all` is not on the schema, so it is parsed, discarded, and the call
degenerates to an unfiltered list. Severity: bug. Fix is the already-planned
`deny_unknown_fields`.

### 5. Confirmed open: `tag_brief` has no not-found signal (C3)

```
sb oracle call tag_brief --json '{"tag":"definitely-not-a-tag"}'
-> {"by_type": [], "results": [], "starred": 0, "tag": "definitely-not-a-tag", "total_notes": 0, "unread": 0}
```

A caller cannot distinguish "canonical tag with no notes" from "no such tag".
Severity: bug (low).

### 6. Observation: AND-mode tag filtering is not exposed

`vault::search` supports AND (`tags_all`) and the debug logs carry it, but
neither `list_notes` nor `knowledge_search` exposes it. Both document `tags` as
"OR across the list", and that is all a client can reach. Under a tags-only
regime the tag filter carries every query that `domain` used to narrow, so
OR-only is a real ceiling. Severity: suggestion.

### 7. Observation: `tag_search` page size vs. documented default

The schema says `limit` defaults to 20. A no-arg call returns 50 rows with
`count: 50`; `{"limit":200}` returns all 115 with `count: 115`. `count` is the
page length, not the total, so a client cannot tell it was truncated. The 115
matches `sb doctor`'s "115 distinct tag(s)". Severity: cosmetic.

### 8. Observation: 1075 notes carry no tags, and the numbers reconcile

`sb doctor` reports `tags=4` gaps; `vault_overview.schema_gaps` reports 1065.
Both are right and the gap is policy, not a bug:

| Where | Count | Status |
|---|---|---|
| `entities/` (hub stubs from `cortex hub`) | 915 | path-exempt |
| `journal/` | 88 | `daily` exempt |
| `notes/ai/daily/` (no `tags:` key at all) | 50 | path-exempt (`notes/ai/**`) |
| `system/` | 17 | path-exempt |
| stragglers (`test_folder/`, `home.md`, `inbox/.claude`) | 5 | |
| **non-exempt** | **4** | matches lint's `frontmatter.required.tags = 4` |

Oracle counts raw index nulls; cortex lint applies the path exemptions. Nothing
to fix, but the 100x difference between the two "gaps" numbers is a foot-gun.

### 9. Not yet observable: the harvest `scope:` key

P6 makes `scope:` a frontmatter key on every harvest note. No harvest has run on
v0.15.x (last harvest receipt is 2026-09-19T10:05Z, the deploy was 2026-09-20
20:39 PDT), so no vault note demonstrates it. The three notes carrying `scope:`
are hand-authored. `sb borg harvest --dry-run --limit 3 --since 7d` selects 2
threads and rejects 1, so the path is live and ready; a live run would prove it.

### 10. Known, out of scope: classifier key 401

`classify_status` shows `deterministic: 1207 / llm: 1041` historically, and
every fresh ingest lands `cortex-classified-by: deterministic`. `CLASSIFY_API_KEY`
still returns 401, as Addendum C4 records.

## Live surface checks

### borg ingest (the publish path)

The newest ingested note, `ht-dcce2864`:

```yaml
type: youtube
origin: assisted
method: http
cortex-classified: true
cortex-classified-by: deterministic
cortex-confidence: high
tags:
  - ai
  - architecture
  - data
  - llm
  - networking
  - software-engineering
```

Block-form tags, six canonical values, no `domain`.

### borg harvest (the session path)

Newest harvest note, `hv-578682e7`: `type: session`, `origin: generated`,
`method: harvest`, `cortex-session-ids` block, `tags: [cli, design, mcp, rust,
security, tech]`. No `domain`.

`sb borg harvest --dry-run --limit 3 --since 7d` (60 s, clyde export):

```
DRY RUN - nothing written
Would select 2 thread(s) -> 2 note(s)
Would reject 1 candidate(s):
  159a590f... - enrich-status is skipped-personal, not ok
```

### cortex

| Command | Result |
|---|---|
| `cortex lint --format json` | 21 rules fire, 1551 violations. No `domain` rule. `frontmatter.required.tags = 4`. |
| `cortex lint --rule tags --format json` | 2 `tags.orphan` infos (single-use tags). |
| `cortex sweep --dry-run` | `No new tag proposals.` The vault's tags are all canonical or mapped. |
| `cortex migrate --only v6-drop-domain` | `No violations found.` |
| `cortex schema --check` | exit 1, four files drifted (finding 1). |

### doctor / status

With network egress allowed, the only true failures are the unreachable systemd
user bus (this session cannot reach it; both daemons *are* running, verified by
`readlink /proc/<pid>/exe`) and the drifted schema docs. Everything else is
green: three configs parse, three shared catalogues match the binary, 26
patterns match, fabric live probe OK, signal OK, telegram OK, 3745 notes indexed
across **115 distinct tags**, embedding coverage 99.9%.

The `115 distinct tag(s)` line is the 0.15.0 fix for the old bug that printed
the top-20 cap. `vault_overview` now carries both: `by_tag` (top 20) and
`distinct_tags: 115`.

## Output format matrix

| Surface | Format | Works |
|---|---|---|
| `sb oracle call <tool>` | JSON on stdout, logs on stderr | yes, `jq`-clean once stderr is separated |
| `sb cortex lint` | `--format human` / `--format json` | both |
| `sb borg log` | table | aligned, no overflow observed |
| `sb doctor` / `sb status` | severity-tagged text | consistent glyphs, one suggested fix per failure |

Note: `sb oracle call ... 2>&1 | jq` fails because the config INFO line goes to
stderr and merging breaks the JSON. Use `2>/dev/null`. Correct behavior, easy
trap.

## Pipeline recipes (all run, all work)

```bash
# Every canonical tag with its note count, biggest first
sb oracle call tag_search --json '{"limit":200}' 2>/dev/null \
  | jq -r '.results[] | "\(.count)\t\(.tag)"' | sort -rn | head -20

# Tag-filtered semantic search, titles only
sb oracle call knowledge_search --json '{"query":"rust async","tags":["rust"],"limit":3,"detail":"metadata"}' \
  2>/dev/null | jq -r '.results[].title'

# Prove a tag filter actually filtered
sb oracle call knowledge_search --json '{"query":"rust async","tags":["rust"],"limit":3,"detail":"metadata"}' \
  2>/dev/null | jq '[.results[].tags | index("rust") != null] | all'

# Lint rule histogram
sb cortex --vault ~/repos/scottidler/obsidian lint --format json 2>/dev/null \
  | jq '[.[].rule] | group_by(.) | map({rule: .[0], n: length}) | sort_by(-.n)'

# Vault-wide schema snapshot
sb oracle call vault_overview 2>/dev/null | jq '{total_notes, distinct_tags, schema_gaps}'

# Dump every MCP tool schema and audit it
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | timeout 60 sb oracle serve 2>/dev/null | grep '"id":2' | jq '.result.tools | length'
```

## Edge cases

| Input | Exit | Behavior |
|---|---|---|
| `sb oracle call` (no tool) | 2 | clap usage, no crash |
| `sb oracle call no_such_tool` | 1 | `Error: unknown tool: no_such_tool (use oracle call --list)` |
| `sb oracle call tag_search --json '{bad json'` | 1 | `key must be a string at line 1 column 2` |
| `sb oracle call tag_brief` (missing required `tag`) | 1 | typed error |
| `tag_search --json '{"tag":"no-such-tag-xyz"}'` | 0 | `count: 0`, empty results |
| `list_notes --json '{"domain":"tech"}'` | 0 | **silently ignored** (finding 4) |

## Release validation

| Check | Result |
|---|---|
| Tag `v0.15.1` | exists, **annotated** (`git cat-file -t` -> `tag`), points at `b57370f` |
| GitHub release | published, not a draft, `2026-09-21T04:40:06Z` |
| Assets | `sb-v0.15.1-linux-amd64.tar.gz` + `.sha256`. No arm64, no darwin. |
| Checksum | `sb-v0.15.1-linux-amd64.tar.gz: OK` |
| Downloaded binary | `sb v0.15.1`, matches the locally installed build |

This phase downloaded and executed a release binary from the repo's own GitHub
releases, after verifying it against the published checksum.

## Skipped

**Mutating (discovered, not run):** `borg {note, reingest, reingest-failed,
replay, retention, backfill-ingested, dedupe-sessions, daemon *, extension
{sign,install,uninstall}, blocklist {add,remove}}`, `cortex {classify --apply,
lint --apply, link, unlink, intel, migrate --apply, sweep --migrate, embed,
graph, associate --apply, hub, entities, concept-promote, bridge-*}`, `oracle
{index --force, serve}` beyond the schema probe, `bootstrap`.

**Blocked:** `sb borg ingest --help` is refused by a local PreToolUse hook
("bulk vault ingest is Scott's call"). Its help text was read from the offline
help-tree dump instead.

## Observations

- `sb cortex classify`'s `--apply`, `--path`, `--force`, `--review-only` have no
  help text. Already listed as open in Addendum C3.
- `sb cortex lint`'s `--apply`, `--rule`, `--path` likewise, and
  `sb cortex sweep`'s `--migrate`, `--dry-run`, `--proposals`, `--cold`.
- Shell caveat, not an `sb` defect: when a command is prefixed with
  `cd <repo> && ...`, bare words that match a file or directory in that CWD are
  rewritten to absolute paths, so `sb borg harvest` became
  `sb /path/to/borg harvest`. Verified with `echo vault` / `echo nosuchdirname`.
  Run `sb` without a `cd` prefix, or quote the subcommand.
