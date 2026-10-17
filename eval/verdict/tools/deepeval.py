"""DeepEval `SummarizationMetric` over a record's source and note.

A note-level score with a reason; DeepEval gives no per-claim breakdown. The judge
is an OpenAI model on purpose: the distiller is Anthropic and the default judge is
Gemini, so this one adds a third vendor to the agreement table.
"""

import os
from collections.abc import Callable
from typing import Any

from verdict.contract import verdict

TOOL = "deepeval"
METRIC = "summarization"
DEFAULT_MODEL = "gpt-4o-mini"


def tool_version() -> str:
    import deepeval

    return deepeval.__version__


def default_metric(model: str) -> Any:
    from deepeval.metrics import SummarizationMetric

    return SummarizationMetric(threshold=0.5, model=model, include_reason=True, async_mode=False)


def run(
    record: dict[str, Any],
    *,
    model: str = DEFAULT_MODEL,
    version: str | None = None,
    make_metric: Callable[[str], Any] = default_metric,
) -> list[dict[str, Any]]:
    """The summarization verdict for `record`: one note-level row."""
    from deepeval.test_case import LLMTestCase

    case = LLMTestCase(input=record["source"], actual_output=record["note_text"])
    metric = make_metric(model)
    metric.measure(case)
    if metric.score is None:
        raise RuntimeError(f"deepeval returned no score for {record['trace']}")
    return [
        verdict(
            record,
            tool=TOOL,
            tool_version=version or tool_version(),
            model=model,
            metric=METRIC,
            value=float(metric.score),
            detail=getattr(metric, "reason", None),
        )
    ]


def require_key() -> None:
    """Fail before any trace is judged when the OpenAI key is not in the env."""
    if not os.environ.get("OPENAI_API_KEY"):
        raise SystemExit("deepeval needs OPENAI_API_KEY in the environment; it is not set")
