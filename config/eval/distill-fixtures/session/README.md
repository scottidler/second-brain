# Session export contract fixtures (Phase 0 spike)

Real `clyde session export` output (schema-version 1), captured against the
live catalog on `desk` (`clyde v0.10.1`, pre-files-touched-export) for
`docs/design/2026-07-17-harvest-clyde-sessions.md` Phase 0. Unlike the
`source.md`/`distilled.yml` pairs elsewhere in `distill-fixtures/`, these are
the raw contract payloads, not distillation fixtures - the input Phase 3's
export reader and Phase 7's golden session fixtures build against, per the
design doc's "reuse clyde's Phase 0 fixtures where they fit."

- `bulk-envelope.json` - a curated 8-session slice of a real
  `clyde session export` bulk-metadata call, hand-picked (not a bulk dump) to
  exercise every documented `enrich-status` value clyde has actually written
  (`ok`, `skipped-personal`, `failed`, `null`/never-enriched), `dormant`
  true/false, `repo` present (`tatari-tv/marquee`, `tatari-tv/pagerduty-cli`,
  `NateBJones-Projects/ringer`) and present-null, and a nonzero
  `redaction-count`. `skipped-empty` does not appear (zero rows carry it in
  the live 1450-session catalog as of this capture) - its presence in the
  contract is confirmed instead against `clyde/docs/design/2026-07-17-session-export-contract.md:87`,
  which freezes it as a legal value clyde's code paths write (`db.rs:358/388/400`
  in the clyde repo).
- `with-body-envelope.json` - one `clyde session export --id <id> --with-body`
  payload: a benign, non-personal work session (PR review housekeeping,
  `tatari-tv/marquee` PR #23 CodeRabbit comments) chosen specifically so a
  real transcript could be checked in without exposing sensitive content.
  Confirms the `body` array shape (`role`/`text`/`subagent`) and the
  `body-truncated`/`body-error` fields.

All sessions in both fixtures are `redaction-count: 0` except one
(`88547451-...`, `redaction-count: 1`) picked deliberately to prove the field
is populated in the wild, not just present-as-zero.

See the implementation notes
(`docs/design/2026-07-17-harvest-clyde-sessions-implementation-notes.md`,
Phase 0) for the full Selection-signal -> contract-field mapping table.

## Null-tolerance regression fixtures (harvest-completion Phase 0 spike)

`bulk-envelope.json` above predates the null-string-fields bug (it never
happened to contain a null `title`/`first-prompt`/`cwd`/`created` row), which
is exactly how the strictness gap survived review. These three fixtures were
captured against the SAME live catalog (`desk`, `clyde v0.10.1`) specifically
to contain the full null class
(`docs/design/2026-07-20-harvest-completion.md` Phase 0):

- `null-string-fields.json` - a real bulk-metadata envelope, five real
  sessions (no fabricated fields except one redacted `first-prompt`):
  - `9b17cdba-...` - `cwd`, `created`, `title`, `first-prompt`, `git-branch`,
    `repo`, `model`, `summary` are ALL JSON `null` in the same real record
    (`n-msgs: 0`, an empty/never-touched session). This is the single
    richest real null-bearing row found in the 60d catalog.
  - `2e324122-...` / `b81fb8a2-...` - `title` + `first-prompt` null, `cwd`/
    `created` present.
  - `28b526fb-...` - `first-prompt` null only (`title` present as `"mcp"`).
  - `687368e0-...` - a real WORK session (`repo: scottidler/claude`) with
    `model`/`summary` null; its `first-prompt` (a long unified diff) is
    replaced with a benign placeholder string per the redaction instruction,
    the null-vs-present shape of every other field is untouched.
- `empty-body.json` - REAL output of
  `clyde session export --id 9b17cdba --with-body`: `body: null`,
  `body-error: "parsed empty"`. Confirms the empty-body class is a real
  clyde output, not a hypothetical.
- `malformed-record.json` - SYNTHETIC-MALFORMED (labeled explicitly, not a
  real export): a real session (`28b526fb-...`) followed by a hand-edited
  twin whose `session-id` is renamed to
  `malformed-00000000-0000-0000-0000-000000000000` and whose `n-msgs` field
  (contract type `i64`) is corrupted to the string `"not-a-number"`. Every
  other field is left as the real record's, so the fixture documents "one
  wrong-typed field breaks the batch," not a wholesale fabrication.

These fixtures feed the RED regression test `parse_tolerates_null_string_fields`
in `borg/src/harvest/contract/tests.rs`, added in Phase 0 to lock the bug
before Phase 1 fixes it.
