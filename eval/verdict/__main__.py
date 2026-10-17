"""`python -m verdict --records in.jsonl --out verdicts.jsonl`

Runs every selected tool over every record and writes the verdicts. A tool that
fails on one record is reported on stderr and the run goes on; the exit is
non-zero if any failed, so a partial verdict file is never mistaken for a full one.
"""

import argparse
import os
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

os.environ.setdefault("DEEPEVAL_TELEMETRY_OPT_OUT", "YES")

from verdict import contract  # noqa: E402
from verdict.tools import deepeval, minicheck  # noqa: E402

TOOLS = (deepeval.TOOL, minicheck.TOOL)

Runner = Callable[[dict[str, Any]], list[dict[str, Any]]]


def run_tools(records: list[dict[str, Any]], runners: dict[str, Runner]) -> tuple[list[dict[str, Any]], list[str]]:
    """Every runner over every record: the verdicts, and one line per failure."""
    verdicts: list[dict[str, Any]] = []
    failures: list[str] = []
    for record in records:
        for name, runner in runners.items():
            try:
                verdicts.extend(runner(record))
            except Exception as error:  # a tool's failure is a failure line, never a silent skip
                failures.append(f"{name} failed on {record['trace']}: {error}")
    return verdicts, failures


def parse(argv: list[str]) -> argparse.Namespace:
    cache_home = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    parser = argparse.ArgumentParser(prog="verdict", description=__doc__)
    parser.add_argument("--records", type=Path, required=True, help="JSONL from `sb borg eval --format json`")
    parser.add_argument("--out", type=Path, required=True, help="verdict JSONL to write")
    parser.add_argument("--tool", action="append", choices=TOOLS, help="repeat; default is every tool")
    parser.add_argument("--deepeval-model", default=deepeval.DEFAULT_MODEL)
    parser.add_argument("--minicheck-model", default=minicheck.DEFAULT_MODEL)
    parser.add_argument("--model-cache", type=Path, default=cache_home / "sb" / "eval" / "models")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse(sys.argv[1:] if argv is None else argv)
    selected = args.tool or list(TOOLS)
    records = contract.read_records(args.records)
    runners: dict[str, Runner] = {}
    if deepeval.TOOL in selected:
        deepeval.require_key()
        version = deepeval.tool_version()
        runners[deepeval.TOOL] = lambda r: deepeval.run(r, model=args.deepeval_model, version=version)
    if minicheck.TOOL in selected:
        args.model_cache.mkdir(parents=True, exist_ok=True)
        scorer = minicheck.load_scorer(args.minicheck_model, args.model_cache)
        runners[minicheck.TOOL] = lambda r: minicheck.run(r, model=args.minicheck_model, scorer=scorer)
    verdicts, failures = run_tools(records, runners)
    contract.write_verdicts(args.out, verdicts)
    for line in failures:
        print(line, file=sys.stderr)
    print(f"{len(verdicts)} verdicts for {len(records)} records written to {args.out}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
