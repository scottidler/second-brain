#!/usr/bin/env python3
"""Generate the reference BGE embeddings the Phase 3 parity test compares against.

Run this once on any machine that has `sentence-transformers` installed.
The resulting JSON sits next to the test and ships in the repo; CI does
not regenerate it. If the BGE checkpoint or the test texts change, rerun
this script and commit the new fixture.

Recipe:
    pipx install --include-deps sentence-transformers
    pipx run --spec sentence-transformers python bin/gen-bge-reference.py

The fixture is small (~5 KB for three vectors) and stable as long as we
stay on `BAAI/bge-small-en-v1.5`.
"""

from __future__ import annotations

import json
from pathlib import Path

from sentence_transformers import SentenceTransformer


REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURE_PATH = REPO_ROOT / "vault" / "tests" / "fixtures" / "bge-reference.json"

REFERENCE_TEXTS = [
    "The capital of France is Paris.",
    "Mango is a tropical fruit.",
    "Hybrid retrieval combines BM25 with vector search via reciprocal rank fusion.",
]


def main() -> None:
    model = SentenceTransformer("BAAI/bge-small-en-v1.5")
    embeddings = model.encode(REFERENCE_TEXTS, normalize_embeddings=True).tolist()
    payload = {"texts": REFERENCE_TEXTS, "embeddings": embeddings}
    FIXTURE_PATH.parent.mkdir(parents=True, exist_ok=True)
    FIXTURE_PATH.write_text(json.dumps(payload, indent=2) + "\n")
    print(f"wrote {FIXTURE_PATH} ({len(REFERENCE_TEXTS)} vectors, dim={len(embeddings[0])})")


if __name__ == "__main__":
    main()
