from typing import Any

import pytest


@pytest.fixture
def record() -> dict[str, Any]:
    """A trace record in the shape `sb borg eval --format json` writes."""
    return {
        "trace": "cl-aaaa1111",
        "fixture": "article/cl-aaaa1111",
        "run_kind": "baseline",
        "run_id": "",
        "rubric_version": "v2",
        "judge_model": "",
        "kind": "article",
        "content_hash": "00000000deadbeef",
        "source": "Rust has a borrow checker. It was released in 2015.",
        "source_truncated": False,
        "note_text": "SUMMARY: Rust.\nCLAIMS:\n- Rust has a borrow checker.\n- Rust shipped in 2015.\n",
        "claims": [
            {"id": 1, "text": "Rust has a borrow checker."},
            {"id": 2, "text": "Rust shipped in 2015.", "quote": "released in 2015"},
        ],
        "metrics": {},
        "errors": [],
    }
