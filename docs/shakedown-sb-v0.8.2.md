# CLI Shakedown Report: sb v0.8.2-7-gfb73a72

**Date:** 2026-05-19
**Branch:** unified-sb-binary
**Binary:** /home/saidler/.cargo/bin/sb (cargo install --path sb)
**Scope:** read-only exercise of every safe subcommand against the live vault at ~/repos/scottidler/obsidian/. Mutating verbs discovered and documented but not executed.

## Summary

| Metric | Count |
|--------|-------|
| Top-level commands | 6 (borg, cortex, oracle, status, doctor, bootstrap) |
| Subcommands (all levels) | 41 |
| Commands tested | 21 |
| Commands passed | 21 |
| Commands failed | 0 |
| Commands skipped (mutating) | 18 |
| Commands skipped (long-running) | 1 (oracle serve - MCP stdio loop) |
| Pipelines tested | 6 |
| Edge cases tested | 6 |
| Bugs found | 4 (1 cosmetic, 3 minor UX) |

The binary works end-to-end. All read-only verbs return sensible output. Error handling is clean (informative messages, non-zero exit codes). The vault was not modified.

## Command Tree

```
sb [-l LEVEL] [-v]
├── borg [-c CONFIG] [-l LEVEL]
│   ├── daemon  [--install|--uninstall|--reinstall|--start|--stop|--restart|--status]   MUTATING
│   ├── ingest [URL] [--clipboard|--file FILE] [-t TAGS] [--force]                       MUTATING (hits running daemon)
│   ├── note   [TEXT] [--clipboard] [-t TAGS]                                            MUTATING
│   ├── hotkey [--install|--uninstall] [--host|--port|--key]                             MUTATING
│   ├── sign                                                                              MUTATING (signs ext)
│   ├── migrate [--dry-run|--apply]                                                      MUTATING w/ --apply
│   ├── audit [--fix] [--invariant] [--bound-secs N]                                     SAFE w/o --fix
│   ├── intake { list [--method|--since|--limit N], show TRACE_ID }                      SAFE
│   ├── dlq { list, show TRACE, archive ..., replay TRACE }                              list/show SAFE; archive/replay MUTATING
│   ├── reingest [--all|--type|--domain|--source|--before|--after|--dry-run]             SAFE w/ --dry-run
│   ├── replay [TRACE_ID] [--from-stage|--since|--rejected|--bootstrap-from-vault|--note|--dry-run]  SAFE w/ --dry-run
│   ├── retention { sweep [--dry-run], status }                                          status SAFE; sweep MUTATING w/o --dry-run
│   ├── reingest-failed [--dry-run]                                                      SAFE w/ --dry-run
│   ├── blocklist { list, remove DOMAIN, clear }                                         list SAFE; rest MUTATING
│   ├── backfill-ingested [--dry-run]                                                    SAFE w/ --dry-run
│   └── dashboard { refresh }                                                            MUTATING
├── cortex [-c CONFIG] [-r/--vault] [-l LEVEL]
│   ├── classify [--apply] [--path|--force|--review-only|--reclassify-domain]            SAFE w/o --apply
│   ├── lint [--apply] [--format human|json] [--rule] [--path]                           SAFE w/o --apply
│   ├── link [--apply] [--scan all|people|projects|concepts]                             SAFE w/o --apply
│   ├── intel [--daily|--weekly] [--output]                                              MUTATING
│   ├── state [--refresh|--diff]                                                         SAFE w/o --refresh
│   ├── daemon [--install|--uninstall|--start|--stop|--status]                           MUTATING
│   ├── migrate [--apply|--plan]                                                         SAFE w/o --apply
│   ├── sweep [--migrate|--dry-run|--proposals|--cold]                                   SAFE (proposals/cold write report files); --migrate MUTATING
│   ├── summarize [--backfill|--since|--domain|--extractor|--dry-run|--resume BOOL]      SAFE w/ --dry-run
│   └── embed [--backfill|--kind|--model|--batch-size|--prefetch-model]                  MUTATING
├── oracle [-c CONFIG]
│   ├── serve                                                                            LONG-RUNNING (MCP stdio)
│   ├── index                                                                            idempotent (safe)
│   ├── stats                                                                            SAFE
│   └── call [TOOL] [--json] [--list]                                                    SAFE for read tools
├── status                                                                                SAFE
├── doctor                                                                                SAFE
└── bootstrap [--skip-prefetch-model]                                                     IDEMPOTENT
```

## Tested commands

### sb status

```
$ sb status
[systemd]
  ✅ borg.service: active (PID 1062215, RSS 89.9 MB)
  ✅ cortex.service: active (PID 1062254, RSS 1.2 GB)

[config]
  ✅ borg: /home/saidler/.config/borg/borg.yml
  ⚠️  cortex: missing (/home/saidler/.config/obsidian-cortex/obsidian-cortex.yml)
  ⚠️  oracle: missing (/home/saidler/.config/oracle/oracle.yml)

[patterns]
  ✅ 14 patterns in sync

[embedding]
  ⚠️  fastembed cache missing
```

Exit 0. Reports live state from systemctl + filesystem.

### sb doctor

Same checks as status, sorted by severity (errors first → ok last), with `-> <suggested fix>` lines under warnings. Exit 0 even when warnings present (correct: doctor reports, doesn't gate).

### sb borg audit (no --fix)

```
$ sb borg audit | /usr/bin/tail -5
  [DUPLICATE] https://www.youtube.com/watch?v=8jKAT8GNDE0 -> 2 notes found
  [DUPLICATE] https://www.youtube.com/watch?v=jYMhDEzNAN0 -> 2 notes found
  [DUPLICATE] https://www.youtube.com/watch?v=U1oHRqUkI1E -> 2 notes found
  [DUPLICATE] https://www.xda-developers.com/i-tried-these-docker-containers-and-now-i-cant-live-without-them/ -> 2 notes found
  [DUPLICATE] youtube-transcript -> 8 notes found
```

Exit 0. Walked vault, surfaced ~30 duplicate-source notes (real data finding, not a sb bug).

### sb borg intake list / show (chained)

```
$ sb borg intake list --limit 5
Date        Time  Method    Origin        Kind      Trace      Preview
2026-05-19  15:07 http      http          url       ht-9fd57e https://www.youtube.com/watch?v=HQEm4rBKdec
2026-05-19  10:52 cli       http          url       cl-8d4862 https://github.com/matt1398/claude-devtools
2026-05-19  10:52 cli       http          url       cl-6a3643 https://github.com/coleam00/archon
2026-05-19  07:24 cli       http          url       cl-a1849f https://github.com/matt1398/claude-devtools
2026-05-19  07:24 cli       http          url       cl-62e3a1 https://github.com/coleam00/archon

$ sb borg intake show ht-9fd57e
Intake row:
  date: 2026-05-19
  time: 15:07
  method: http
  origin: http
  kind: url
  preview: https://www.youtube.com/watch?v=HQEm4rBKdec
  trace: ht-9fd57e

--- sidecar /home/saidler/repos/scottidler/obsidian/system/intake/ht-9fd57e.txt (43 bytes) ---
https://www.youtube.com/watch?v=HQEm4rBKdec
```

Output-of-list → input-of-show chain works. Sidecar resolution works.

### sb borg dlq list / show

`dlq list --limit 5` returns 5 watchdog-orphan rows. `dlq show <trace>` resolves the row + its intake row + sidecar + reconciliation note ("ledger has a completed row for this source"). All clean.

### sb borg blocklist list

`(blocklist empty)` — clean empty-state message.

### sb borg retention status

```
Staging root: ~/.local/share/borg/stages
Traces:       0
Rejected:     0
Disk usage:   0 bytes
```

Clean key-value report.

### sb borg reingest --dry-run

With filters `--all --type youtube --before 2026-05-01`: enumerated 797 candidate notes for dry-run reingestion. No actual reingest invoked.

### sb borg reingest-failed --dry-run

Detected 2 notes matching the failed-fetch signature; no rewrites.

### sb borg replay --dry-run --since 1d

`replay: no traces matched` — empty result.

### sb borg backfill-ingested --dry-run

Scanned 1236 notes, identified 115 that would receive `ingested: <date>`, 476 already had the field, 645 skipped (origin != assisted). Detailed summary at the end.

### sb cortex lint --rule frontmatter

369 errors, 679 warnings, 0 info. Surfaced real schema gaps (missing required `tags`, missing required `domain`, missing required `origin`). Exit 0 (lint reports, doesn't gate).

### sb cortex lint --format json --rule tags

3833 violation objects. Passed `jq` validation; each entry has `path`, `rule`, `severity`, `message`.

### sb cortex state, state --diff

`state` reports last scan timestamp and file count. `state --diff` enumerated 4 added + 22 modified files since the last cached manifest.

### sb cortex link

44 info-level "could be linked as [[X]]" suggestions across system/design-vault-reorganization.md. Dry-run by default.

### sb cortex migrate

`No violations found.` — vault already on current schema.

### sb cortex classify

Found 0 inbox notes + 2 unclassified domain-less notes. Would catch-up classify both via LLM. Dry-run by default.

### sb cortex summarize --backfill --dry-run --since 7d

Walked notes, listed 32 candidates that would be re-distilled. Detailed kind-aware ("kind=Some(\"video\")", "kind=Some(\"article\")").

### sb oracle stats

Standard vault stats — domain/type/status breakdowns, schema gaps.

### sb oracle index

Idempotent reindex against ~/repos/scottidler/obsidian/. Detected 0 inserted, 0 updated, 1362 unchanged.

### sb oracle call --list

18 tools discovered (knowledge_search, vault_overview, schema_info, domain_brief, tag_search, find_links, find_similar, note_read, list_notes, recent_activity, source_browse, creator_browse, ingest_history, classify_status, inbox_status, quality_report, duplicate_groups, reindex).

### sb oracle call vault_overview / knowledge_search / domain_brief / schema_info / tag_search / find_links / note_read / recent_activity

All return well-formed JSON. The candle BERT model (`BAAI/bge-small-en-v1.5`) loaded on first `knowledge_search` call (~1s); subsequent calls reused the loaded model.

## Output format matrix

| Command | Default | --format json | --format csv | --json (oracle) |
|---|---|---|---|---|
| `cortex lint` | human report | ✅ valid JSON array | n/a | n/a |
| `cortex link` | human report | n/a | n/a | n/a |
| `oracle call <tool>` | n/a | ✅ valid JSON (object or shape-per-tool) | n/a | ✅ uses `--json` flag for ARGUMENTS, not output format. Output is always JSON. |
| `borg intake list` | aligned table | n/a (no --format) | n/a | n/a |
| `borg dlq list` | aligned table | n/a | n/a | n/a |

Observation: only `cortex lint` carries an explicit `--format` flag. Other commands either always output JSON (oracle), always output human text (borg listings), or always output a fixed report shape (cortex/state/migrate). Reasonable for current scope; document if you add `--json` to more commands later.

## Pipeline recipes

All copy-pasteable. Each was tested with the output shown.

```bash
# Top 5 tags by usage count
sb oracle call tag_search --json '{"limit":50}' \
  | jq -r '.tags | sort_by(-.count) | .[:5] | .[] | "\(.count)\t\(.tag)"'
# 394    llm
# 252    claude
# 231    agents
# 158    automation
# 116    offense

# Paths of top 5 search hits for "docker"
sb oracle call knowledge_search --json '{"query":"docker","limit":5}' \
  | jq -r '.results[].path'

# All lint violations as JSON, filtered to errors only
sb cortex lint --format json --rule frontmatter \
  | jq '[.[] | select(.severity == "Error")] | length'

# Recent-activity timeline (last 3 days), paths only
sb oracle call recent_activity --json '{"days":3}' \
  | jq -r '.results[].path' | head -10

# Top domains in the vault, sorted by note count
sb oracle call vault_overview \
  | jq '.by_domain | to_entries | sort_by(-.value[1]) | .[:5]'

# Chain: search -> read metadata of top hit
sb oracle call knowledge_search --json '{"query":"rust","limit":1}' \
  | jq -r '.results[0].path' > /tmp/path.txt
sb oracle call note_read --json "{\"path\":\"$(cat /tmp/path.txt)\",\"detail\":\"metadata\"}" \
  | jq '{path, title, domain, type, tags}'
```

## Edge cases

| Case | Behavior | Exit |
|---|---|---|
| `sb borg intake show bogus-trace-id` | `Error: trace_id bogus-trace-id not found in intake log` + source location | 1 |
| `sb oracle call` (missing TOOL) | clap usage error | 2 |
| `sb oracle call nonexistent_tool` | `Error: unknown tool: nonexistent_tool (use oracle call --list)` | 1 |
| `sb borg ingest` (no URL, no --clipboard, no --file) | `Error: No URL provided. Use a URL argument or --clipboard` | 1 |
| `sb borg invalid-subcommand` | clap unrecognized-subcommand error | 2 |
| `sb cortex` (missing required subcommand) | clap missing-subcommand error + help | 2 |

All errors are informative and use appropriate exit codes (1 for runtime/data errors, 2 for clap arg errors).

## Findings / bugs

### 1. `sb borg` (no subcommand) prints ROOT help, not borg help (cosmetic)

```
$ sb borg
... [borg INFO log lines] ...
second-brain unified CLI: borg + cortex + oracle
Usage: sb [OPTIONS] <COMMAND>
Commands:
  borg       ...
  cortex     ...
  ...
```

`sb/src/cli/borg.rs:284` calls `crate::cli::Cli::command().print_help()` instead of `crate::cli::Cli::command().find_subcommand("borg").unwrap().print_help()` (or constructing borg's clap tree directly). Result: user runs `sb borg`, gets the top-level help showing all subsystems, not borg's verb list.

**Severity:** cosmetic. **Fix:** in `BorgCli::run` None-arm, print the borg-scoped help instead of the root.

### 2. `sb borg --help` after_help text references the wrong log path (cosmetic)

`sb borg --help` ends with `Logs are written to: /home/saidler/.local/share/borg/logs/borg.log`. Actual log location after the layout consolidation is `~/.local/share/sb/borg.log`. Same staleness in `sb cortex --help` (`/home/saidler/.local/share/cortex/logs/cortex.log`).

`sb/src/cli/borg.rs:390` and `sb/src/cli/cortex.rs:18` hardcode the old XDG-per-subsystem path. Should derive from the same logic in `sb/src/logger.rs::log_path`.

**Severity:** cosmetic. **Fix:** centralize log-path computation; have after_help text call it.

### 3. `sb status` "fastembed cache missing" check checks the wrong directory (minor UX)

`sb status` reports `⚠️  fastembed cache missing` because it checks `~/.cache/fastembed/`. Actual embeddings on this machine load from candle's cache (`~/.cache/huggingface/hub/models--BAAI--bge-small-en-v1.5/`). After `sb oracle call knowledge_search` ran successfully, that cache exists and the model loaded — but `sb status` still says "missing" because it's checking a path the system doesn't use.

`sb/src/cli/checks.rs:204` (`embedding_findings`). Should check both fastembed and candle cache paths, or query the vault crate for which backend is active.

**Severity:** minor UX (reports a false warning).

### 4. `sb borg blocklist remove` requires `<DOMAIN>` arg but `--help` doesn't show it as required (minor UX)

`sb borg blocklist remove --help` shows `domain` as positional with no marker. Running `sb borg blocklist remove` (no arg) errors with clap's `required arguments were not provided` — correct behavior, but the help could indicate it's required.

**Severity:** minor UX. **Fix:** add a usage line or mark explicitly.

## Skipped (mutating)

Documented but not executed during this read-only shakedown:

- `sb borg {daemon --install/--uninstall/--reinstall/--start/--stop/--restart, ingest, note, hotkey, sign, migrate --apply, dlq {archive, replay}, blocklist {remove, clear}, dashboard refresh, retention sweep}`
- `sb cortex {classify --apply, lint --apply, link --apply, intel --daily/--weekly, daemon --install/--start/..., migrate --apply, sweep --migrate, summarize --backfill (without --dry-run), embed --backfill}`
- `sb oracle serve` (long-running MCP stdio)
- `sb bootstrap` (would write config files; idempotent but skipped for reversibility)

## Conclusion

`sb v0.8.2-7-gfb73a72` is **ready for the cutover gate**. All read-only verbs work end-to-end; error handling is clean; output formats are well-formed; pipelines compose cleanly with `jq`. The four findings above are all cosmetic / minor UX — none block cutover.

Recommended next steps before cutover:

1. Fix the after_help log path strings (Finding #2) — they'll mislead anyone reading `--help` on the live system.
2. Optionally fix the fastembed-cache check (Finding #3) so `sb status` stops reporting a false warning.
3. The `sb borg` → root-help quirk (Finding #1) is purely cosmetic; defer.
4. Run `sb borg daemon --install && sb cortex daemon --install` per the cutover plan; smoke-test daemons come back up under the new binary.
