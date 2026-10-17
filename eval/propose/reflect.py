"""The reflection step: fabric rewrites the pattern from the scored notes."""

import subprocess
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any

Runner = Callable[..., subprocess.CompletedProcess]

OPEN, CLOSE = "<pattern>", "</pattern>"

PROMPT = """You are improving a prompt pattern that distills a source into a structured note.

{open}
{pattern}
{close}

Below are notes this pattern produced, each with the start of its source and what an evaluator found.

{examples}

The evaluator multiplies three rates: quote grounding (every quote must appear verbatim in the source), claim
support (an independent judge checks each claim against the source) and listicle survival (an enumeration must list
every item it declares). A rewrite that produces fewer claims or fewer enumeration items than the current pattern
scores zero, so never trade coverage for accuracy.

Rewrite the pattern to fix the findings. Keep its SCHEMA section, its YAML output contract and every field name
exactly as they are. Return the complete new pattern between {open} and {close}, and nothing else."""


def render(pattern: str, records: Sequence[Mapping[str, Any]]) -> str:
    examples = "\n\n".join(
        f"### Example {i}\n{r['Inputs']}\n\nNote:\n{r['Generated Outputs']}\n\nEvaluator:\n{r['Feedback']}"
        for i, r in enumerate(records, 1)
    )
    return PROMPT.format(open=OPEN, close=CLOSE, pattern=pattern, examples=examples)


def extract(reply: str) -> str:
    """The text between the first `<pattern>` and the last `</pattern>`."""
    start, end = reply.find(OPEN), reply.rfind(CLOSE)
    if start < 0 or end <= start:
        raise ValueError(f"the reflection reply carries no {OPEN}...{CLOSE} block")
    text = reply[start + len(OPEN) : end].strip("\n")
    if not text.strip():
        raise ValueError("the reflection reply's pattern is empty")
    return text + "\n"


@dataclass
class FabricProposer:
    """GEPA's `custom_candidate_proposer`: one fabric call per component."""

    model: str
    fabric: str = "fabric"
    run: Runner = field(default=subprocess.run, repr=False)
    # Rewrites returned to GEPA; it keeps only those that win their minibatch.
    proposed: int = 0

    def __call__(
        self,
        candidate: dict[str, str],
        reflective_dataset: Mapping[str, Sequence[Mapping[str, Any]]],
        components_to_update: list[str],
    ) -> dict[str, str]:
        out = {}
        for component in components_to_update:
            prompt = render(candidate[component], reflective_dataset[component])
            done = self.run([self.fabric, "-m", self.model], input=prompt, capture_output=True, text=True, check=False)
            if done.returncode != 0:
                raise RuntimeError(f"fabric -m {self.model} exited {done.returncode}: {done.stderr.strip()[-2000:]}")
            out[component] = extract(done.stdout)
        self.proposed += 1
        return out
