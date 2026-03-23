# Design Document: Fix Classify Pipeline - PATH and Tag Matching

**Author:** Scott Idler
**Date:** 2026-03-23
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The cortex classify pipeline is completely non-functional in production: Tier 2 (LLM via Fabric) is dead because the systemd service lacks `go/bin` in PATH, and Tier 1 (deterministic tag matching) is too narrow because it uses exact string equality against a small set of trigger words. Result: 81 of 83 inbox notes are stuck with `cortex-needs-review: true` and nothing gets promoted to `notes/`.

## Problem Statement

### Background

The classify pipeline was implemented per the 2026-03-21 design doc with a 3-tier approach:
1. **Tier 1a - Tag matching:** map note tags to domains via `tag_domain_map`
2. **Tier 1b - Source URL matching:** map source URLs to domains via `source_domain_map`
3. **Tier 2 - LLM classification:** use Fabric CLI to classify with vault context from SearchIndex
4. **Tier 3 - Hold for review:** no signal, mark `cortex-needs-review: true`

### Problem

Two independent failures cause 100% of inbox notes to be held for review:

**Failure 1: Fabric not on PATH (Tier 2 dead)**

The cortex systemd unit has no `Environment=` directive. The borg service correctly sets:
```
Environment="PATH=/home/saidler/.local/bin:/home/saidler/.cargo/bin:/home/saidler/go/bin:..."
```
But cortex inherits the minimal systemd default PATH which does not include `/home/saidler/go/bin/`. The `is_available()` function uses `which::which("fabric")` which checks PATH. Result: every note that reaches Tier 2 sees "fabric not available" and falls through to Tier 3.

**Failure 2: Tag matching too narrow (Tier 1 mostly dead)**

The `tag_domain_map` has ~50 exact trigger words like `"claude"`, `"llm"`, `"obsidian"`, `"rust"`. But borg's Fabric-generated tags are compound slugs: `ai-agents`, `claude-code`, `ai-coding-agents`, `prompt-engineering`, `large-language-models`. The match logic at `classify.rs:475` does exact string equality:
```rust
if trigger_tags.iter().any(|t| t.to_lowercase() == lower_tag)
```
The top inbox tags (`ai-agents` x16, `ai-strategy` x11, `claude-code` x10, `prompt-engineering` x8) match zero trigger words.

Additionally, 19 of 83 inbox notes have no tags at all, so Tier 1a can never work for those.

**Compounding factor: `cortex-needs-review` flag**

After the first classify cycle, 81 notes got stamped `cortex-needs-review: true`. The `filter_inbox_notes` function skips notes that have `cortex-classified: true` but does NOT skip `cortex-needs-review` notes (unless `review_only` flag is set). However, those notes still fail classification on subsequent cycles for the same reasons, producing redundant log spam.

### Goals

- Fix Fabric availability in the cortex systemd service
- Make Tier 1 tag matching work with compound/slug tags from borg's Fabric patterns
- Automatically drain the current 83-note inbox backlog once fixes are deployed

### Non-Goals

- Rewriting the LLM classification logic (Tier 2)
- Changing borg's tag generation approach
- Adding new domains or restructuring the vault
- Building a manual triage UI

## Proposed Solution

### Overview

Two fixes, ordered by impact and independence:

### Fix 1: Add PATH to cortex.service

Mirror the borg service's `Environment=` directive in the cortex systemd unit:

```ini
Environment="PATH=/home/saidler/.local/bin:/home/saidler/.cargo/bin:/home/saidler/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
```

This immediately unblocks Tier 2 LLM classification for all notes that fail Tier 1.

### Fix 2: Segment-based tag matching in Tier 1

Change `classify_by_tags` to split compound tags on hyphens and match individual segments against trigger words. A note tag like `ai-agents` splits to `["ai", "agents"]` - if either segment matches a trigger word, the tag counts toward that domain. `claude-code` splits to `["claude", "code"]` and matches `"claude"` in the ai domain.

The matching logic at `classify.rs:475` changes from:
```rust
if trigger_tags.iter().any(|t| t.to_lowercase() == lower_tag)
```
to:
```rust
if trigger_tags.iter().any(|t| {
    let t_lower = t.to_lowercase();
    lower_tag == t_lower
        || lower_tag.split('-').any(|segment| segment == t_lower)
})
```

This splits compound tags on hyphens and checks if any segment matches a trigger word. This is more precise than pure substring matching (`"cli"` won't match `"clinical"`) because it operates on hyphen-delimited segments.

The `tag_domain_map` also needs a few new trigger words that segment matching alone can't resolve:

| Domain | Add triggers | Reason |
|--------|-------------|--------|
| ai | `ai` | segment `"ai"` in `ai-coding`, `ai-strategy`, etc. - currently not a trigger |
| tech | `programming`, `gemini` | common segments not yet covered |
| knowledge | `productivity` | appears 5x, segments to just `"productivity"` |

Most compound tags (e.g., `ai-agents`, `claude-code`, `prompt-engineering`) are already covered by segment matching against existing triggers (`agents`, `claude`, `prompting`).

**Scoring semantics are preserved:** The existing loop structure scores +1 per note_tag per domain (using `any()` inside the trigger check). Segment matching stays inside that `any()` closure, so one compound tag like `ai-agents` still gives at most +1 to the ai domain even though both `"ai"` and `"agents"` are triggers. No double-counting.

**Edge cases for segment matching:**
- Single-word tags (`"rust"`) split to `["rust"]` - works identically to exact match
- Tags with many segments (`"ai-coding-agents"`) split to `["ai", "coding", "agents"]` - any segment matching a trigger counts
- Empty segments from malformed tags (`"ai--agents"`) produce empty strings that match nothing - safe
- The `"ai"` trigger will match every `ai-*` prefixed tag, which is correct since those tags are inherently ai-related

### Fix 3 is not needed

The `cortex-needs-review` flag does NOT block reprocessing. Only `cortex-classified: true` does (see `filter_inbox_notes` at classify.rs:540-546). Since none of the 83 inbox notes were ever successfully classified, the fixed daemon will automatically reprocess them all on its next cycle. No manual intervention required.

### Implementation Plan

**Phase 1: Systemd fix (infra)**
1. Add `Environment=PATH=...` to `cortex.service`
2. Add hardening directives matching borg's service (NoNewPrivileges, ProtectSystem, etc.)
3. `systemctl --user daemon-reload && systemctl --user restart cortex`
4. Verify via logs: `fabric not available` messages should stop

**Phase 2: Tag matching improvement (code)**
1. Update `classify_by_tags` in `classify.rs` to use segment matching
2. Expand `default_tag_domain_map()` with common compound triggers
3. Add tests for compound tag matching
4. `otto ci` to validate

**Phase 3: Deploy and verify**
1. Build and install updated cortex: `cargo install --path cortex`
2. Restart daemon: `systemctl --user restart cortex`
3. Verify classify is promoting notes in daemon logs (look for `promoted N note(s)`)
4. Check inbox count is decreasing: `ls ~/repos/scottidler/obsidian/inbox/ | wc -l`

## Alternatives Considered

### Alternative 1: Full substring matching
- **Description:** `lower_tag.contains(&t_lower)` instead of segment matching
- **Pros:** Catches more matches
- **Cons:** False positives - `"cli"` matches `"clinical"`, `"ai"` matches `"maintain"`
- **Why not chosen:** Too loose, would misclassify notes

### Alternative 2: Expand trigger list only (no code change)
- **Description:** Add every known compound tag to the `tag_domain_map`
- **Pros:** No code changes needed
- **Cons:** Brittle - every new compound tag from borg requires a config update. Whack-a-mole.
- **Why not chosen:** Doesn't scale. Segment matching handles the general case.

### Alternative 3: Skip Tier 1, rely entirely on Tier 2 LLM
- **Description:** Just fix the PATH issue and let the LLM handle everything
- **Pros:** LLM can classify based on content, not just tags
- **Cons:** Costs API tokens for every note, adds latency, Fabric can be flaky. Deterministic classification should be the fast path.
- **Why not chosen:** Tier 1 should handle the easy cases cheaply. LLM is the fallback.

### Alternative 4: Embed fabric path in cortex config
- **Description:** Add a `fabric-binary` config field and use absolute path
- **Pros:** Doesn't depend on PATH
- **Cons:** Still need PATH for other tools, doesn't fix the root cause. Borg already solved this with Environment=.
- **Why not chosen:** The systemd PATH fix is the standard solution and fixes all external tool lookups at once.

## Technical Considerations

### Dependencies
- `which` crate (already used) for `is_available()`
- No new dependencies needed

### Performance
- Segment matching adds negligible cost (splitting on `-` for ~50 trigger words x ~10 tags per note)
- Fixing Fabric availability means Tier 2 LLM calls will now execute when Tier 1 fails - this adds 5-30s per note but only for notes that genuinely can't be classified deterministically

### Testing Strategy
- Unit tests for segment-based tag matching (compound tags, single-word tags, edge cases)
- Unit test for ambiguous ties with compound tags
- Integration: restart daemon, verify inbox count drops in logs

### Rollout Plan
1. Deploy systemd fix first (zero code change, immediate effect for Tier 2)
2. Deploy code fix second (Tier 1 improvement)
3. Monitor logs for promoted note count - backlog should clear automatically within one daemon cycle

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Segment matching creates false positives | Low | Med | Conservative trigger words, test edge cases, LLM as backstop |
| Fabric LLM calls are slow/expensive for 83-note backlog | Med | Low | Tier 1 improvements should classify most notes before Tier 2 |
| Notes get misclassified to wrong domain | Low | Low | `cortex-classified-by` field tracks method, easy to audit and re-run |
| Daemon oscillation after bulk classify | Med | Low | Existing cycle detection handles this, may skip 1-2 cycles |
| `ai` trigger dominates scoring (many `ai-*` tags per note) | Med | Low | Correct behavior - notes with 10+ `ai-*` tags ARE about ai. If misclassified, `cortex-classified-by` enables audit |

## Open Questions
- [ ] Should `source_domain_map` in config be populated with common URL patterns (youtube.com -> varies, github.com -> tech)?
- [ ] Worth adding a `cortex classify --dry-run` output to the daemon logs each cycle so we can see what WOULD be classified?

## References
- [2026-03-21 Classify & Promote design doc](2026-03-21-cortex-classify-promote.md) - original classify pipeline design
- [cortex.service](~/.config/systemd/user/cortex.service) - systemd unit
- [borg.service](~/.config/systemd/user/borg.service) - reference for correct PATH setup
- [classify.rs](../../cortex/src/classify.rs) - classify implementation
