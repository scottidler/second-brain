"""MiniCheck: a small local fact-checker, one support probability per claim.

Runs on CPU. The model comes from the Liyan06 GitHub repo (the PyPI name belongs to
something else). It needs NLTK's `punkt_tab` at runtime, which is not shipped and
which `nltk.download` will not fetch here; `bin/eval-nltk` provides it through a
private `NLTK_DATA`.
"""

import os
from collections.abc import Callable
from pathlib import Path
from typing import Any

from verdict.contract import verdict

TOOL = "minicheck"
METRIC = "supported"
VERSION = "git-b58b9fa"
DEFAULT_MODEL = "roberta-large"

Scorer = Callable[[list[str], list[str]], tuple[list[int], list[float]]]


def require_nltk_data() -> None:
    """Fail loudly when `NLTK_DATA` does not hold `punkt_tab`."""
    root = os.environ.get("NLTK_DATA")
    if not root or not (Path(root) / "tokenizers" / "punkt_tab").is_dir():
        raise SystemExit(
            "minicheck needs NLTK_DATA pointing at a dir holding tokenizers/punkt_tab "
            "(run bin/eval-nltk and export the path it prints)"
        )


def load_scorer(model: str, cache_dir: Path) -> Scorer:
    """A `(docs, claims) -> (labels, probabilities)` scorer on the CPU."""
    require_nltk_data()
    from minicheck.minicheck import MiniCheck

    checker = MiniCheck(model_name=model, cache_dir=str(cache_dir))

    def score(docs: list[str], claims: list[str]) -> tuple[list[int], list[float]]:
        labels, probabilities, _, _ = checker.score(docs=docs, claims=claims)
        return [int(label) for label in labels], [float(p) for p in probabilities]

    return score


def run(record: dict[str, Any], *, model: str, scorer: Scorer) -> list[dict[str, Any]]:
    """One verdict per claim: the probability that the source supports it."""
    claims = record["claims"]
    if not claims:
        return []
    texts = [claim["text"] for claim in claims]
    labels, probabilities = scorer([record["source"]] * len(texts), texts)
    if len(labels) != len(claims) or len(probabilities) != len(claims):
        raise RuntimeError(f"minicheck scored {len(labels)} of {len(claims)} claims for {record['trace']}")
    return [
        verdict(
            record,
            tool=TOOL,
            tool_version=VERSION,
            model=model,
            metric=METRIC,
            value=probability,
            claim_id=claim["id"],
            detail={"label": label},
        )
        for claim, label, probability in zip(claims, labels, probabilities)
    ]
