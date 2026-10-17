"""The GEPA adapter: one text component (the pattern), evaluated by sb."""

import hashlib
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from gepa.core.adapter import EvaluationBatch

from propose.sb import Sb


def feedback_text(trace: Mapping[str, Any]) -> str:
    """What the reflection step reads about one note."""
    score = trace.get("score")
    lines = [f"score {score:.3f}" if score is not None else "score: not scored (an arm errored)"]
    lines += [f"- {f}" for f in trace.get("feedback", [])]
    lines += [f"- error: {e}" for e in trace.get("errors", [])]
    if len(lines) == 1:
        lines.append("- no finding: every claim supported, every quote verbatim, the enumeration complete")
    return "\n".join(lines)


class PatternAdapter:
    """Scores a candidate pattern on a batch of staged traces through sb.

    A trace that errored on either arm scores 0 for the search (GEPA's rule:
    never raise for one example); sb itself fails the call when every trace
    errored, and that failure propagates.
    """

    # GEPA reads this attribute; None hands proposals to the
    # `custom_candidate_proposer` (propose/reflect.py).
    propose_new_texts = None

    def __init__(self, sb: Sb, pattern: str, work_dir: Path) -> None:
        self.sb = sb
        self.pattern = pattern
        self.work_dir = work_dir

    def pattern_file(self, text: str) -> Path:
        """The candidate text on disk, named by its content."""
        digest = hashlib.sha256(text.encode()).hexdigest()[:12]
        path = self.work_dir / f"{self.pattern}-{digest}.md"
        if not path.exists():
            path.write_text(text)
        return path

    def evaluate(self, batch: list[str], candidate: dict[str, str], capture_traces: bool = False) -> EvaluationBatch:
        result = self.sb.score(self.pattern, self.pattern_file(candidate[self.pattern]), batch)
        by_trace = {t["trace"]: t for t in result["traces"]}
        missing = [trace for trace in batch if trace not in by_trace]
        if missing:
            raise RuntimeError(f"sb returned no score for {missing}")
        traces = [by_trace[trace] for trace in batch]
        scores = [t["score"] if t["score"] is not None else 0.0 for t in traces]
        return EvaluationBatch(outputs=traces, scores=scores, trajectories=traces if capture_traces else None)

    def make_reflective_dataset(
        self,
        candidate: dict[str, str],
        eval_batch: EvaluationBatch,
        components_to_update: list[str],
    ) -> Mapping[str, Sequence[Mapping[str, Any]]]:
        records = [
            {
                "Inputs": f"start of the source:\n{t.get('source-head', '')}",
                "Generated Outputs": t.get("candidate-note", ""),
                "Feedback": feedback_text(t),
            }
            for t in eval_batch.trajectories or []
        ]
        return {component: records for component in components_to_update}
