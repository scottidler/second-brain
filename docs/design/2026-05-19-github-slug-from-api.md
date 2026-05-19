# Design Document: borg github-URL slug from owner/repo, not scraped HTML title

**Author:** Scott Idler
**Date:** 2026-05-19
**Status:** Implemented
**Review Passes Completed:** 5/5 + 1 review-driven amendment

## Summary

For `github.com/<owner>/<repo>` URLs, borg currently derives the note's filename slug and `title:` frontmatter from the scraped HTML `<title>`. Auth-walled responses collapse that title to GitHub's generic login-page string, so distinct repos slug to the same filename and clobber each other. The fix has three coupled parts that ship together:

1. **Slug override:** in the pipeline, when the URL is a github repo root, shadow `title` with the `owner/repo` pair already produced by `parse_repo_url` before the title flows into `sanitize_filename` and frontmatter rendering. The slug becomes deterministic from URL identity, regardless of what the fetcher saw.
2. **Quality-gate hardening:** add the GitHub auth-wall sentinel (`"search code, repositories, users, issues, pull requests"`) to `BLOCKED_TITLE_INDICATORS` in `borg/src/quality.rs`, and pass the *scraped* title (not the override) to `detect_blocked_content`. Without this, the override would hide the only signal the gate currently has that the fetcher returned garbage - turning visible filename collisions into silent corruption.
3. **Gist host-check tightening:** narrow `parse_repo_url`'s host check from `host.ends_with(".github.com")` to an explicit allowlist (`host == "github.com" || host == "www.github.com"`), so `gist.github.com/owner/hash`, `api.github.com/...`, and other GitHub subdomains no longer mis-route into the repo distiller. The allowlist preserves the pre-existing `www.github.com` case (already test-covered) while closing the gist gap.

## Problem Statement

### Background

When borg ingests a github repo URL, the pipeline runs two roughly-independent operations:

1. **Article fetch** (`process_article_fabric` / `process_article_jina`) returns `(title, article_md)`. `title` is scraped from HTML `<title>`; `article_md` is the rendered page body.
2. **Repo distill** (`distill_for_publish_repo`) calls the GitHub REST API (`GET /repos/{owner}/{repo}` + `/readme`) and returns a `Distilled` whose `kind_specific` carries `RepoPayload { stars, primary_language, last_commit, topics, install }`. The owner/repo identifiers themselves live in the borg-local `RepoMetadata` struct inside `fetch_repo`'s `RepoFetch` envelope and are dropped at the distiller boundary.

`title` from step 1 then drives two outputs:
- **Filename slug:** `hygiene::sanitize_filename(&title)` produces the on-disk path (`{slug}.md`).
- **Frontmatter `title:`:** rendered verbatim into the note's YAML header.

Bug site: `borg/src/pipeline.rs:490-540`.

### Problem

On 2026-05-19 two distinct github URLs were reingested:
- `https://github.com/coleam00/archon`
- `https://github.com/matt1398/claude-devtools`

Both came back from the article fetcher with the title **"Search code, repositories, users, issues, pull requests..."** - GitHub's auth-wall page title. Both slugged to the same filename `notes/search-code-repositories-users-issues-pull-requests.md`. The second ingest clobbered the first. Archon's note is lost; the on-disk file contains only the claude-devtools content. Both ledger rows show success (`✅`) for traces `cl-62e3a1` and `cl-a1849f`.

The article fetcher is doing its job: the page it was given really did have that title. The bug is that the pipeline keeps using that title for slug + frontmatter even though, by the time the distiller runs, we have authoritative `(owner, repo)` from `parse_repo_url`.

### Goals

- Filenames for github repo URLs are derived from canonical `owner/repo`, not from HTML title scraping.
- The fix applies whether or not the GitHub API call succeeds (works even for private repos, 404s, and rate-limited fetches).
- The note's `title:` frontmatter likewise reflects `owner/repo` rather than the auth-wall title.
- Distinct github repos never collide on a single filename slug.
- The quality gate rejects auth-walled scrapes regardless of whether the slug-override fired, so a failed API fetch combined with an auth-walled article scrape bails the ingest rather than silently publishing the login page under a clean filename.
- Gist URLs (`gist.github.com/...`) no longer route through the repo distiller; they fall through to the article path where they belong.

### Non-Goals

- Fixing slug derivation for non-repo-root github URLs (issues, PRs, files deep in a tree, org pages). Those go through the generic article path. The quality-gate hardening in Goal 5 catches auth-walled responses for those URL kinds too, so the regression vector closes even though we don't invent synthetic titles for them.
- Repointing existing notes in the vault with bad slugs - this is a forward-looking fix. Data recovery for the two known-clobbered notes is a separate manual step recorded in the rollout plan.
- Changing the GitHub fetcher's auth behavior, rate-limit handling, or transcript rendering. Those are independently tracked (`project-github-fetcher-rate-limit`).
- Adding `owner` / `repo` fields to `vault::distilled::RepoPayload`. The fix uses `parse_repo_url` at the pipeline boundary, which keeps the vault schema unchanged.
- Re-routing gist URLs to a dedicated gist distiller. They fall through to the article path; designing a real gist distiller is a separate initiative.

## Proposed Solution

### Overview

Three coupled changes across three files:

**1. Pipeline-layer slug override (`borg/src/pipeline.rs`).** Call `crate::github::parse_repo_url(&url_match.url)` once at the top of the non-YouTube branch. Bind the article fetcher's return as `scraped_title` (not `title`). If `parse_repo_url` returns `Some((owner, repo))`, set `title = format!("{owner}/{repo}")`; otherwise `title = scraped_title.clone()`. The downstream `sanitize_filename` and frontmatter rendering use `title`. The quality gate at `pipeline.rs:545` is amended to receive `scraped_title` (not `title`), so the gate still sees what the fetcher actually got.

The override lives at the **pipeline layer**, not the fetcher layer, because:

- The article fetcher's job is to return what the page actually said. Forcing it to second-guess HTML titles based on URL identity would couple a generic fetcher to GitHub-specific knowledge.
- The pipeline is the layer that already routes by URL kind (github / thread / article / video) and already knows it's looking at a github repo before it dispatches the distiller.
- The same canonical name (`parse_repo_url`'s output) is needed twice - once to gate the distiller branch, once to derive the title - so computing it at the pipeline layer reuses one call rather than duplicating URL parsing across modules.

The article fetch still runs and its `article_md` continues to feed `distill_for_publish_repo` as a fallback (`article_md_fallback` param) for the case where the GitHub REST API call fails. Only the `title` half of the article fetcher's return tuple is discarded for github URLs - and even then, the original is preserved as `scraped_title` for the quality gate's benefit.

**2. Quality-gate hardening (`borg/src/quality.rs`).** Add the auth-wall sentinel to `BLOCKED_TITLE_INDICATORS`:

```rust
const BLOCKED_TITLE_INDICATORS: &[&str] = &[
    "just a moment",
    "attention required",
    "access denied",
    "one more step",
    "please verify you are a human",
    "search code, repositories, users, issues, pull requests", // NEW
];
```

Combined with passing `scraped_title` to `detect_blocked_content`, this closes the silent-corruption path: if the article fetcher hits the auth wall AND the GitHub REST API call fails (so distillation falls back to the auth-wall body), the gate now bails the ingest rather than publishing the login page under a clean filename. This change is independently correct - it benefits all github URL kinds, including the issue/PR/deep-file URLs that remain out of scope for the slug override.

**3. Gist host-check tightening (`borg/src/github.rs`).** Change `parse_repo_url`'s host check from `host != "github.com" && !host.ends_with(".github.com")` to an explicit allowlist `host != "github.com" && host != "www.github.com"`. Gist URLs (`gist.github.com/owner/hash`) currently mis-route into the repo distiller, where the REST API call to `/repos/owner/hash` definitively 404s and the pipeline falls back to a gist-body article under a structured-looking but inaccurate `owner-hash` filename. Tightening the host check sends gists down the regular article path. No other `.github.com` subdomain currently has a repo-distiller code path that depends on the loose check.

### Architecture

No new modules. Three small in-place edits:
- `borg/src/pipeline.rs`: hoist the existing `parse_repo_url` call out of the distiller-dispatch `if` (currently `pipeline.rs:507`); add a `scraped_title` binding; rebind `title` based on `github_repo`; pass `scraped_title` to the quality gate at `pipeline.rs:545`.
- `borg/src/quality.rs`: extend `BLOCKED_TITLE_INDICATORS` with the auth-wall sentinel.
- `borg/src/github.rs`: tighten `parse_repo_url`'s host check.

### Data Model

No schema changes. `RepoPayload` in `vault::distilled` is unchanged; owner/repo continue to live only in borg-local code (`borg::github::RepoMetadata` and the function-local binding in the pipeline). This keeps the design free of vault-crate migrations and avoids touching the L2 Distilled contract.

### API Design

No public API change. The change is internal to `borg::pipeline::ingest_url`.

Sketch of the diff:

```rust
let (scraped_title, article_md) = if use_fabric {
    match process_article_fabric(&url_match.url, config, trace_id).await {
        Ok((title, article_md, _)) => (title, article_md),
        Err(e) => {
            log::warn!("Fabric article fetch failed: {e:#}, falling back to Jina");
            let (title, article_md, _) = process_article_jina(&url_match.url, config, trace_id).await?;
            (title, article_md)
        }
    }
} else {
    let (title, article_md, _) = process_article_jina(&url_match.url, config, trace_id).await?;
    (title, article_md)
};

// For github repo URLs, the HTML <title> is unreliable (auth-walled pages
// collapse to a generic login title). The URL itself is the canonical
// name, so override. `scraped_title` is kept separate so the quality gate
// below still sees what the fetcher actually returned.
let github_repo = crate::github::parse_repo_url(&url_match.url);
let title = match &github_repo {
    Some((owner, repo)) => format!("{owner}/{repo}"),
    None => scraped_title.clone(),
};

let distilled = if github_repo.is_some() {
    crate::stages::distill::distill_for_publish_repo(
        &config.fabric, &config.staging, trace_id, &url_match.url, &article_md,
    ).await
} else if crate::stages::raw::is_thread_url(&url_match.url) {
    // unchanged
    ...
} else {
    // unchanged
    ...
};

// ... later, at pipeline.rs:545 (quality gate):
// Pass `scraped_title`, not `title` - we want the gate to see what the
// fetcher actually returned, not the override.
if let Some(reason) = crate::quality::detect_blocked_content(&distilled.summary, &scraped_title) {
    eyre::bail!("Content quality check failed: {reason}");
}
```

`sanitize_filename("owner/repo")` produces `"owner-repo"` (slash is treated as a separator and replaced with `-`; existing behavior, covered by `vault::hygiene` tests).

### Implementation Plan

#### Phase 1: Fix + tests + ship
**Model:** sonnet

Pipeline (`borg/src/pipeline.rs`):
- Rename the existing `title` binding from the article fetch to `scraped_title`.
- Hoist `parse_repo_url` to a `let github_repo = ...` binding immediately after the article fetch.
- Set `title` from `github_repo`: `Some((owner, repo))` -> `format!("{owner}/{repo}")`; `None` -> `scraped_title.clone()`.
- Use `github_repo.is_some()` (not a second `parse_repo_url` call) to gate the distiller branch.
- At the quality gate (`pipeline.rs:545`), pass `&scraped_title` instead of `&title` to `detect_blocked_content`. The gate's job is to see what the fetcher actually returned, not the override.
- Update the inline comment near the dispatch to note that the slug is derived from URL identity, not scraped title.
- Keep the override inline (two lines of code don't justify a helper crate-surface).

Quality gate (`borg/src/quality.rs`):
- Append `"search code, repositories, users, issues, pull requests"` (lowercase, contains-match) to `BLOCKED_TITLE_INDICATORS`.
- Add a unit test asserting `detect_blocked_content("...auth wall body...", "Search code, repositories, users, issues, pull requests · GitHub")` returns `Some` with a reason mentioning the title indicator.

URL parser (`borg/src/github.rs`):
- Change the host check in `parse_repo_url` from `if host != "github.com" && !host.ends_with(".github.com")` to `if host != "github.com"`.
- Add a unit test in `borg/src/github/tests.rs` asserting `parse_repo_url("https://gist.github.com/owner/abc123").is_none()`.

Existing test surface:
- Add a unit test for `sanitize_filename("coleam00/archon") == "coleam00-archon"` next to the existing `sanitize_filename` tests in `vault/src/hygiene.rs` (the inline `#[cfg(test)] mod tests` block at the bottom of the file; this file predates the project's "tests in their own file" convention - keeping the new test adjacent to the existing ones rather than extracting the whole block is the right scope for this fix).
- Add a unit test in `borg/src/github/tests.rs` confirming `parse_repo_url("https://github.com/coleam00/archon")` returns `Some(("coleam00".into(), "archon".into()))` if not already covered.

End-to-end:
- The pipeline-level title override itself is exercised by the verification step in Phase 2 (manual reingest produces the expected filenames). `pipeline::ingest_url` is async, depends on Fabric/Jina/network, and lacks a unit-test seam today; adding one is out of scope for this fix.
- `otto ci`.
- Ship via the `shipit` skill (commit + bump + push + install). No daemon restart needed for borg's CLI ingest path beyond `otto deploy` (which restarts borg automatically).

#### Phase 2: Data recovery
**Model:** sonnet

- `rkvr rmrf ~/repos/scottidler/obsidian/notes/search-code-repositories-users-issues-pull-requests.md` (the clobbered note).
- `borg ingest --force https://github.com/coleam00/archon`
- `borg ingest --force https://github.com/matt1398/claude-devtools`
- Verify each lands as `notes/coleam00-archon-*.md` and `notes/matt1398-claude-devtools-*.md` with `title: coleam00/archon` and `title: matt1398/claude-devtools` respectively in frontmatter.
- Commit + push the vault.

## Alternatives Considered

### Alternative 1: Add `owner`/`repo` to `vault::distilled::RepoPayload`, derive title from `Distilled`

- **Description:** Extend `RepoPayload` with `owner: String` and `repo: String`. Plumb them through the repo distiller. In `pipeline.rs`, after `distill_for_publish_repo` returns, override `title` from `distilled.kind_specific`.
- **Pros:** Title derivation lives next to the rest of the distilled metadata; future readers of `RepoPayload` see the full identity.
- **Cons:** Touches the vault crate schema, requires a migration for any persisted `distilled.yml` files in staging that lack the fields. The fallback path (`fallback_distilled`) doesn't have owner/repo unless we plumb them through separately - so we'd end up with the same conditional logic anyway, just located inside the distiller. The owner/repo are already free at the pipeline boundary from `parse_repo_url`; adding schema fields to carry them downstream is unnecessary plumbing.
- **Why not chosen:** Higher blast radius for the same outcome. The pipeline-local shadow is strictly smaller and keeps the vault schema stable.

### Alternative 2: Override title only when scraped title matches a known-bad pattern

- **Description:** Keep the scraped title by default, but if it matches `"Search code, repositories, users, issues, pull requests..."` (the GitHub auth-wall sentinel), substitute `owner/repo`.
- **Pros:** Narrower behavior change. Keeps useful scraped titles like `"GitHub - owner/repo: tagline"` (when the fetcher does see the real page).
- **Cons:** Brittle - GitHub can change the auth-wall title at any time; pattern-matching it permanently couples our slug behavior to GitHub's marketing copy. Doesn't solve the *root* problem (we already have a canonical name we're discarding). Multi-language variants of the auth wall would not match the English sentinel. Repo collisions become "rarer" but not "impossible."
- **Why not chosen:** The whole point is that the URL is more authoritative than scraped HTML for naming purposes; pattern-matching is a workaround for not trusting the URL.

### Alternative 3: Use `RepoMetadata.description` to enrich the title

- **Description:** Title becomes `format!("{owner}/{repo}: {description}")` when description is non-empty.
- **Pros:** Richer titles in frontmatter; nicer to scan in Obsidian.
- **Cons:** Description comes from the GitHub API call - when the API call fails (private repo, rate-limit, network error), `distill_for_publish_repo` returns a `fallback_distilled` and we have no description. Title would then silently degrade. Inconsistent titling across success and fallback paths is a worse outcome than uniformly compact `owner/repo` titles. The description is already preserved in the rendered body and in `cortex-repo-*` frontmatter additions, so the data isn't lost.
- **Why not chosen:** Determinism wins. The description belongs in the body, not the title.

## Technical Considerations

### Dependencies

None added. Uses existing `crate::github::parse_repo_url`, existing `vault::hygiene::sanitize_filename`.

### Performance

`parse_repo_url` is already called in the current code (inside the distiller-dispatch `if`). The change hoists it one scope outward and uses its result twice. No additional API calls, no additional allocations beyond a `String::from(format!(...))` for the new title.

### Security

None. Owner/repo strings come from a user-supplied URL that has already been parsed by `url::Url`; `sanitize_filename` strips anything that isn't `[a-z0-9-]`.

### Testing Strategy

- Existing `vault::hygiene::sanitize_filename` tests already cover slash-to-hyphen and basic slugging.
- Existing `crate::github::parse_repo_url` tests cover the URL parsing.
- New: a unit test asserting that for a github repo URL, the title used downstream is `owner/repo` regardless of what the article fetcher returned. The cleanest surface is to extract a tiny helper `fn canonical_title(url: &str, scraped: String) -> String` that returns `format!("{o}/{r}")` if `parse_repo_url` succeeds else `scraped`, and unit-test that.
- Manual verification: reingest both clobbered URLs and confirm distinct filenames.

### Rollout Plan

Direct ship to `main`:
1. Write the design doc (this file).
2. Run `/architect` review (per project convention).
3. Implement Phase 1 (fix + tests).
4. `otto ci`.
5. `shipit` skill (commit, bump patch, push, install, `otto deploy`).
6. Phase 2 data recovery (delete clobbered note + reingest both URLs + commit vault).

No phased rollout, no feature flag, no soak time (per `feedback-no-phase-gating`).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `sanitize_filename("owner/repo")` produces an unexpected slug | Very Low | Med | Add an explicit unit test for the `owner/repo` case before shipping; existing tests already cover slash handling. |
| Some downstream consumer reads `title:` and expects HTML-derived prose | Very Low | Low | `title:` is only consumed by Obsidian for display + by oracle search for BM25. Both work fine with `owner/repo`. |
| Existing notes with bad slugs (beyond the two known cases) are still on disk | Low | Low | This fix is forward-looking. A separate `cortex` audit could find slugged-as-auth-wall notes and propose renames; out of scope here. |
| User reingests a github URL that hits a 404 (typo, deleted repo) | Low | Low | `parse_repo_url` still succeeds on a well-formed URL; title becomes `owner/repo` even when the API fetch fails. The note still publishes via the `fallback_distilled` path with a sensible filename. If the article fetcher *also* hit the auth wall, the quality gate now bails (Goal 5). |
| Article fetcher *does* get past the auth wall (fabric session present) and returns a high-quality HTML title like `"GitHub - coleam00/archon: Build agents with ease"`. The fix discards that in favor of compact `owner/repo`. | Medium (depends on fabric auth state) | Low | Intentional tradeoff: determinism beats variable scrape quality, and the discarded tagline is already preserved in the rendered body and `cortex-repo-*` frontmatter additions. The user can hand-edit `title:` if they want the tagline back; reingest will overwrite that edit, but that's the existing reingest contract, not a regression. |
| Silent data corruption on the API-failure fallback path: GitHub REST API fails, distillation falls back to `article_md`, but `article_md` is the auth-wall body. With only the slug override, the note would publish as login-page content under a clean `owner-repo.md` filename. | Was: Med / High (would silently fill the vault with login pages). Now: addressed. | Was: High. Now: gate-bailed. | `BLOCKED_TITLE_INDICATORS` augmented with the auth-wall sentinel + the gate is fed `scraped_title` rather than the override. Combined, this bails the ingest on the failure path instead of publishing. Raised by Architect round-1 review. |
| `gist.github.com/owner/gist_id` URLs mis-classified as repo URLs by `parse_repo_url`'s loose host check. | Was: Low (gist fallback persists garbage under structured filename). Now: closed. | Was: Med. Now: gone. | Host check tightened to `host == "github.com"` exactly in the same fix. Gist URLs fall through to the article path. Raised by Architect round-1 review. |

## Open Questions

- [ ] Should we audit the vault for other notes with bad slugs (e.g., a `cortex` command that scans for `title:` matching known auth-wall sentinels)? Out of scope for this fix; tracked separately if the manual recovery turns up more than the two known cases.
- [ ] **Resolved (Architect round 1):** Tightening `parse_repo_url`'s host check is in scope and folded into this fix.
- [ ] **Resolved (Architect round 1):** Silent corruption on the API-failure fallback path is addressed by extending `BLOCKED_TITLE_INDICATORS` and passing the scraped title to the gate.

## References

- Bug site: `borg/src/pipeline.rs:490-540`
- API source: `borg/src/github.rs:31` (`RepoMetadata` with owner/repo/description)
- URL parser: `borg/src/github.rs:63` (`parse_repo_url`)
- Slug derivation: `vault/src/hygiene.rs:112` (`sanitize_filename`)
- Vault schema (unchanged): `vault/src/distilled.rs:91` (`RepoPayload`)
- The two lost-content URLs: `https://github.com/coleam00/archon`, `https://github.com/matt1398/claude-devtools`
- The bad on-disk note: `~/repos/scottidler/obsidian/notes/search-code-repositories-users-issues-pull-requests.md`
- Memory: `feedback-design-doc-first`, `feedback-no-phase-gating`, `feedback-no-full-paths-for-installed-bins`, `feedback-no-single-use-bindings`, `reference-shipit-workflow`, `reference-otto-deploy`
