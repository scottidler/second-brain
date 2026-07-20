# Harvest selection fixtures (Phase 3)

Golden + edge fixtures for `sb borg harvest`'s selection gate, thread
clustering, and watermark / re-appearance logic
(`docs/design/2026-07-17-harvest-clyde-sessions.md`, Phase 3). These are
schema-version-1 `clyde session export` bulk-metadata payloads. Distinct from
`config/eval/distill-fixtures/session/` (Phase 0's raw contract-coverage
captures) and from the `source.md`/`distilled.yml` distillation fixtures.

## `golden-2026-07-02.json` - the golden selection fixture

The four sessions of the 2026-07-02 "token-broker v2 / Slack CLI" arc, captured
from the LIVE clyde catalog via `clyde session export --id <id>` on `desk`
(2026-07-19). Every SELECTION-RELEVANT field is real and verbatim: `session-id`,
`cwd`, `repo`, `git-branch`, `created`, `modified`, `n-msgs`, `dormant`,
`enrich-status`, `scope`. The free-text `title`/`first-prompt`/`summary` are
REDACTED to benign placeholders (the design doc's "pick benign sessions or
redact" instruction - the real prompts carry work internals; they are not
load-bearing for selection).

**Deterministic outcome the golden test asserts** (this is the honest
deterministic result, NOT the hand-written note's "4 sessions = 1 note" - see
below):

| session   | cwd                                  | enrich-status     | outcome  |
|-----------|--------------------------------------|-------------------|----------|
| `9521f589`| `/home/saidler`                      | `skipped-personal`| REJECTED |
| `4e55a52c`| `/home/saidler`                      | `skipped-personal`| REJECTED |
| `871f6428`| `.../tatari-tv/slack-cli/main`       | `ok`              | selected |
| `4ae69e3a`| `.../tatari-tv/slack-cli/main`       | `ok`              | selected |

- Selected ids: `{871f6428, 4ae69e3a}`. Both share `(cwd, git-branch)` and are
  ~15s apart -> **1 thread, 1 note**, primary `871f6428` (486 > 320 msgs).
- Rejected ids: `{9521f589, 4e55a52c}` - `skipped-personal` (and their cwd is
  not a `~/repos/<org>/<repo>` anchor either).

### Why not "4 sessions = 1 note"?

The hand-written `obsidian/notes/ai/2026-07-02-claude-sessions-summary.md`
grouped four sessions under this subject by HUMAN judgment that crossed two
distinct cwds AND the selection bar (it included the two `skipped-personal`
`/home/saidler` sessions). The deterministic Phase 3 rule (dormant + enrich-ok +
real-repo cwd, then `(cwd, git-branch) + gap` clustering) cannot and must not
reproduce that: two of the four fail selection, and the survivors live in a
different cwd. Seeing the multi-cwd subject whole is exactly the job the design
defers to the repo-hub layer (Phases 9-13), and the doc's "collapses to what the
deterministic rule produces / known limitation, tested not hidden" language
anticipates this. The fixture therefore pins the deterministic truth, with real
data, rather than a number the code should not produce.

## `same-cwd-unrelated.json` - the non-merge boundary

Two CONSTRUCTED (not captured) sessions sharing `(cwd, git-branch)` in one repo
but separated by ~4h - more than the 2h `thread-window`. Proves the gap
boundary splits them into **2 threads**, so same-repo work on unrelated topics
hours apart does not blindly merge.

## `reject-cases.json` - one session per rejection reason

Constructed sessions, each failing a DIFFERENT selection signal: not-dormant,
`skipped-personal`, non-repo cwd, below `min-msgs`, and an excluded title
pattern (`security-review`). Drives the reject-path test (a `rejection.yml` +
a `rejected` receipts row keyed by the selection-time trace).

## `single-repo-session.json` - re-appearance base

One selected session in a real repo (structural fields from the real marquee
PR #23 session, benign redacted prose). The re-appearance tests seed a
published watermark entry for it, then drive resumed-body / unchanged-body /
unchanged-cursor cases through a fake reader.
