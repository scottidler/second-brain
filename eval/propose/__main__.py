"""`python -m propose --pattern distill-video [--sample N] [--budget N]`

GEPA over one distill pattern. sb splits staged traces into two disjoint
samples; GEPA searches on the train sample, every score coming from
`sb borg eval --candidate-pattern` (quote grounding x claim support x listicle
survival, 0 under the coverage floor); fabric proposes each rewrite. The best
candidate, if it is not the current pattern, is re-scored on both samples by
sb, which writes the candidate file. Nothing touches borg/patterns: that is
`sb borg eval --apply-candidate`.
"""

import argparse
import os
import sys
import tempfile
import time
from pathlib import Path

import gepa

from propose.adapter import PatternAdapter
from propose.reflect import FabricProposer
from propose.sb import Sb

REPO_PATTERNS = Path(__file__).resolve().parents[2] / "borg" / "patterns"


def parse(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="propose", description=__doc__)
    parser.add_argument("--pattern", required=True, help="the distill pattern to search, e.g. distill-video")
    parser.add_argument("--sample", type=int, default=4, help="traces per sample (train and holdout)")
    parser.add_argument("--budget", type=int, default=40, help="GEPA max metric calls (one per trace scored)")
    parser.add_argument("--minibatch", type=int, default=3, help="traces per reflection minibatch")
    parser.add_argument("--reflection-model", default="gemini-2.5-pro", help="fabric -m for the rewrite step")
    parser.add_argument("--fabric", default="fabric")
    parser.add_argument("--sb", default=os.getenv("SB", "sb"), help="the sb binary (env SB)")
    parser.add_argument("--sb-config", default=os.getenv("SB_BORG_CONFIG"), help="a borg.yml (env SB_BORG_CONFIG)")
    parser.add_argument("--patterns-dir", type=Path, default=REPO_PATTERNS, help="where the current pattern is read")
    parser.add_argument("--work-dir", type=Path, help="candidate patterns and GEPA state (default: a temp dir)")
    parser.add_argument("--out", type=Path, help="candidate file dir (default: sb's eval-reports/candidates)")
    parser.add_argument("--seed", type=int, default=0)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse(argv)
    work = args.work_dir or Path(tempfile.mkdtemp(prefix="propose-"))
    work.mkdir(parents=True, exist_ok=True)
    sb = Sb(binary=args.sb, config=args.sb_config, patterns_dir=args.patterns_dir.resolve())
    started = time.monotonic()

    split = sb.split(args.pattern, args.sample)
    train, holdout = split["train"], split["holdout"]
    print(f"split: {split['eligible']} eligible {split['kind']} traces; train {train}; holdout {holdout}", flush=True)
    seed = (args.patterns_dir / f"{args.pattern}.md").read_text()
    adapter = PatternAdapter(sb, args.pattern, work)
    proposer = FabricProposer(model=args.reflection_model, fabric=args.fabric)
    result = gepa.optimize(
        seed_candidate={args.pattern: seed},
        trainset=train,
        valset=train,
        adapter=adapter,
        custom_candidate_proposer=proposer,
        reflection_minibatch_size=min(args.minibatch, len(train)),
        max_metric_calls=args.budget,
        run_dir=str(work / "gepa"),
        seed=args.seed,
        display_progress_bar=False,
        raise_on_exception=True,
    )
    for i, (cand, val) in enumerate(zip(result.candidates, result.val_aggregate_scores, strict=True)):
        mark = "*" if i == result.best_idx else " "
        print(f"{mark} candidate {i}: train mean {val:.4f}  {adapter.pattern_file(cand[args.pattern]).name}")
    print(
        f"metric calls: {result.total_metric_calls}  rewrites proposed: {proposer.proposed}  "
        f"elapsed {time.monotonic() - started:.0f}s",
        flush=True,
    )

    # GEPA swallows a failing reflection step and carries on, so a search
    # that never got one rewrite back is a failure, not "nothing better".
    if proposer.proposed == 0:
        print("no rewrite was ever proposed: the reflection step failed every time (see stderr)", file=sys.stderr)
        return 1
    best = result.best_candidate[args.pattern]
    if best == seed:
        print(f"none of {proposer.proposed} rewrites beat the current pattern on the train sample; nothing written")
        return 0
    written = sb.write_candidate(args.pattern, adapter.pattern_file(best), train, holdout, args.out)
    print(f"candidate file: {written['file']}  elapsed {time.monotonic() - started:.0f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
