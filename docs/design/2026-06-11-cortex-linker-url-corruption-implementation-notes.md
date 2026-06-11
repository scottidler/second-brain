# Implementation Notes: Cortex Linker URL Corruption

Running record of decisions/deviations during execution of
`2026-06-11-cortex-linker-url-corruption.md`. Append-only.

## Phase 1: Guard predicate + wiring

### Design decisions
- `inside_structure(text, start, end)` operates on the BODY slice both callers
  already scan, with offsets into that slice (`linking.rs`). `insert_first_wikilink`
  passes `body` + `mat.start()/mat.end()` (NOT content-absolute offsets), resolving
  the offset footgun the Staff Engineer flagged.
- Both `find_mention` and `insert_first_wikilink` now ITERATE occurrences and take
  the first non-structural one (was single-find).

### Deviations
- None from the corrected design.

### Tradeoffs
- HTML-tag / link-destination detection is line-scoped (not a full Markdown AST).
  A wikilink target inside a multi-line HTML block split mid-attribute would be
  missed, but real iframes/links are single-line. Chose the simple, fast scan.

### Open questions
- None.

## Phase 2: Repair migration (`bin/repair-url-wikilinks`)

### Design decisions
- The migration's `--selftest` (22-case Suite B) IS the test suite, per the
  no-Rust-migrations rule; otto ci (Rust-only) does not run it.
- DRY-RUN is the default; `--apply` is the explicit destructive verb and refuses
  on active cortex or dirty worktree (`--force` overrides).

### Deviations (design rewritten mid-implementation, driven by the live dry-run)
- **Top-level balanced-span model, not per-innermost-match.** The doc described
  iterative innermost de-link but checked structural-ness per innermost match.
  That failed `[[blog-[[langchain]]-com|blog.langchain.com]]/x` (B-R7): the inner
  `[[langchain]]` isn't adjacent to the URL signal, only the outer is. Fixed by
  computing OUTERMOST balanced `[[...]]` spans, deciding structural-ness at the
  outer span, then collapsing the whole nest.
- **Code/math spans are PROTECTED (skip), not merely "excluded."** The doc said
  "migration subset = URL/dest/tag, NOT code/math." The first live dry-run showed
  the URL heuristic was actively MODIFYING wikilinks inside code spans (design-doc
  filename lists like `` `[[dragonlance|...]].md` ``, fenced tree diagrams like
  `[[Python]]/`). Added explicit protection: file-level fenced/indented/`$$` block
  tracking + inline backtick/`$` parity → skip. This realizes the Architect's
  "don't strip code-span wikilinks" intent, which exclusion alone did not.
- **Dropped the `.tld` heuristic entirely.** It de-linked `[[readme]].md` filename
  references and overreached into the deferred prose-domain case. Real URL
  corruption carries `://`, `www.`, a `scheme:`, or a domain-like bare path.
- **Bare-path glue tightened twice from live findings:** (1) `/` or `?` after a
  span only counts as URL when the span's de-linked text is domain-like (caught a
  prose heading `In [[obsidian|Obsidian]]?`); (2) `?` further requires query
  content `\?\S` (caught a prose question `the other [[youtu-be|youtu.be]]?`). A
  bare trailing `?` is prose punctuation, not a URL query.

### Tradeoffs
- Indented-code (4-space/tab) lines are skipped wholesale — a genuine corrupted
  URL on an indented line would be left for manual review rather than risk
  stripping an authored wikilink in indented list content. Miss < corrupt.
- Bare non-scheme domains in prose followed by a space (`I use github.com daily`
  → `[[github-com|github.com]]`) are intentionally NOT repaired (prose-domain
  policy, deferred — see Open Questions).

### Open questions
- Prose-context domain wikilinks (`I use [[github-com|github.com]] daily`) are
  left as-is. Whether to also strip those, and whether `entities/` domains should
  be link targets at all, remain the doc's open policy questions.

## Phase 2 (harvest round): real-data grounding

At the user's request, added a `--harvest` (HARVEST=1) mode that buckets every
real vault wikilink by structural shape and prints representatives + counts. This
grounded the test suite in actual data and surfaced two gaps the synthetic tests
missed:

### Deviations
- **Removed the blanket indented-line skip.** A real corrupted URL lived in a
  4-space-indented list item (`    * Nix GitHub Action  https://[[github-com|...]]/...`);
  the blanket skip protected it and caused a miss. Indented list items are not
  code — prose wikilinks in them are already preserved by the structural checks,
  so the skip only lost real repairs. Fenced + inline-code/math protection stays.
- **Nested wikilinks are now repaired in EVERY context, not just URLs.** The
  harvest found broken embeds (`![[a-[[claude-code]]-b.jpg]]`) and link targets
  (`[[atavus-[[football]]|Atavus Football]]`) — nesting is always corruption
  (Obsidian can't render it). repair_line now: structural span → full de-link;
  non-structural span that CONTAINS nesting → collapse inner links but keep the
  outermost link/embed intact; plain prose wikilink → untouched.

### Tradeoffs
- Real-data test cases (`H-*`, 22 of them) assert PRESERVE shapes are byte-identical
  (the high-harm class) and REPAIR shapes hit a hand-verified expected. Expected
  outputs are verified by eye, not generated by the impl, so the tests have real
  regression power.

### Result
- Live dry-run after these changes: 1235 files / 2124 lines. The increase over the
  earlier 1207 is the newly-covered nested-prose/embed + indented-URL repairs. The
  only prose-context changes in the manifest are genuine bare-path URLs
  (`linkedin.com/in/...`, a quoted `youtube.com/watch?v=test`).

## Phase 3: Deploy + verify

### Design decisions
- Ran on `desk` (the daemon host). Stopped cortex AND borg via `systemctl --user
  stop` (not `sb cortex daemon --stop`, which is a no-op), verified both inactive,
  `otto install`ed the guarded `sb`, applied `bin/repair-url-wikilinks --apply
  --force`, then restarted both daemons on the new binary.
- Verified completeness by an idempotent re-run (0 files changed) + an independent
  grep for residual `http...[[` patterns.

### Deviations
- **Did NOT git-commit the obsidian vault repair.** The vault worktree is
  perpetually dirty from daemon churn (hundreds of unrelated M files), so a
  sweeping commit would mix the repair with daemon noise, and committing the
  user's vault is their call (no-unauthorized-git-state-changes). The repair is
  applied to disk and Syncthing propagates it; whether/when to commit the vault
  is left to the user.

### Result / residuals (left by design, surfaced for manual review)
- Apply: 1235 files / 2124 lines repaired. Idempotent re-run: 0.
- 4 residual `http...[[` matches remain, all correct:
  - 3 corrupted URLs inside ```bash fenced blocks (curl/git-clone examples) -
    PROTECTED by the code-span policy; fix by hand if copy-pasteable commands matter.
  - 1 malformed unbalanced `[[Python` (no closing `]]`) - left for manual review.

### Open questions
- Prose-domain linking and `entities/` target curation remain open (see design doc).
