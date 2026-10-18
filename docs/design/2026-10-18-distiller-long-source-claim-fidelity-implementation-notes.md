# Implementation Notes: Distiller Long-Source Claim Fidelity

## Phase 0: Prove one call beats chunking (zero committed code)

### Design decisions
- One-call arm switched by a process-level `PHASE0_ONECALL` flag in a scratch `distillers::phase0` module (`onecall`, `budget`, `cap`, `with_budget`), one arm per process. Both arms ran the same binary and the same `Dispatcher`, so the only differences are routing, the claim cap, the budget line and the configs the doc names.
- The harness is a scratch borg example (`borg/examples/phase0_distill.rs`), not `sb borg eval`. It builds the `Dispatcher` with `Dispatcher::with_configs` from ripr's live `borg.yml` (read only), wraps the fabric caller to log every call, and calls `FabricClaimJudge` directly with `max_chars` 0 and 600s. It never opens `eval-cache.db`, so isolation holds by construction, and `XDG_DATA_HOME` still pointed at a scratch dir. `--regenerate` judges golden fixtures only, so it could not take staged traces.
- `Claim budget: N` goes on the first line of the input. For sessions it sits before the `BEGIN DATA` frame: inside the frame the model is told to treat everything as data.
- The budget uses `approx_tokens(source bytes)`, the estimator production routing uses, so the 12,000-token boundary is the same in both arms.
- Videos get `VideoMetadata.duration_seconds` from the staged `kind-specific` payload, so anchor validation runs as in production. The thirds are binned on that duration.
- The coverage clause is scored one-sided (a cut), as `eval.candidate.max-coverage-cut` is defined (`borg/src/config/eval.rs:18`). One call raised claims per note by 23%.
- The served model for an empty `model` is read from the `DEFAULT_MODEL` line of `~/.config/fabric/.env` on ripr (`claude-sonnet-5`). That file was grepped for that line only.
- The judge pattern is the claim-judge Phase 0 copy with the cut-off rule deleted, the pattern whose flags picked this test set.
- Harness files live in the session scratchpad and on ripr under `~/phase0-distill/` (patch, scripts, scratch patterns, the 18 copied trace dirs, JSONL output). Nothing under `docs/`.

### Deviations
- `hv-12ae5acc` was not run on its own. Its `body.txt` is byte-identical to `hv-16d375ce`'s (same md5), so 14 sources ran, as the doc counts them.
- `cl-31951716` is an article, not "the largest video", and its staged source is undecoded PDF bytes, so it cannot test claim fidelity. I added `ht-99d3c5` (the largest staged video, 510,171 chars) as an extra one-call probe, and it carries the anchor-thirds check.
- "fabric's `DEFAULT_MODEL`" and the session model are the same model on ripr (`claude-sonnet-5`), so the two `hv-a158c300` probes test two patterns on one model, not two models.
- Session distills got no `session_metadata` or `source_url`. They only feed the session payload and the truncation marker (no staged body here is export-truncated), and both arms were treated the same.
- Logged input tokens are approximate (sent chars / 4). fabric does not report usage.
- The scratch patch touched the article, video, thread, session and repo distillers. Voicenote was left alone because the test set has no voice notes. Its pattern was still edited with the other five.

### Tradeoffs
- One table row per source and arm with r1-r5 values slash-separated, against 140 one-run rows. It carries the same per-run data at a size a reader can scan.
- Arms run one after the other at 4 in flight, against interleaving them. That keeps API rate limits from making fallbacks in one arm only, at the cost of the arms starting 21 minutes apart (17:16Z and 17:37Z).
- Core-error marks come from reading claim text with keyword pulls checked by hand against the originally flagged claims, not from judge flags alone. Judges flag claims, and a core error is one specific claim.

### Open questions
- The doc is back at Draft with Alternative 1, but 5 of the 6 core errors do not reproduce on current code in 5 chunked runs. Phase 4's "3 of 6 countable" bar would be inconclusive for Alternative 1 too. Re-pick the test set from fresh flags before building it?
- Head bias was the clear loss (late-third share 0.37 -> 0.16 on videos). Should a one-call variant carrying Phase 3's "early, middle and late" rule be measured before committing to Alternative 1?
- `kind: null` on any claim fails the whole note (`distillers/src/parse.rs:459`, `ClaimKind` with `#[serde(default)]` rejects an explicit null). It hit both paths in this run. It is a small standalone fix outside this design. File it?
- `cl-31951716` published from binary PDF bytes as its article source (`meta.input-tokens` 300,974, 0 claims, `reduce-selection-failed`). That is a fetch/extraction bug for its own ticket.
- claude-sonnet-5 as claims judge returned `empty response` on `hv-5194d942` repeatedly, and on the 2.47M-char session it continued the transcript instead of giving verdicts. `judge-claims` has no data frame for session sources. That belongs to the claim-judge doc's re-run.

### Cost
Prices, read 2026-10-18 from the live pages:
- `claude-sonnet-5`: $2 / MTok input, $10 / MTok output, flat across the 1M window (https://docs.claude.com/en/docs/about-claude/pricing). Every distill call was served by it, including fabric's `DEFAULT_MODEL`.
- `gemini-2.5-pro`: $1.25 input and $10 output per MTok for prompts at or under 200k tokens, $2.50 and $15 above, output including thinking (https://ai.google.dev/gemini-api/docs/pricing).

What is measured and what is estimated:
- Measured: call counts, the chars sent per distill call (`sent_chars`), the output chars per distill call, and the source and claim chars per judged output.
- Estimated: every token figure. fabric reports no usage, so tokens = chars / 4. Distill input adds the pattern file. Judge input = (judge pattern + source + claims) / 4 per attempt, counting failed attempts. Judge output = 4 tokens per verdict line.
- Not captured: thinking tokens. `gemini-2.5-pro` always thinks and bills it as output, so the gemini rows are a floor. The harness logged no token fields for judge calls, and judging finished before the cost ask arrived, so every judge row is derived.

| stage | calls | model | n | input tokens (est.) | output tokens (est., visible) | $ (est.) |
|---|---|---|---|---|---|---|
| smoke | distill | claude-sonnet-5 | 4 | 30,549 | 3,892 | 0.10 |
| smoke | judge | claude-sonnet-5 | 2 | 23,403 | 76 | 0.05 |
| smoke | judge | gemini-2.5-pro | 2 | 23,403 | 76 | 0.03 |
| chunked | distill | claude-sonnet-5 | 180 | 1,596,327 | 191,680 | 5.11 |
| chunked | judge | claude-sonnet-5 | 73 | 1,762,939 | 2,692 | 3.55 |
| chunked | judge | gemini-2.5-pro | 69 | 1,576,256 | 2,808 | 2.00 |
| one call | distill | claude-sonnet-5 | 70 | 1,697,065 | 127,938 | 4.67 |
| one call | judge | claude-sonnet-5 | 69 | 1,577,124 | 3,352 | 3.19 |
| one call | judge | gemini-2.5-pro | 65 | 1,477,579 | 3,468 | 1.88 |
| probes | distill | claude-sonnet-5 | 4 | 1,530,354 | 6,990 | 3.13 |
| probes | judge | claude-sonnet-5 | 3 | 1,370,586 | 96 | 2.74 |
| probes | judge | gemini-2.5-pro | 2 | 749,762 | 180 | 1.72 |
| **total** | | | 543 | 13,415,347 | 343,248 | **28.17** |
