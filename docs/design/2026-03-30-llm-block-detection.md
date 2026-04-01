# Design Document: LLM Block Detection + Domain Blocklist

**Author:** Scott Idler
**Date:** 2026-03-30
**Status:** In Review
**Review Passes Completed:** 5/5

## Summary

Replace brittle regex-based block page detection in borg with a Haiku LLM classifier that compares raw fetched content against curated block page examples. Add a persistent domain blocklist so that known-blocking domains are rejected before any fetch is attempted, giving users immediate and informative feedback.

## Problem Statement

### Background

Borg ingests URLs by fetching content and summarizing it with Fabric. A quality gate in `quality.rs` uses regex patterns to detect block pages (Cloudflare, captcha, etc.) before note creation. Two recent failed ingestions slipped through:

- `xda-developers.com` - returned "Anonymous access to domain blocked until Mon Mar 30 2026 due to previous abuse"
- `howtogeek.com` - returned a custom DDoS block page

Both sites returned HTTP 200 with error-page content. The existing patterns didn't cover these custom block formats. Additionally, the quality gate fires on the Fabric *summary* - after the LLM has already turned the error page into coherent-looking prose. By then the raw block signal is gone.

### Problem

1. **Pattern brittleness** - block pages vary infinitely; each new variant requires a new regex
2. **Late detection** - the quality gate runs post-summarization; Fabric sanitizes error content before the gate can catch it
3. **No memory** - every ingest attempt hits a blocked domain fresh, with no record of past failures
4. **Poor UX** - failed notes silently appear in the vault rather than surfacing a clear error

### Goals

- Detect block pages on *raw fetched content* before summarization, using LLM classification with few-shot examples
- Maintain a persistent domain blocklist to fast-fail on known-blocking domains
- Give users clear, informative feedback when a domain is known to block
- Ship curated block page examples alongside the binary
- Allow the blocklist to be inspected and managed via CLI

### Non-Goals

- Bypassing or circumventing block pages (user-agent spoofing, proxies, CAPTCHA solving)
- Blocking specific URLs or paths - only whole domains
- Replacing the existing truncation artifact detection
- Automatically "retrying with a different fetcher" when blocked

## Proposed Solution

### Overview

Three components:

1. **`classify.rs`** - Haiku LLM classifier: takes raw markdown + few-shot block page examples, returns `BlockResult`
2. **`blocklist.rs`** - Persistent domain blocklist at `~/.local/share/borg/blocked-domains.yml`
3. **Pipeline wiring** - Two new checkpoints added to `pipeline.rs`

The existing pattern check in `quality.rs` is retained as a free pre-filter on raw content (it is moved earlier in the pipeline - see below). Haiku runs only when patterns pass, avoiding unnecessary API calls.

### Architecture

```
URL received
     |
     v
[1] Domain blocklist check  (blocklist.rs)
    Known blocked + not expired  --> bail: "howtogeek.com is a known blocking domain
                                            (2 attempts, last: 2026-03-30). Skipping."
    Known blocked + expired      --> warn + allow attempt; remove on success
     |
     v
Content fetched via Fabric / markitdown / Jina  (unchanged)
     |
     v
[2a] Pattern check on RAW content  (quality.rs - MOVED earlier)
     Catches obvious Cloudflare / captcha pages at zero cost
     |
     v
[2b] Haiku classifier on RAW content  (classify.rs)
     Few-shot prompt: is this a block page or real content?
     Blocked --> record domain in blocklist, bail with reason
     |
     v
Fabric summarization  (unchanged)
     |
     v
Truncation artifact check  (quality.rs - unchanged)
     |
     v
Note created
```

Note: steps [2a] and [2b] replace the *existing* `detect_blocked_content` call at `pipeline.rs:424` which currently fires on the summary. The pattern check moves to raw content; the Haiku check is new.

### Data Model

**`classify.rs`**

```rust
pub struct BlockResult {
    pub is_blocked: bool,
    pub reason: String,  // human-readable, logged and surfaced to user
}
```

**`blocklist.rs`**

```rust
pub struct BlocklistEntry {
    pub first_blocked: DateTime<Utc>,
    pub last_blocked: DateTime<Utc>,
    pub block_count: u32,
    pub expires: Option<DateTime<Utc>>,  // parsed from block page if detectable
    pub last_reason: String,
}

pub struct DomainBlocklist {
    domains: HashMap<String, BlocklistEntry>,
    path: PathBuf,
}
```

Stored as YAML at `~/.local/share/borg/blocked-domains.yml`:

```yaml
domains:
  howtogeek.com:
    first-blocked: "2026-03-30T08:34:45Z"
    last-blocked: "2026-03-30T08:34:45Z"
    block-count: 2
    expires: null
    last-reason: "Security block page detected: suspected DDoS protection"
  xda-developers.com:
    first-blocked: "2026-03-30T08:34:45Z"
    last-blocked: "2026-03-30T08:34:45Z"
    block-count: 1
    expires: "2026-03-31T08:34:45Z"
    last-reason: "Anonymous access blocked until 2026-03-31 due to previous abuse"
```

**Config addition** (`config.rs`):

```rust
pub struct ClassifyConfig {
    pub enabled: bool,              // default: true
    pub model: String,              // default: "claude-haiku-4-5-20251001"
    pub examples_path: PathBuf,     // default: ~/.config/borg/block-examples
    pub confidence_threshold: f32,  // reserved for future use; currently unused
}
```

API key comes from `ANTHROPIC_API_KEY` env var, following the same convention as Fabric.

### Block Examples

Stored at `borg/block-examples/` in the repo (source of truth), installed to `~/.config/borg/block-examples/` on `otto install`.

Each file is a real block page response captured as plain text. Initial set:

| File | Source |
|------|--------|
| `cloudflare-just-a-moment.txt` | Cloudflare browser check page |
| `cloudflare-access-denied.txt` | Cloudflare access denied |
| `xda-anonymous-access-blocked.txt` | XDA Developers custom block |
| `howtogeek-ddos-block.txt` | HowToGeek DDoS protection page |

Each example file begins with a comment line (ignored in prompts):
```
# Source: xda-developers.com, 2026-03-30, type: anonymous-access-block
```

When a new block type is detected, the raw content is saved automatically to a staging area for the operator to review and promote to the examples set.

### Haiku Prompt Structure

```
System: You are a content classifier. Determine whether the provided text is a
security/bot block page or real article content. Respond with JSON only:
{"blocked": true/false, "reason": "brief explanation"}

User: Here are examples of block pages:

--- EXAMPLE 1 (cloudflare-just-a-moment) ---
<contents of cloudflare-just-a-moment.txt>

--- EXAMPLE 2 (xda-anonymous-access-blocked) ---
<contents of xda-anonymous-access-blocked.txt>

--- CONTENT TO CLASSIFY ---
<raw fetched markdown, truncated to first 2000 chars>

Is this a block page?
```

Only the first 2000 characters of the raw content are sent - block pages reveal themselves immediately, and this keeps the call fast and cheap.

### Domain Normalization

Domains are normalized to their registrable root before all blocklist operations:
- `www.xda-developers.com` -> `xda-developers.com`
- `m.howtogeek.com` -> `howtogeek.com`

This prevents subdomain variants from bypassing the list.

### `borg blocklist` CLI Subcommand

```
borg blocklist              # list all blocked domains with count and last-blocked date
borg blocklist --show <domain>   # show full entry for a domain
borg blocklist --remove <domain> # remove a domain from the list
borg blocklist --clear      # wipe the entire list
```

### Reingest Behavior

When `--reingest` is passed, the domain blocklist check is skipped with a warning. The operator is explicitly requesting a retry and should receive it.

```
[WARN] howtogeek.com is on the domain blocklist (2 prior blocks) - proceeding anyway (--reingest)
```

### Implementation Plan

**Phase 1 - Blocklist**
- Create `borg/src/blocklist.rs` with `DomainBlocklist` (load/save/check/record/is_expired)
- Wire pre-fetch domain check into `pipeline.rs` - extract domain from URL, check blocklist
- When existing quality gate triggers, record the domain in the blocklist before bailing
- Add `borg blocklist` subcommand to `cli.rs`

**Phase 2 - Classifier**
- Create `borg/src/classify.rs` with `classify_content(content, config) -> Result<BlockResult>`
- Ship 4 initial block page examples in `borg/block-examples/`
- Move pattern check from post-summary to post-fetch (raw content)
- Add Haiku classification call after pattern check
- When Haiku blocks, record domain in blocklist

**Phase 3 - Config + Install**
- Add `ClassifyConfig` to `config.rs` with defaults
- Add `classify` section to default `borg.yml` template
- Update install instructions to copy `block-examples/`
- Add staging directory logic for auto-saving new block page samples

## Alternatives Considered

### Alternative 1: Expand the pattern list
- **Description:** Add more regex patterns to `quality.rs` for each new block page variant encountered
- **Pros:** Zero latency, zero cost, no new dependencies
- **Cons:** Whack-a-mole forever - block pages vary infinitely. Doesn't fix late detection. Already failed twice in a single day.
- **Why not chosen:** Doesn't scale; brittle by design.

### Alternative 2: Treat non-200 HTTP responses as failures
- **Description:** Fail if the HTTP status code is not 200
- **Pros:** Simple and reliable for true server errors
- **Cons:** All known block pages return HTTP 200. This would not help at all.
- **Why not chosen:** Doesn't apply to the problem.

### Alternative 3: Route classification through Fabric CLI
- **Description:** Use the existing `fabric` binary with a classify pattern instead of direct API call
- **Pros:** Reuses existing infrastructure, no new API client code
- **Cons:** Fabric CLI startup overhead per ingest; harder to enforce structured JSON output; model selection less predictable
- **Why not chosen:** Direct API call is faster, cheaper (Haiku), and gives clean structured output.

### Alternative 4: Use a dedicated text classifier model (e.g. fine-tuned BERT)
- **Description:** Train or download a small classifier specifically for block page detection
- **Pros:** Potentially very fast, no API cost after setup
- **Cons:** Operational complexity; requires a model hosting story; overkill for the frequency of this check
- **Why not chosen:** Not justified for the scale of this problem.

## Technical Considerations

### Dependencies

- `reqwest` (already present) - HTTP client for Anthropic API call
- `serde_json` (already present) - API request/response parsing
- `url` (already present) - domain extraction and normalization
- No new crate dependencies required

### Performance

- Domain blocklist check: negligible (in-memory HashMap lookup after initial file load)
- Pattern check on raw content: negligible (unchanged logic, moved earlier)
- Haiku classification: ~200-500ms, one call per article that passes the pattern check
- Block examples: loaded once at startup and cached in `ClassifyConfig`

The blocklist check eliminates the Haiku call for known-blocking domains, so repeated attempts against blocked domains cost nothing.

### Security

- `ANTHROPIC_API_KEY` must be set in environment - consistent with how Fabric handles its keys
- Block examples contain sanitized error page content, no secrets or credentials
- The blocklist file is user-local (`~/.local/share/borg/`) and not shared across users

### Testing Strategy

- Unit tests for `blocklist.rs`: load/save round-trip, expiry logic, domain normalization, concurrent write safety
- Unit tests for `classify.rs`: mock Anthropic API responses for both blocked and legitimate content
- Regression fixtures: the two specific block pages that triggered this work, as named test files
- Integration test: real block page content fed to the full pipeline, verify it does not produce a note

### Rollout Plan

- The domain blocklist is active immediately after Phase 1 with no config opt-in required
- The Haiku classifier defaults to `enabled: true` but requires `ANTHROPIC_API_KEY` to be set; if the key is absent, it logs a warning and falls through to the pattern check
- No flag day - the system degrades gracefully without the classifier configured

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| False positive - classifies real article as blocked | Low | Medium | Log reason; user can `--reingest` to force; staging area captures the sample for review |
| Haiku API unavailable or key missing | Medium | Low | Non-fatal fallthrough to pattern check; logged as warning |
| Blocklist entry for a domain that later unblocks | Medium | Low | Expiry field; automatic removal on successful `--reingest`; `borg blocklist --remove` |
| Block examples directory missing at runtime | Low | Low | Classifier logs warning and skips few-shot examples; still makes the call with zero-shot |
| Concurrent ingests write blocklist simultaneously | Low | Low | Atomic file write (write to temp, rename); no long-lived lock needed |
| Domain normalization strips relevant subdomain | Very Low | Low | Log which domain was normalized so user can see it |

## Open Questions

- [ ] Default TTL when no expiry is parseable from the block page - 7 days seems conservative; 24h might be better to avoid silently blocking a domain that recovers quickly
- [ ] Should classifier errors be fatal or fall-through? Proposal: fall-through with warning - better to ingest a garbage note (which the user can delete) than to silently drop a valid URL
- [ ] Should new block page samples be auto-staged to a directory for the operator to review and promote to the example set? Useful for improving coverage over time.
- [ ] Is per-user (`~/.local/share/borg/`) the right scope for the blocklist? Seems correct - blocking is about the website, not the vault.

## References

- `borg/src/quality.rs` - current pattern-based quality gate
- `borg/src/pipeline.rs:424` - current quality gate call site
- `borg/src/pipeline.rs:627-663` - `process_article_fabric` and `process_article_jina`
- `borg/src/config.rs` - configuration structure
- Anthropic Messages API: `https://api.anthropic.com/v1/messages`
- Failed note examples: `notes/rebuilt-note-taking-system-from-scratch.md`, `notes/free-linux-tool-keeps-terminal-sessions-alive-forever.md`
