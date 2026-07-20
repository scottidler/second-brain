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
