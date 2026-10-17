"""The JSONL contract with `sb`: read trace records, write verdicts.

A verdict copies the five identity fields from the record it judged; `sb` rebuilds
the `eval_scores` key from them and rejects the verdict if `content_hash` no longer
matches the trace's current distill hash.
"""

import json
from pathlib import Path
from typing import Any

# Copied verbatim from the judged record into every verdict.
IDENTITY_FIELDS = ("trace", "run_kind", "run_id", "rubric_version", "content_hash")

# Every key a verdict line may carry. `sb` rejects any other key.
VERDICT_KEYS = (
    *IDENTITY_FIELDS,
    "tool",
    "tool_version",
    "model",
    "claim_id",
    "metric",
    "value",
    "detail",
)

RECORD_KEYS = (*IDENTITY_FIELDS, "source", "note_text", "claims")


def read_records(path: Path) -> list[dict[str, Any]]:
    """Read trace records, one JSON object per line. A bad line is an error."""
    records = []
    with path.open() as handle:
        for number, line in enumerate(handle, start=1):
            if not line.strip():
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(f"{path}:{number}: not JSON: {error}") from error
            missing = [key for key in RECORD_KEYS if key not in record]
            if missing:
                raise ValueError(f"{path}:{number}: record is missing {missing}")
            records.append(record)
    return records


def verdict(
    record: dict[str, Any],
    *,
    tool: str,
    tool_version: str,
    model: str,
    metric: str,
    value: float,
    claim_id: Any = None,
    detail: Any = None,
) -> dict[str, Any]:
    """One verdict about `record`. `claim_id` and `detail` are omitted when None."""
    out: dict[str, Any] = {key: record[key] for key in IDENTITY_FIELDS}
    out.update(tool=tool, tool_version=tool_version, model=model, metric=metric, value=value)
    if claim_id is not None:
        out["claim_id"] = claim_id
    if detail is not None:
        out["detail"] = detail
    return out


def write_verdicts(path: Path, verdicts: list[dict[str, Any]]) -> None:
    """Write verdicts, one JSON object per line, replacing any existing file."""
    with path.open("w") as handle:
        for item in verdicts:
            handle.write(json.dumps(item) + "\n")
