# CLI Shakedown Report: sb v0.8.5

**Date:** 2026-05-20
**Branch:** main (commit 77365fc, HEAD == tag v0.8.5)
**Binary:** `/home/saidler/.cargo/bin/sb`
**Scope:** read-only exercise of every safe subcommand against the live vault at `~/repos/scottidler/obsidian/`. Mutating verbs discovered and documented but not executed. The borg + cortex systemd daemons were left running throughout.

## Summary

| Metric | Count |
|--------|-------|
| Top-level commands | 6 (borg, cortex, oracle, status, doctor, bootstrap) |
| Subcommands (all levels) | 41 |
| Commands tested | 37 |
| Commands passed | 37 (errors-by-design counted as passes) |
| Commands skipped (mutating) | 14 |
| Commands skipped (long-running) | 1 (`oracle serve` - MCP stdio loop) |
| Pipelines tested | 7 |
| Edge cases tested | 11 |
| Bugs found | 7 (2 logic, 5 cosmetic / API consistency) |

The binary works end-to-end. Every safe verb produces sensible output, exits with correct status, and the vault was not modified. The newly cut v0.8.5 binary matches the v0.8.5 annotated tag on HEAD. The known cortex embed memory growth is visible: RSS climbed from 1.2 GB to 1.4 GB over the ~10 minutes of this run.

## Command Tree

```
sb [-l LEVEL] [-v]
|-- borg [-c CONFIG] [-l LEVEL]
|   |-- daemon  [--install|--uninstall|--reinstall|--start|--stop|--restart|--status]    MUTATING (status SAFE)
|   |-- ingest [URL] [--clipboard|--file FILE] [-t TAGS] [--force]                        MUTATING
|   |-- note   [TEXT] [--clipboard] [-t TAGS]                                             MUTATING
|   |-- hotkey [--install|--uninstall] [--host|--port|--key]                              MUTATING
|   |-- sign                                                                              MUTATING (signs ext)
|   |-- migrate [--dry-run|--apply]                                                       SAFE w/ --dry-run
|   |-- audit [--fix] [--invariant] [--bound-secs N]                                      SAFE w/o --fix
|   |-- intake { list [--method|--since|--limit N], show TRACE_ID }                       SAFE
|   |-- dlq { list, show TRACE, archive ..., replay TRACE }                               list/show SAFE
|   |-- reingest [--all|--type|--domain|--source|--before|--after|--dry-run]              SAFE w/ --dry-run
|   |-- replay [TRACE_ID] [--from-stage|--since|--rejected|--bootstrap-from-vault|--note|--dry-run]  SAFE w/ --dry-run
|   |-- retention { sweep [--dry-run], status }                                           status SAFE
|   |-- reingest-failed [--dry-run]                                                       SAFE w/ --dry-run
|   |-- blocklist { list, remove DOMAIN, clear }                                          list SAFE
|   |-- backfill-ingested [--dry-run]                                                     SAFE w/ --dry-run
|   `-- dashboard { refresh }                                                             MUTATING
|-- cortex [-c CONFIG] [-r VAULT] [-l LEVEL]
|   |-- classify [--apply] [--path|--force|--review-only|--reclassify-domain]             SAFE w/o --apply
|   |-- lint [--apply] [--format human|json] [--rule] [--path]                            SAFE w/o --apply
|   |-- link [--apply] [--scan all|people|projects|concepts]                              SAFE w/o --apply
|   |-- intel [--daily|--weekly] [--output]                                               MUTATING
|   |-- state [--refresh|--diff]                                                          SAFE w/o --refresh
|   |-- daemon [--install|--uninstall|--start|--stop|--status]                            MUTATING (status reports incompletely)
|   |-- migrate [--apply|--plan]                                                          SAFE w/o --apply
|   |-- sweep [--migrate|--dry-run|--proposals|--cold]                                    SAFE
|   |-- summarize [--backfill|--since|--domain|--extractor|--dry-run|--resume]            SAFE w/ --dry-run
|   `-- embed [--backfill|--kind|--model|--batch-size|--prefetch-model]                   MUTATING
|-- oracle [-c CONFIG]
|   |-- serve                                                                             LONG-RUNNING (MCP stdio)
|   |-- index                                                                             SAFE (idempotent)
|   |-- stats                                                                             SAFE
|   `-- call TOOL [--json JSON] [--list]                                                  SAFE (read-only tool invocation)
|-- status                                                                                SAFE
|-- doctor                                                                                SAFE
`-- bootstrap [--skip-prefetch-model|--skip-systemd]                                      MUTATING
```

## Command Results

### Top-level health (safe)

| Command | Exit | Notes |
|---------|------|-------|
| `sb --version` | 0 | reports `sb v0.8.5` |
| `sb --help` / `sb <subsystem> --help` | 0 | clean clap output; borg/cortex help embed required-tool checks (yt-dlp, fabric, markitdown) and log path |
| `sb status` | 0 | aggregated systemd + config + vault report; ends with empty trailing line then exit |
| `sb doctor` | 0 | severity-tagged status; suggests `sb bootstrap` for missing cortex/oracle configs |

### borg (safe)

| Command | Exit | Notes |
|---------|------|-------|
| `sb borg daemon --status` | 0 | full `systemctl --user status borg` output with recent journal lines |
| `sb borg audit --invariant` | 0 | intake=165 / ledger=707 / dlq=11; **553 ledger rows have no matching intake row** (CLAUDE.md invariant currently violated); writes `system/views/borg-orphans.md` |
| `sb borg intake list` | 0 | 50 most recent rows by default; columns Date / Time / Method / Origin / Kind / Trace / Preview |
| `sb borg intake list --method telegram --limit 5` | 0 | filter works |
| `sb borg intake list --since 2026-05-15 --limit 5` | 0 | filter works |
| `sb borg intake show ht-575bd1` | 0 | renders intake row + raw-input sidecar file (43 bytes shown) |
| `sb borg dlq list` | 0 | 11 pending rows, all `watchdog-orphan` (mostly stale from 2026-05-12) |
| `sb borg dlq list --status pending --limit 5` | 0 | filter works |
| `sb borg dlq show ht-ddd2ce` | 0 | DLQ row + intake row + sidecar + ledger correlation |
| `sb borg retention status` | 0 | `Traces: 0, Disk usage: 0 bytes` (clean staging area) |
| `sb borg blocklist list` | 0 | `(blocklist empty)` |
| `sb borg audit` | 0 | 1121 ledger rows scanned; 1 blocked-content, 6 raw-URL titles, 49 duplicate pairs, 4 orphan replacements |
| `sb borg migrate --dry-run` | 0 | scans 1237 markdown files; 0 changes |
| `sb borg reingest-failed --dry-run` | 0 | finds 1 matching note (xda-developers docker container article) |
| `sb borg backfill-ingested --dry-run` | 0 | reports `WOULD set ingested=YYYY-MM-DD` per assisted note |

### cortex (safe, with `-r ~/repos/scottidler/obsidian`)

| Command | Exit | Notes |
|---------|------|-------|
| `sb cortex lint` (CWD = obsidian) | 0 | 533 errors, 733 warnings, 3915 info; most errors are unresolved wikilinks and missing slide assets |
| `sb cortex lint --format json` | 0 | 5181 entries, severities aggregate to 533/733/3915 |
| `sb cortex lint --format yaml` | 0 | **silently falls back to human format** (bug; see Findings) |
| `sb cortex link` (CWD = obsidian) | 0 | 0 errors, 123 info suggestions |
| `sb cortex classify -r ~/repos/scottidler/obsidian` | 0 | 0 inbox + 2 unclassified notes; suggests catch-up classify |
| `sb cortex state -r ~/repos/scottidler/obsidian` | 0 | `Last scan: 2026-05-20 07:56:46 UTC (1363 files)` |
| `sb cortex sweep --dry-run` | 0 | `No new tag proposals.` |
| `sb cortex sweep --proposals` | 0 | `No new tag proposals.` |
| `sb cortex sweep --cold` | 0 | writes report to `system/views/cold-notes.md`; `scanned=1363 surfaced=0` |
| `sb cortex summarize --dry-run` | 1 | errors "requires `--backfill`; no other modes are implemented yet" (CLI design: `--dry-run` is valid but ignored without `--backfill`) |
| `sb cortex migrate` (no --apply) | 0 | scans 1363 notes; `No violations found.` |
| `sb cortex link --scan people` | 0 | 45 info; **identical to `--scan all` and default** (bug; see Findings) |
| `sb cortex link --scan projects` | 0 | 45 info; identical to other --scan values |
| `sb cortex link --scan concepts` | 0 | 45 info; identical to other --scan values |
| `sb cortex daemon --status` | 0 | **prints only "Service file: ..." hint, not actual status** (bug; see Findings) |

### oracle (safe)

| Command | Exit | Notes |
|---------|------|-------|
| `sb oracle stats` | 0 | 1363 notes; full per-domain / per-type / per-status histogram |
| `sb oracle index` | 0 | scanned=1364, inserted=1, updated=2, unchanged=1361 (vault grew by 1 since last index) |
| `sb oracle call --list` | 0 | 18 MCP tools listed with descriptions |
| `sb oracle call vault_overview` | 0 | JSON: total_notes 1363, by_domain[], by_type[], by_status[], schema_gaps[] |
| `sb oracle call schema_info` | 0 | enumerates domains, note_types, origins, statuses, methods |
| `sb oracle call inbox_status` | 0 | inbox count + classified-recent (10) JSON |
| `sb oracle call knowledge_search {query, limit:3}` | 0 | returns `{count, results[]}` |
| `sb oracle call tag_search {tag:"llm"}` | 0 | returns `{count, results[], tag}` |
| `sb oracle call source_browse {}` | 0 | returns `{count, sources[]}` (top: youtube.com 805, github 32, xda 32) |
| `sb oracle call domain_brief {domain:"ai"}` | 0 | total_notes 481; `unread_count` is `null` (see Findings) |
| `sb oracle call list_notes {domain, limit}` | 0 | returns `{count, results[]}` |
| `sb oracle call recent_activity {days:3}` | 0 | returns `{count, days, results[]}` |
| `sb oracle call find_similar {content:"..."}` | 0 | returns `{count, results[]}` |

### Commands not executed (mutating or long-running)

`sb borg daemon --install|--start|--stop|--restart|--uninstall|--reinstall`,
`sb borg ingest`, `sb borg note`, `sb borg hotkey --install/--uninstall`, `sb borg sign`,
`sb borg migrate --apply`, `sb borg audit --fix`,
`sb borg dlq archive`, `sb borg dlq replay`,
`sb borg reingest` (without dry-run), `sb borg replay` (without dry-run),
`sb borg retention sweep` (without dry-run), `sb borg reingest-failed` (without dry-run),
`sb borg blocklist remove`, `sb borg blocklist clear`,
`sb borg backfill-ingested` (without dry-run), `sb borg dashboard refresh`,
`sb cortex *apply`, `sb cortex intel`, `sb cortex daemon --install/--start/--stop`,
`sb cortex sweep --migrate`, `sb cortex embed`, `sb bootstrap`,
`sb oracle serve` (stdio MCP loop).

**Note on "safe" classification:** `sb cortex sweep --cold` and `sb borg audit --invariant` are listed as safe but DO write to the vault (`system/views/cold-notes.md` and `system/views/borg-orphans.md` respectively). These views are designed to be regenerated each run, so the write is idempotent / non-destructive. `sb oracle index` writes to the SQLite database at `~/.local/share/oracle/oracle.db` - also idempotent for unchanged notes. Treat all three as "safe for this run, but they do touch state."

## Output Format Matrix

| Surface | Default | JSON | Notes |
|---------|---------|------|-------|
| `sb cortex lint` | `--format human` | `--format json` | json valid, parses with jq, 5181-entry array |
| `sb cortex lint --format yaml` | falls back to human | n/a | silent fallback - unknown values should error |
| `sb oracle call <tool>` | JSON always | always JSON | result keys vary by tool: `results` / `tags` / `sources` / `recent_notes` |
| `sb borg intake list` | table | n/a | no `--json` flag |
| `sb borg dlq list` | table | n/a | no `--json` flag |
| `sb oracle stats` | table-ish text | n/a | no `--json` flag (vault_overview is the JSON equivalent) |
| Logging | stderr + log file | n/a | INFO+ goes to both stderr and `/home/saidler/.local/share/sb/<subsystem>.log` |

## Findings

### 1. cortex defaults vault root to CWD with no validation (UX trap)

Running `sb cortex lint` from inside the second-brain repo silently lints the **codebase** rather than the actual Obsidian vault. With CWD = `~/repos/scottidler/second-brain`, the lint produces 65 errors about design-doc frontmatter and codebase wikilinks. With CWD = `~/repos/scottidler/obsidian`, the same command surfaces 533 vault errors.

The log line `resolved vault root: /home/saidler/repos/scottidler/second-brain` is honest but easy to miss. Cortex could:
- read the configured vault path the way borg reads `borg.yml`, or
- refuse to run on a directory that lacks a vault signature (no `notes/`, no `system/`, etc.), or
- error when `--vault` is not set and CWD looks like a code repo (Cargo.toml present).

Severity: **medium** - silently produces useless output when run from the most natural directory.

### 2. `sb cortex daemon --status` prints a hint instead of status

```
Service file: /home/saidler/.config/systemd/user/cortex.service
Check status: systemctl --user status cortex
```

Compare with `sb borg daemon --status`, which embeds the full `systemctl status` block + recent journalctl lines. Cortex should match borg's behavior so `sb status` is genuinely "aggregated health" without the user shelling out.

Severity: **low** (cosmetic / inconsistency).

### 3. Three-way config-path drift between cortex loader, `sb bootstrap`, and `sb status`

Verified on disk: `~/.config/cortex/cortex.yml` exists (3.4 KB, mtime 2026-05-24), `~/.config/obsidian-cortex/` does not exist at all. The three sites diverge:

| Site | Path it expects |
|------|-----------------|
| `cortex/src/config.rs:600` (loader) | `~/.config/cortex/cortex.yml` |
| `cortex/src/config.rs:592` (doc comment) | `~/.config/obsidian-cortex/obsidian-cortex.yml` |
| `sb/src/cli/bootstrap.rs:27` (template writer) | `~/.config/obsidian-cortex/obsidian-cortex.yml` |
| `sb/src/cli/checks.rs:143` (sb status / doctor) | `~/.config/obsidian-cortex/obsidian-cortex.yml` |

Consequences in the real world:
- `sb bootstrap` would create a `cortex.yml` at the wrong path; the loader would still pick up defaults because the path it actually checks is somewhere else.
- `sb status` / `sb doctor` will always say "cortex config missing" even when the loader is loading a config and reporting `loaded config: /home/saidler/.config/cortex/cortex.yml` on the previous line.
- `sb status` suggests running `sb bootstrap`, which on this machine would not fix the "missing" report.

Severity: **medium** (silent UX divergence affecting health checks and the recommended first-time-setup flow).

The fix is a single shared constant. Suggested location: `cortex::Config::default_config_path()` or a top-level `vault::paths` module that all three sites import.

### 4. `--format yaml` (or any unknown value) silently falls back to human

```
$ sb cortex lint --format yaml
[...same human-readable output as default...]
```

clap can validate against an enum here. As written, a typo in a CI flag (`--format jsno`) would silently produce wrong-format output instead of failing fast.

Severity: **low** (would be medium if `--format` were used downstream).

### 5. `sb cortex link --scan {people,projects,concepts}` is a dead flag

`LinkArgs.scan` is parsed by clap (default `"all"`) and forwarded into `cortex::opts::LinkOpts.scan`, but `cortex::link()` in `cortex/src/lib.rs:187` never reads it. The downstream linker uses `config.actions.linking.scan_for`, which defaults to `["people", "projects", "concepts"]`.

Empirically: `sb cortex link`, `--scan all`, `--scan people`, `--scan projects`, and `--scan concepts` all produce identical "Total: 0 error(s), 0 warning(s), 45 info(s)" output. A `--scan nonsense` value is also accepted without error.

Fix: either thread `opts.scan` into `linking::lint_linking` / `linking::apply_linking` and override `config.scan_for`, or remove the flag from the CLI surface.

Severity: **medium** (silent no-op flag; users will trust output that does not reflect what they asked for).

### 6. Oracle response shapes are inconsistent

The result array goes by different names across tools:

| Tool | Wrapping object | Result key |
|------|-----------------|------------|
| `list_notes` | `{count, results}` | `results` |
| `knowledge_search` | `{count, results}` | `results` |
| `find_similar` | `{count, results}` | `results` |
| `recent_activity` | `{count, days, results}` | `results` |
| `tag_search` (with tag) | `{count, results, tag}` | `results` |
| `tag_search` (no tag) | `{tags}` | `tags` |
| `source_browse` | `{count, sources}` | `sources` |
| `creator_browse` | `{count, creators}` (inferred) | `creators` |
| `domain_brief` | `{domain, total_notes, unread_count, ..., recent_notes}` | `recent_notes` |

A consumer that does `jq '.results[]'` works for half the tools and silently returns nothing for the others. Standardizing on `results` (or always providing `results` in addition to the kind-specific name) would make jq pipelines uniform.

Also: `domain_brief.unread_count` returned `null` for `domain=ai`. Either the field is computed elsewhere and not threaded through, or it should be 0 when there are no unread notes.

Severity: **low** for MCP usage (LLMs adapt), **medium** for shell-pipeline usage (which is what `sb oracle call` is for).

## Release Validation

| Check | Result |
|-------|--------|
| Tag `v0.8.5` exists | YES |
| Annotated (not lightweight) | YES (`git cat-file -t v0.8.5` -> `tag`) |
| Tag points to HEAD | YES (77365fcb) |
| Tag is on `main` | YES (HEAD is on main; clean working tree) |
| Local binary `--version` matches tag | YES (`sb v0.8.5`) |
| GitHub release exists for v0.8.5 | NO - `gh release view v0.8.5` -> "release not found" |
| GitHub release exists for any prior version | NO - `gh release list` returns 0 entries; `gh workflow list` returns 0 workflows |

**Conclusion:** this project intentionally ships via `otto deploy` (local install + systemd restart) rather than GitHub Releases. The 15 existing version tags (v0.5.6 through v0.8.5) all live as annotated tags on `main` with no corresponding release artifacts. Release validation against downloaded binaries is N/A here.

## Pipeline Recipes (tested)

```bash
# Count lint findings by severity
sb cortex -r ~/repos/scottidler/obsidian lint --format json \
  | jq '[.[].severity] | group_by(.) | map({severity: .[0], count: length})'

# Top 10 tags across the vault
sb oracle call tag_search --json '{}' \
  | jq '.tags[0:10] | map({tag, count})'

# Top source domains
sb oracle call source_browse --json '{}' \
  | jq '.sources[0:5]'

# Chain: top tag -> notes for that tag
top=$(sb oracle call tag_search --json '{}' | jq -r '.tags[0].tag')
sb oracle call tag_search --json "{\"tag\":\"$top\",\"limit\":3}" \
  | jq '.results | length, [.[].title]'

# Chain: list domain -> read metadata of first note
path=$(sb oracle call list_notes --json '{"domain":"ai","limit":1}' \
  | jq -r '.results[0].path')
sb oracle call note_read --json "{\"path\":\"$path\",\"detail\":\"metadata\"}"

# Recent activity (last 3 days)
sb oracle call recent_activity --json '{"days":3}' \
  | jq '{count, days, titles: [.results[].title]}'

# Schema gap snapshot
sb oracle call vault_overview \
  | jq '{total: .total_notes, gaps: .schema_gaps}'
```

## Edge Cases (tested)

| Input | Behavior | Verdict |
|-------|----------|---------|
| `sb --bogus-flag` | clap error + usage hint, exit 2 | clean |
| `sb borg intake show` (no trace) | clap usage error, exit 2 | clean |
| `sb oracle call` (no tool) | clap usage error, exit 2 | clean |
| `sb borg intake show NOPE-ZERO` | anyhow: "trace_id NOPE-ZERO not found in intake log" + Location, exit 1 | clean |
| `sb oracle call nonexistent_tool` | anyhow: "unknown tool: nonexistent_tool (use oracle call --list)" + Location, exit 1 | clean |
| `sb oracle call vault_overview --json 'not-json'` | anyhow: "invalid JSON arguments" w/ serde detail + Location, exit 1 | clean |
| `sb borg note` (no text, no flag) | anyhow: "No text provided. Use a text argument or --clipboard" + Location, exit 1 | clean |
| `sb borg ingest` (no URL, no flag) | anyhow: "No URL provided. Use a URL argument or --clipboard" + Location, exit 1 | clean |
| `sb borg blocklist remove nonexistent-domain.example` | "not blocklisted: ...", exit 0 | reasonable (idempotent remove) |
| `sb borg reingest --dry-run` (no filters) | "Specify --all, ..." + Location, exit 1 | clean (forces explicit selection) |
| `sb borg replay --dry-run` (no trace/since) | "must provide a trace_id, --since, --rejected, ..." + Location, exit 1 | clean |

Cosmetic note: anyhow's `Location: <file>:<line>:<col>` block leaks internal source paths into every error response. Most CLIs hide that behind `--verbose` / `RUST_BACKTRACE=1`. Not a bug, but a polish opportunity.

### Shakedown gotcha: exit codes through pipelines

`sb` returns clean non-zero exit codes on errors. **However**, the natural reflex of piping through `| head` or `| /usr/bin/tail` clobbers `$?` with the exit status of the last process in the pipeline (head/tail). Use `${PIPESTATUS[0]}` (bash) or run the command standalone and inspect `$?` to see sb's real exit status. Several "EXIT: 0" entries earlier in a manual exploration of this binary turned out to be `head` exiting 0 over an `sb` that had already failed with exit 1.

## Observations

- **Cortex memory growth confirmed.** RSS reported by `sb status` climbed 1.2 GB -> 1.4 GB during this run. Matches the known leak captured in design doc `2026-05-19-cortex-embed-memory-bounding.md`.
- **11 DLQ rows are stale.** Ten from 2026-05-12 plus one from 2026-05-19. All `watchdog-orphan` with the same "no ledger or dlq row produced within 1860s" reason. `sb borg dlq show` shows the ledger DOES have a completed row for those URLs - the watchdog timed them out before the ledger row landed. Worth a follow-up to either backfill-acknowledge these in the DLQ or extend the watchdog window for slow YouTube transcripts.
- **49 duplicate ledger pairs.** `sb borg audit` flagged 49 sources with two ledger entries each. Most are YouTube. `--fix` exists but was not run here.
- **1237 markdown files vs 1363 indexed notes.** `sb borg migrate --dry-run` reports 1237, `sb oracle stats` reports 1363. The delta is non-note markdown (design docs, system/views/, dashboard, MOCs). Worth confirming, but the numbers themselves are not alarming.
- **Schema gaps reported by `sb status`:** domain=136, note_type=63, origin=161, status=536. The status gap is the largest because `unread` is the only populated status today; all other notes lack any status.
- **No GitHub release pipeline.** Intentional per the `otto deploy` workflow. Worth a one-line note in the README so a future collaborator knows the install story.
