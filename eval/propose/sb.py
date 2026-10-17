"""The `sb borg eval` calls the search makes. sb owns the split, the scoring,
the coverage floor, the `candidate` rows and the candidate file; this side
only parses its JSON."""

import json
import subprocess
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

Runner = Callable[..., subprocess.CompletedProcess]


class SbError(RuntimeError):
    """An `sb` call that failed or printed something other than JSON."""


@dataclass
class Sb:
    binary: str = "sb"
    config: str | None = None
    patterns_dir: Path | None = None
    run: Runner = field(default=subprocess.run, repr=False)

    def _eval(self, args: Sequence[str]) -> dict[str, Any]:
        argv = [self.binary, "borg"]
        if self.config:
            argv += ["--config", self.config]
        argv += ["eval", *args]
        if self.patterns_dir is not None:
            argv += ["--patterns-dir", str(self.patterns_dir)]
        done = self.run(argv, capture_output=True, text=True, check=False)
        if done.returncode != 0:
            raise SbError(f"{' '.join(argv)} exited {done.returncode}: {done.stderr.strip()[-2000:]}")
        try:
            return json.loads(done.stdout)
        except json.JSONDecodeError as error:
            raise SbError(f"{' '.join(argv)} printed no JSON: {error}") from error

    def split(self, pattern: str, n: int) -> dict[str, Any]:
        """Two disjoint samples of n staged traces that reach `pattern`."""
        return self._eval(["--candidate-split", str(n), "--pattern", pattern])

    def score(self, pattern: str, candidate: Path, traces: Sequence[str]) -> dict[str, Any]:
        """The candidate against the current pattern on `traces`."""
        return self._eval(
            ["--candidate-pattern", str(candidate), "--pattern", pattern, "--traces", *traces, "--format", "json"]
        )

    def write_candidate(
        self,
        pattern: str,
        candidate: Path,
        train: Sequence[str],
        holdout: Sequence[str],
        out: Path | None,
    ) -> dict[str, Any]:
        """Score the winner on both samples and write the candidate file."""
        args = ["--candidate-pattern", str(candidate), "--pattern", pattern, "--traces", *train]
        args += ["--holdout", *holdout, "--format", "json", "--write-candidate"]
        if out is not None:
            args.append(str(out))
        return self._eval(args)
