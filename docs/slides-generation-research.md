# Slides Generation: Claude Code → Beautiful Google Slides

**Date:** 2026-06-17
**Context:** Tracking the hunt for a workflow that can be triggered from a Claude Code conversation and end with a beautiful, content-rich-but-not-overstuffed **Google Slides** deck. Sibling research doc to `retrieval-methods-research.md`. Companion to the committed design `design/2026-06-08-artifact-generation-layer.md` (`sb cortex render`), which builds the native Marp path; this doc scouts the *whole* landscape, including paths that design rejected.

---

## The Bar (what "done" means)

A candidate only counts as the answer if it hits **all** of these:

1. **Triggerable from a Claude Code conversation** — not a separate browser ritual I drive by hand.
2. **Ends in real, editable Google Slides** — not HTML, not PDF, not a proprietary format I have to remember to export.
3. **Beautiful** — looks designed, not robotic-default-template.
4. **Content-rich but right-sized** — my message, the right density per slide, not a wall of text and not three vapid bullets.
5. **My content, my message** — sourced from what I actually want to say (ideally from vault notes), not generic filler.

**Anything short of all five = still searching.** Nothing below clears the bar yet.

---

## Candidate Scorecard

| Path | CC-triggerable | → real Google Slides | Beautiful | Notes |
|------|:--:|:--:|:--:|-------|
| `sb cortex render` (Marp) | ✅ (native verb) | ❌ (HTML/PDF/PPTX-ish, not GSlides) | ⚠️ theme-bound | Native, in-vault, owned. Wrong output target. |
| Gemini Canvas | ❌ (browser) | ✅ (direct export) | ✅ | Best *output* today, worst *trigger*. Human-in-loop. |
| Gamma (+ Claude connector / Cowork) | ✅ (official connector) | ❌ (PPTX-only bridge, lossy) | ✅ (in Gamma's format) | Best CC-triggerable deck *generator* found — but not a Google Slides solution. See research log. |
| NotebookLM Studio | ❌ (cloud app) | ⚠️ | ✅ | Studio is the gap `sb cortex render` was scoped against. |
| HTML deck (Claude) + Marquee | ✅ | ❌ (clickable HTML) | ✅ | Mirek's pick; great for review, not Google Slides. |
| **Google Slides MCP + branded template** | ✅ (`gslide-mcp` / Composio) | ✅ (native) | ✅ *via template + placeholder injection* | **Frontrunner.** Clears 4/5; beauty is a known recipe (pre-built master template, inject content into placeholders). See research log. |
| Canva | — | — | — | Not a real candidate — only a passing export-destination mention in vault notes. |
| Raw Claude Code → .pptx/Slides | ✅ | ⚠️ | ❌ | 3/10 useful (Andrew); struggles past ~3 slides. Needs back-and-forth (Vasily). |

---

## Path Detail

### `sb cortex render` (Marp) — *ours, native, wrong target*
The committed design (`design/2026-06-08-artifact-generation-layer.md`). Turns a vault note (or small set) into a Marp slide deck via the existing Fabric-pattern → renderer → vault-attachment machinery, landing the deck back as an oracle-indexed note. Owns the corpus, ownership, retrieval. **Gap:** output is Marp (HTML/PDF), not Google Slides. Clears trigger + content + ownership; misses the Google Slides requirement.

### Gemini Canvas — *best output, manual trigger*
The 5-step workflow (source: Lawrence Tijjani, `notes/geminis-canvas-mode-just-changed-how-i-make-presentations.md`):

1. **Open Canvas mode** in Gemini; flip on **thinking mode** before submitting — produces real slide titles/descriptions, practical over theoretical.
2. **Prompt for the deck** — literally "create a presentation," e.g. *"Act as a presentation creator and draft a comprehensive 10-slide presentation outline on <topic>."* Full draft deck, noticeably better than default templates.
3. **Add speaker notes** — microphone icon in Canvas → delivery-ready notes per slide.
4. **Export to Google Slides** — direct export (or PDF). This is the step that yields a real, editable Slides deck.
5. **Polish in Slides** — "Beautify this slide" per-slide; swap images with **Nano Banana** (Gemini image gen) in place.

**Verdict:** Best for ad-hoc one-shot decks. Browser-based → not automatable/templatizable, not CC-triggerable. Clears output + beauty; misses trigger.

### Gamma — *CC-connectable, exports to Slides, credit-gated*
From Aamir in the Slack thread:
- There's a **Claude connector for Gamma** (AI slide maker). Confirmed to work with **Claude Code** and **Claude Cowork**.
- Gamma decks are a proprietary format **but export as Google Slides**.
- Possible chain: **Claude Code + Google Drive connector** → point at a file → summarize → **Gamma connector** → generate → **manually export** to Google Slides. (Untested by Aamir.)
- **Cost:** requires a Gamma account; burns credits.

**Verdict (updated after research, see log):** Closest thing to CC-triggerable, **but not a Google Slides solution.** There is **no direct Gamma → Google Slides export** — you export `.pptx` and upload it to Slides, and that bridge is **lossy** (complex layouts flatten to uneditable images, fonts substitute to Calibri/Arial). The connector is real and excellent; the *output target* is wrong. Best for upstream content + first-draft design, not as the final Slides emitter.

### HTML deck + Marquee — *great review artifact, not Slides*
Mirek: Claude generates strong `.html` presentations you click through like Google Slides (used one for a tech-spec review). Scott's example: `https://marquee.test.tatari.dev/p/~bryce-york/marquee-setup-guide`. **Verdict:** excellent for internal review/sharing via Marquee; fails the Google Slides requirement outright.

### Google Slides MCP + branded template — *the frontrunner (build-it)*
Originally the in-thread suggestion to have Claude Code drive the [Slides API](https://developers.google.com/workspace/slides/api/quickstart/python) programmatically. Research upgraded this from "promising idea" to **frontrunner** — the CC-native tooling already exists:

- **[jemmanuele/gslide-mcp](https://github.com/jemmanuele/gslide-mcp)** — a Google Slides MCP server *explicitly tailored for Claude Code*. Deck/slide/shape/layout modules, a **template-library registry** (ingest deck IDs → assemble from them), cross-deck slide copy via Apps Script, asset/logo fetching. Desktop OAuth, `token.json` auto-refresh. **The one to try first** — directly targets the "beautiful + my content" bars.
- **[Composio Google Slides + Claude Code](https://composio.dev/toolkits/googleslides/framework/claude-code)** — hosted toolkit with a "create presentation from Markdown" tool.
- **[matteoantoci/google-slides-mcp](https://github.com/matteoantoci/google-slides-mcp)** (179★) — lower-level `create_presentation` / `batch_update_presentation` primitives.
- **[k1LoW/deck](https://github.com/k1LoW/deck)** (1.2K★, Go) — Markdown → Google Slides, *actively maintained*; edits an existing deck so it inherits that template's design. Use this over the semi-abandoned official [md2googleslides](https://github.com/googleworkspace/md2googleslides).
- **[FlashDocs](https://www.flashdocs.com/)** — commercial API/MCP that maps a Markdown AST onto template placeholders (subgraph isomorphism) → on-brand Slides or PPTX. The "beauty solved" buy-option if we don't want to build it.

**Verdict:** clears **4 of 5** bars, and the 5th has a known recipe. ✅ CC-triggerable, ✅ real editable Google Slides (native), ✅ my content (feed vault-note / `Distilled` Markdown), and **beauty ⚠️→✅ only via a pre-built branded template + placeholder injection** — raw Slides-API layout is ugly *by design* (Google says so: the API is "for prototyping," beauty comes from templates). "Right-sized density" stays a content-discipline problem, not a tool feature. This is the only path that clears bar 2 without a lossy bridge **and** stays fully owned.

### Raw Claude Code → deck — *unreliable today*
Field reports from the thread: Andrew — "maybe 3/10 times I get a useful slide flow," struggles beyond ~3 slides; Vasily — works but needs back-and-forth. Consensus in-thread: HTML/JSX outputs are superior to direct slide output.

---

## Sources

**Vault notes (ingested 2026-06-09 cluster + others):**
- `notes/geminis-canvas-mode-just-changed-how-i-make-presentations.md` — Gemini Canvas (Lawrence Tijjani)
- `notes/gemini-notebooklm-is-insane-for-ai-slides-and-free.md`, `notes/notebooklm-is-insane-for-ai-slides-its-free.md` — NotebookLM → slides
- `notes/build-slides-in-seconds-ai-agent-n8n-workflow-tutorial.md`, `notes/the-fastest-way-to-create-polished-slides-with-ai-agents.md`, `notes/why-90-of-ai-presentations-fail-do-this-instead.md` — AI slide agents/workflows
- `notes/ai-slides-agent-generates-presentations-like-a-pro-designer-using-nano-banana.md` — Kimi/Nano Banana (Canva/Gamma named only as "alternatives")
- `notes/claude-design-is-insane.md` — Claude Design (Canva listed as an export destination)

**Slack:** #ai-discuss thread, Wei Chen, 2026-06-17 — `https://tatari.slack.com/archives/C090JUZTKDX/p1781714690537619`

**Design doc:** `design/2026-06-08-artifact-generation-layer.md` (`sb cortex render`)

---

## Research Log

### 2026-06-17 — `/last30days`: Gamma → Google Slides

Window 2026-05-18 → 2026-06-17; 16 Reddit threads (2,945 upvotes), GitHub, 10 web pages (X unavailable, no auth).

- **No direct Gamma → Google Slides export.** Confirmed by Gamma's own consultant guide and multiple sources: you export `.pptx`, then upload to Google Slides. The PPTX hop is the **loudest complaint** in a 500+ Reddit-comment analysis — "the PowerPoint export is bad": complex/overlapping layouts flatten into a **single uneditable image**, custom fonts substitute (Calibri/Arial), animations drop. Root cause: Gamma renders HTML/CSS → converts to OOXML, which doesn't map 1:1. Mitigation: set "Traditional 16:9" before export to limit damage.
- **The Claude connector is real, official, and good** — Gamma is a first-class [Claude Connector](https://claude.com/connectors/gamma); there's also a community [MCP server](https://github.com/nickloveinvesting/gamma-mcpserver) (21★) and a [Composio Claude Code path](https://composio.dev/toolkits/gamma/framework/claude-code). One-conversation deck generation genuinely works (Aakash Gupta: "I built an entire slide deck without leaving Claude"). Confirms Aamir's connector claim; his "export to Google Slides" needs the asterisk above.
- **Credits are stingy.** Free = 400 **non-refilling** credits (~10 decks ever — "a trial, not a free tier"). Plus $9/mo (1,000 credits/mo); Pro $25/mo adds API access + custom fonts.
- **Net verdict:** Gamma clears bars 1, 3, 5 and clears 4 *only inside its own format*. It **fails bar 2** decisively — the beauty/editability that make it attractive **do not survive the PPTX→Slides bridge**. Gamma's value is **upstream** (content + first-draft design), not as the Google Slides emitter. This strengthens the case that the **Google Slides API generator** is the only route that clears bar 2 without a lossy bridge.

### 2026-06-17 — `/last30days`: AI/Claude Code → Google Slides (programmatic)

Window 2026-05-18 → 2026-06-17; 27 Reddit threads, 15 YouTube (3M views), GitHub, 9 web. High-signal evidence was GitHub + vendor docs, not Reddit (which skewed to off-topic AI drama).

- **Markdown/AI → real Google Slides is a solved tooling category.** Maintained adapters exist: **[k1LoW/deck](https://github.com/k1LoW/deck)** (1.2K★) beats the semi-abandoned official [md2googleslides](https://github.com/googleworkspace/md2googleslides). Both emit true *editable* Slides, not HTML.
- **A Claude-Code-native MCP already exists, purpose-built:** **[jemmanuele/gslide-mcp](https://github.com/jemmanuele/gslide-mcp)** (template library, asset fetching, "tailored for Claude Code"), plus [Composio](https://composio.dev/toolkits/googleslides/framework/claude-code) and [matteoantoci/google-slides-mcp](https://github.com/matteoantoci/google-slides-mcp) (179★). "Trigger from a Claude Code conversation → real Google Slides" is wired *today*, not theoretical.
- **Beauty is the only unsolved bar, and everyone converges on the same fix:** templates + placeholders, never raw API layout. Google itself says the Slides API is "for prototyping… does not yet produce stunningly beautiful decks." [FlashDocs](https://www.flashdocs.com/post/markdown-to-slides-how-flashdocs-transforms-text-into-presentations) productized the AST→placeholder mapping; `gslide-mcp` gives a manual template library.
- **"Right-sized density" is a content-discipline problem, not a tool feature** — the most-watched warning in the window is [Jeff Su's "Why 90% of AI Presentations Fail"](https://youtu.be/mi2vCsP1oKY): AI makes pretty slides that say nothing. That's exactly bar 4.
- **Net verdict:** this path clears **4/5** and the 5th has a recipe. It's the frontrunner and the thing to build/try.

## Open Gap / Next Experiments

**The frontrunner is now clear: a CC-triggered Google Slides MCP injecting vault content into a branded master template.** This is the only path that clears bar 2 (real editable Slides) without a lossy bridge *and* stays fully owned. Concrete next steps:

1. **Try `gslide-mcp` first.** Wire [jemmanuele/gslide-mcp](https://github.com/jemmanuele/gslide-mcp) into `.mcp.json`, build one branded Google Slides master template with named placeholders, and generate a deck from a vault note / `Distilled` contract through Claude Code. Measure: does the template survive, how good is the density, how much hand-tuning remains.
2. **Evaluate `k1LoW/deck`** as the non-MCP alternative — Markdown-in, edits an existing template deck. Simpler, scriptable, fewer moving parts than an MCP.
3. **Encode the density heuristic** ("right-sized") in the content-extraction step — this is *our* problem to solve (per Jeff Su), upstream of whichever emitter we pick. Natural home: a `sb cortex render`-style Fabric pattern that produces placeholder-keyed Markdown.
4. **Buy-vs-build check:** [FlashDocs](https://www.flashdocs.com/) already automates AST→placeholder mapping (the polish trick). Worth a spike to calibrate how much the owned path has to do.

**The shape that clears all five:** `sb cortex render`-style content extraction (our corpus, our message, right-sized) → **`gslide-mcp` template injection** (real, beautiful, editable Google Slides) → optional Nano-Banana image fill for image slots. Gamma's role shrinks to upstream first-draft content, not the emitter.
