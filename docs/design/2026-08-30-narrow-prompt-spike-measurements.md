# Phase 0 spike: narrow-prompt token measurements

**Date:** 2026-08-30
**Companion to:** `2026-08-30-video-distill-token-budget.md`

Raw API measurements taken against `claude-sonnet-5` at `max_tokens=4096`, with the
pattern as the `system` message and the input as the `user` message (matching
fabric's non-raw composition, `internal/core/chatter.go:280,339`). The scratch
narrow patterns were carved from `borg/patterns/distill-video.md` verbatim.

Corpus:

- **A** = `DWWrLlM3gwQ`, staged transcript at
  `~/.local/share/sb/borg/stages/cl-1dc7db93/distilled.yml`, 34,092 chars. This is a
  BORDERLINE gate case: the creator tours his tools without ever declaring a count
  (`declared_count: null`).
- **B** = `config/eval/distill-fixtures/video/top-10-claude-code-skills-plugins-clis-april-2026/source.md`,
  6,918 chars. Unambiguous listicle, ground truth 10 items, `declared_count: 10`.

## Corpus A, thinking at model default (adaptive)

| call | run | stop_reason | output | thinking | text | spare/4096 | items |
|---|---|---|---|---|---|---|---|
| `-enumeration` | 1 | `max_tokens` | 4096 | 4095 | 1 | 0 | 0 |
| `-enumeration` | 2 | `end_turn` | 2883 | 1536 | 1347 | 1213 | 19 |
| `-enumeration` | 3 | `end_turn` | 3328 | 2079 | 1249 | 768 | 19 |
| `-enumeration` | 4 | `end_turn` | 3556 | 2347 | 1209 | 540 | 19 |
| `-enumeration` | 5 | `end_turn` | 2695 | 1415 | 1280 | 1401 | 19 |
| `-summary` | 1 | `end_turn` | 510 | 38 | 472 | 3586 | — |
| `-summary` | 2 | `end_turn` | 464 | 0 | 464 | 3632 | — |
| `-summary` | 3 | `end_turn` | 422 | 0 | 422 | 3674 | — |
| `-ideas` (with wave-1 hint) | 1 | `end_turn` | 1851 | 618 | 1233 | 2245 | — |
| `-ideas` (with wave-1 hint) | 2 | `end_turn` | 1609 | 343 | 1266 | 2487 | — |
| `-ideas` (with wave-1 hint) | 3 | `end_turn` | 1980 | 787 | 1193 | 2116 | — |

`-ideas` run 1 produced `key_ideas=5`, `claims=10`, and **0** repeats of the 19
wave-1 item names, so the prompt-level no-repeat rule survived the split.

## Corpus A, enumeration call only, thinking lever swept (direct API)

| setting | outputs per run | max thinking | worst total | worst spare | stop_reasons | items per run |
|---|---|---|---|---|---|---|
| `off` | 1214, 15, 15, 1272, 1196, 15 | 0 | 1272 | 2824 | end_turn | 19, 0, 0, 19, 19, 0 |
| `low` | 1130, 15, 15, 15, 100, 15 | 84 | 1130 | 2966 | end_turn | 19, 0, 0, 0, 0, 0 |
| `medium` | 2445, 101, 2146, 88, 1898, 1982 | 1148 | 2445 | 1651 | end_turn | 19, 0, 19, 0, 19, 19 |

`low` and `medium` were reachable here ONLY because this harness calls the API
directly with `thinking.type=adaptive` + `output_config.effort`. Through fabric they
return HTTP 400 (see the reachability table below).

## Corpus B, enumeration call, detection quality

| setting | run | stop_reason | output | thinking | spare | items (truth = 10) |
|---|---|---|---|---|---|---|
| `default` | 1 | `end_turn` | 854 | 163 | 3242 | 10 |
| `default` | 2 | `end_turn` | 966 | 245 | 3130 | 10 |
| `default` | 3 | `end_turn` | 920 | 207 | 3176 | 10 |
| `default` | 4 | `end_turn` | 961 | 243 | 3135 | 10 |
| `off` | 1 | `end_turn` | 677 | 0 | 3419 | 10 |
| `off` | 2 | `end_turn` | 684 | 0 | 3412 | 10 |
| `off` | 3 | `end_turn` | 645 | 0 | 3451 | 10 |
| `off` | 4 | `end_turn` | 673 | 0 | 3423 | 10 |

Disabling thinking cost NOTHING on an unambiguous listicle: 10/10 items on 4/4 runs,
identical to default. Default only spent 163-245 thinking tokens here, versus up to
4,095 on the borderline corpus A.

## fabric `--thinking=` reachability (v1.4.470, `claude-sonnet-5`)

Run as `fabric -p <abs-path> -m claude-sonnet-5 --thinking=<value> < transcript`:

| value | exit | result |
|---|---|---|
| `off` | 0 | 3,424 bytes, valid YAML, 19 items |
| `low` | 1 | HTTP 400 |
| `medium` | 1 | HTTP 400 |
| `high` | 1 | HTTP 400 |
| `2048` | 1 | HTTP 400 |

All four failures return an identical body:

```
"thinking.type.enabled" is not supported for this model.
Use "thinking.type.adaptive" and "output_config.effort" to control thinking behavior.
```

fabric emits the legacy `thinking.type.enabled` shape for every non-`off` value.
`off` works because it maps to `thinking.type.disabled`, which the model still
accepts. So **`off` is the only reachable non-default setting** short of forking
fabric, which is out of scope by Resolved Decision.


## Sibling distillers, re-measured against named fixtures

Each is the largest `source.md` for its kind under `config/eval/distill-fixtures/`,
truncated to the 48,000-char single-call maximum where longer. One run each,
`claude-sonnet-5`, `max_tokens=4096`, thinking at model default.

| kind | fixture | input | stop_reason | output | thinking | headroom |
|---|---|---|---|---|---|---|
| `article` | `article/www-theregister-com-ai-ml-2026-05-20-amd-says-its-4k-ryzen-a` | 48,000 ch (truncated) | `end_turn` | 659 | 0 | 3437 |
| `session` | `session/slack-cli-release-promote` | 2,164 ch | `end_turn` | 1043 | 10 | 3053 |
| `thread` | `thread/x-com-vllm-project-status-2059344804295942513` | 4,062 ch | `end_turn` | 1177 | 0 | 2919 |
| `voicenote` | `voicenote/retrieval-cache-idea` | 1,366 ch | `end_turn` | 889 | 10 | 3207 |

No `article` fixture carries a populated `enumeration` block (all five return 0
items), so the first pass's 'real 10-item listicle' article row could not be
reproduced and was dropped from the design doc rather than re-asserted.

## Note on transcript size for `DWWrLlM3gwQ`

The design doc's fat-pattern table cites 26,700 chars for this video, from the
original measurement pass. The staged transcript used for the narrow-prompt
measurements here is 34,092 chars
(`~/.local/share/sb/borg/stages/cl-1dc7db93/distilled.yml`). The two numbers come
from different capture runs of the same video; each table is labelled with the
size it actually used. The narrow-prompt numbers are the LARGER input, so they
are the conservative ones.

## Round-2 measurement: enumeration null rate on corpus A, N=12 per setting

The round-1 spike used 5-6 samples and the doc concluded "detection is identical".
Review-panel round 2 challenged that, correctly. Re-run at N=12:

| setting | items detected | `enumeration: null` (parses clean, no fallback) | truncated (`stop=max_tokens`, loud) | max thinking | max output |
|---|---|---|---|---|---|
| `default` | 7/12 | 3/12 | 4/12 | 4095 | 4096 |
| `off` | 4/12 | 8/12 | 0/12 | 0 | 1332 |

Counts overlap: a truncated run whose cut lands cleanly can also parse as `null`.

This FALSIFIES the round-1 claim that thinking-off is quality-neutral. On the
ambiguous corpus it detects LESS (4/12 vs 7/12) and more than doubles the silent-
null rate (3/12 to 8/12), while removing the loud truncations (4/12 to 0). It
trades a failure the parser catches for one nothing catches.

## Round-2 measurement: the REDUCE-path enumeration prompt

Round 1 measured only the short-path narrow prompts; the reduce variant takes
chunk summaries + a claim pool + candidates, not a raw transcript, so nothing
transferred by analogy. Built from real data end to end:

- Long-path control `f8cfH5XX-XU`, staged at `~/.local/share/sb/borg/stages/ht-c096e5e2/`, 134,273-char transcript.
- Split into 5 chunks (matching production) and run through the REAL
  `borg/patterns/distill-video-chunk.md`. All 5 returned `end_turn`; outputs 890 /
  1,923 / 1,165 / 781 / 1,320, thinking 0 / 823 / 12 / 0 / 597. Worst case 1,923 of
  4,096, which independently confirms the doc's Non-Goal claim that the chunk step
  has ample headroom.
- Chunk outputs assembled per `distillers/src/parse.rs::build_reduce_input`:
  5 summaries, 25 pool claims, 16 enumeration candidates, `Declared count: 3`.
  Result is 7,327 chars, comparable to the largest production reduce input on
  2026-08-30 (7,129 chars).

| setting | N | items detected | null | truncated | max output | max thinking | min spare |
|---|---|---|---|---|---|---|---|
| `default` | 8 | 8/8 | 0/8 | 0/8 | 774 | 547 | 3322 |
| `off` | 8 | 8/8 | 0/8 | 0/8 | 227 | 0 | 3869 |

Every run at both settings returned exactly 3 items. The reduce-path enumeration
call is STABLE and CHEAP: worst total 774 of 4,096 at default thinking, 3,322
spare. It needs no thinking lever.

The reason is structural and it explains the whole defect: the reduce prompt reads
a pre-extracted candidate list carrying `Declared count: 3`, so its gate has
explicit evidence and the model does not deliberate. The short-path prompt reads a
raw transcript where the gate genuinely is ambiguous, and THAT is what burns 4,095
tokens. The budget problem is a property of gate ambiguity, not of enumeration
output size.
