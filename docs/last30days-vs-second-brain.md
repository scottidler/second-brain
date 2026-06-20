# What Second-Brain Can Steal from last30days-skill

**Date:** 2026-06-16
**Context:** Deep comparison of `mvanhorn/last30days-skill` (Python, multi-source search engine) vs `scottidler/second-brain` (Rust, borg ingestion + cortex governance). Both systems ingest content from multiple sources into a knowledge store, but they solve different halves of the problem. last30days searches broadly and scores aggressively; second-brain ingests deliberately and governs carefully.

---

## The Systems at a Glance

| Dimension | last30days | second-brain |
|-----------|-----------|--------------|
| Language | Python 3.12 (zero deps, stdlib only) | Rust workspace (6 crates) |
| Purpose | Search → score → synthesize → brief | Ingest → classify → govern → query |
| Sources | 17+ (Reddit, X, YT, TikTok, HN, Polymarket, GitHub, web…) | 7 transports (Telegram, Discord, ntfy, HTTP, clipboard, CLI, Signal) |
| Output | Markdown/HTML briefs, JSON findings | Obsidian vault notes with structured frontmatter |
| Storage | SQLite (findings, sightings, FTS5) | Markdown files + SQLite (receipts) + markdown ledger |
| Scoring | RRF fusion + LLM reranking + engagement signals | Quality scoring (word count, structure, metadata completeness) |
| Dedup | URL normalization + finding_sightings re-sighting counter | Inflight guard + ledger lookup + content hash (cortex) |
| Trend detection | Delta computation (new/continued/dropped across runs) | None currently |
| Synthesis | LLM judge with entity grounding + fun judge | LLM intel (daily digest, weekly review) via Fabric |

---

## Ideas Worth Stealing

### 1. Weighted Reciprocal Rank Fusion for Cortex Classification

**What last30days does:** When multiple sources return results for the same topic, it uses Reciprocal Rank Fusion (RRF) with per-source weights to merge ranked lists into a single scored ranking. Formula: `score = weight / (K + rank)` where K=60.

**What second-brain could do:** Cortex's Tier-1a classification uses a tag-to-domain map and counts matches, rejecting ties. When multiple domains have equal signal strength, it falls through to LLM (Tier 2) unnecessarily. Instead, apply weighted scoring where different signal types (tags, URL patterns, title keywords, similar-note FTS5 hits) each contribute a weighted score. Only fall to LLM when the deterministic score gap is below a confidence threshold.

**Concretely:** Replace the simple match-count in `classify` with:
```
score = 0.40 × tag_match_score + 0.25 × url_pattern_score + 0.20 × title_keyword_score + 0.15 × similar_note_domain_score
```
This would resolve most tie-breaks deterministically, saving LLM calls and latency.

---

### 2. Cross-Source Cluster Merging for Dedup

**What last30days does:** Two-pass clustering — first text-similarity (ngram-Jaccard + token-Jaccard, threshold ~0.45), then entity-overlap merging (overlap coefficient, threshold 0.45) across different source types. Representative selection via MMR (Maximal Marginal Relevance).

**What second-brain could do:** Cortex's duplicate detection is exact content hash only (same-type constraint). This misses near-duplicates — the same YouTube video ingested as both a transcript summary and a linked article, or two articles covering the same announcement with different framing. Cortex could add a second dedup pass using token-Jaccard similarity on note bodies (or just summaries) to detect near-dupes across types, flagging them as `cortex-related-cluster` rather than hard duplicates.

**Concretely:** After the existing hash-based dedup, run a lightweight similarity pass:
- Extract significant tokens from each note's summary (proper nouns, technical terms, 4+ char words)
- Compute pairwise overlap coefficient within a sliding time window (e.g., notes ingested within 48h of each other)
- Cluster notes above threshold, set `cortex-cluster-group` frontmatter
- Surface clusters in the daily digest as "Related ingestions you might want to merge"

---

### 3. Engagement Signals as a Quality Input

**What last30days does:** Per-source engagement scoring using `log1p()` normalization:
- Reddit: 50% score + 35% comments + 5% upvote_ratio + 10% top_comment
- YouTube: 45% views + 32% likes + 13% comments
- GitHub: stars, forks, recent commit velocity

**What second-brain could do:** Borg already has some of this data (cortex writes `cortex-repo-stars`, `cortex-repo-last-commit` for GitHub repos) but doesn't use it systematically. The quality scoring module could incorporate engagement signals fetched at ingest time:
- YouTube view count + like count (from yt-dlp metadata, already fetched)
- GitHub stars + fork count + last commit recency (already partially there)
- Reddit upvote count (if the source URL is a Reddit thread)

This would let quality scoring distinguish between "a random blog post nobody read" and "a highly-engaged discussion" — feeding into the daily digest's decision of what's worth surfacing.

**Concretely:** Add `engagement-score` to frontmatter during distillation, computed from whatever platform metadata is available. Cortex quality module uses it as a signal alongside structural quality.

---

### 4. Watchlist / Trend Monitoring Pattern

**What last30days does:** SQLite store (`findings` + `finding_sightings` tables) that tracks:
- URL-based dedup with re-sighting counters
- Delta computation: compare latest two runs → classify findings as new/continued/dropped
- Daily/weekly briefings aggregating across tracked topics
- Cost tracking with daily budget enforcement

**What second-brain could do:** This is the biggest gap. Borg ingests what you send it, cortex classifies what lands. But neither system tracks *trends over time* — which topics are heating up, which are cooling down, what's new in your areas of interest.

A `sb watch` subcommand could:
1. Maintain a watchlist of topics/domains/tags you care about
2. Periodically query the vault (via oracle's FTS5) for new notes matching each topic
3. Compute deltas: new notes this period vs last period, engagement trends
4. Feed into the daily digest with a "Trending in your vault" section
5. Alert on anomalies: sudden spike in a domain, a creator you follow publishing heavily

**Concretely:** New table in receipts.db (or a separate `watch.db`):
```sql
CREATE TABLE watch_topics (id, query, domain_filter, tag_filter, schedule);
CREATE TABLE watch_snapshots (topic_id, run_at, note_count, avg_quality, top_note_path);
```
Delta = diff between consecutive snapshots. Surface in cortex intel.

---

### 5. Pre-Research / Entity Resolution Before Ingestion

**What last30days does:** Before searching, it resolves the topic into concrete handles, subreddits, GitHub repos, and hashtags. "OpenClaw" becomes `@steipete` on X, `openclaw/openclaw` on GitHub, `r/openclaw` on Reddit. Bidirectional: person → company, product → founder.

**What second-brain could do:** When you send Borg a URL, it ingests that single URL. But last30days's approach suggests a richer pattern: **fan-out ingestion**. When you send a GitHub repo URL, Borg could automatically:
- Discover the author's other notable repos
- Find related HN/Reddit discussion threads about the project
- Locate the author's blog or recent talks

This is the difference between "I saved this link" and "I captured this topic." The vault would go from a collection of individual bookmarks to a knowledge graph that automatically fills in context around what you deliberately capture.

**Concretely:** A new borg mode — `sb borg ingest --deep <url>` that:
1. Ingests the primary URL as normal
2. Runs lightweight entity resolution (who made this? what community discusses it?)
3. Queues 2-5 related URLs for ingestion with `method: auto-discover`, `origin: assisted`
4. Links them via `cortex-cluster-group` or explicit wikilinks

Gate it behind a flag so casual ingestion stays fast. The deep mode is for "I want to really understand this."

---

### 6. Fun Judge → Interest/Relevance Judge for Digest Curation

**What last30days does:** Separate LLM pass scoring humor/cleverness/shareability (0-100), used to surface entertaining content in briefs. Fallback heuristic: shortness bonus, engagement markers ("lol", "bruh", "ratio").

**What second-brain could do:** Cortex's daily intel digest treats all notes equally. A lightweight "interest scoring" pass (not humor — *relevance to Scott's active projects and interests*) could surface the most actionable items first. This could be deterministic:
- Notes matching active project tags (e.g., `rust`, `claude`, `mcp`) score higher
- Notes with high engagement scores surface first
- Notes that connect to multiple existing vault notes (high wikilink density) score higher
- Notes from creators Scott has starred or engaged with before score higher

**Concretely:** Add a `cortex-interest-score` computed during quality pass. Use it to rank items in the daily digest. No LLM needed — pure signal aggregation.

---

### 7. Quality Nudge / Degradation Detection

**What last30days does:** `quality_nudge.py` detects degraded conditions: stale yt-dlp (transcript extraction fails silently), missing X auth (posts not found), Instagram silent failures. Surfaces warnings in stderr and the engine footer.

**What second-brain already does (partially):** Borg just shipped `degraded_24h` tracking in receipts and `sb doctor` now has a live Fabric probe. But the pattern could be richer:
- Track degradation *per distiller* (article extractor failing more than video?)
- Track quality score trends over time (are recent notes lower quality than last month's?)
- Correlate with Fabric model changes (did a model retirement cause the drop?)

**Concretely:** Add a `degraded_by_kind` breakdown to health audit, and a weekly quality trend to cortex intel.

---

### 8. SQLite FTS5 for Finding Storage (Not Just Search)

**What last30days does:** SQLite with FTS5 as the primary data store for findings. Schema: topics → research_runs → findings (URL-unique) → finding_sightings. BM25 ranking. WAL mode.

**What second-brain already does:** Oracle has an FTS5 index, receipts.db tracks ingestion state. But the *findings* concept — a URL-keyed, re-sightable, scored entity — doesn't exist. Every ingestion is a single event that produces a single note.

**What could change:** If a URL is sent multiple times from different sources (Telegram, then Discord, then appears in a newsletter), borg could track re-sighting frequency as a signal. "This URL keeps coming up" is a strong quality/relevance signal that's currently lost.

**Concretely:** Add `sighting_count` and `first_seen` / `last_seen` to receipts.db. On re-ingest of a known URL (currently just overwrites), increment the counter and optionally bump the note's interest score.

---

## What NOT to Steal

- **Zero-dependency Python pattern:** Admirable for distribution, but second-brain is a Rust workspace with proper dependency management. Not applicable.
- **SKILL.md contract / LAW system:** This is about controlling LLM output format for multi-harness compatibility. second-brain's Fabric patterns serve the same purpose differently.
- **Thread-pool parallel source search:** Borg's ingestion is already async with semaphore-controlled concurrency. The patterns are equivalent.
- **macOS Keychain integration:** second-brain uses `manifest age decrypt` for secrets. Different pattern, equally good.

---

## Priority Ranking

| # | Idea | Effort | Impact | Notes |
|---|------|--------|--------|-------|
| 1 | Watchlist / trend monitoring | Medium | High | Biggest capability gap. Transforms vault from archive → living intelligence |
| 2 | Near-duplicate clustering | Low | Medium | Token-Jaccard on summaries, small cortex addition |
| 3 | Engagement signals in quality | Low | Medium | Data already partially available, wire it through |
| 4 | Weighted classification scoring | Low | Medium | Reduces unnecessary LLM calls in cortex classify |
| 5 | Fan-out deep ingestion | High | High | Major new borg capability, but architecturally clean |
| 6 | Interest scoring for digests | Low | Medium | Deterministic, no LLM cost |
| 7 | Re-sighting frequency tracking | Low | Low-Med | Simple receipts.db schema change |
| 8 | Per-distiller degradation tracking | Low | Low | Extends existing health audit |
