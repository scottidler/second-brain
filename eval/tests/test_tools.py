from pathlib import Path

import pytest

from verdict import __main__ as cli
from verdict.tools import deepeval, minicheck


class FakeMetric:
    def __init__(self, score, reason="because"):
        self.score = score
        self.reason = reason
        self.measured = None

    def measure(self, case):
        self.measured = case


def test_minicheck_writes_one_verdict_per_claim_with_the_claim_id(record):
    seen = {}

    def scorer(docs, claims):
        seen["docs"], seen["claims"] = docs, claims
        return [1, 0], [0.91, 0.12]

    out = minicheck.run(record, model="roberta-large", scorer=scorer)
    assert seen["docs"] == [record["source"]] * 2
    assert seen["claims"] == ["Rust has a borrow checker.", "Rust shipped in 2015."]
    assert [(v["claim_id"], v["value"], v["detail"]) for v in out] == [
        (1, 0.91, {"label": 1}),
        (2, 0.12, {"label": 0}),
    ]
    assert {v["tool"] for v in out} == {"minicheck"}
    assert all(v["content_hash"] == record["content_hash"] for v in out)


def test_minicheck_on_a_record_with_no_claims_writes_nothing(record):
    record["claims"] = []
    assert minicheck.run(record, model="m", scorer=lambda d, c: pytest.fail("scored nothing")) == []


def test_minicheck_fails_when_the_scorer_returns_a_short_answer(record):
    with pytest.raises(RuntimeError, match="1 of 2 claims for cl-aaaa1111"):
        minicheck.run(record, model="m", scorer=lambda d, c: ([1], [0.9]))


def test_minicheck_requires_a_private_nltk_data_with_punkt_tab(monkeypatch, tmp_path: Path):
    monkeypatch.delenv("NLTK_DATA", raising=False)
    with pytest.raises(SystemExit, match="NLTK_DATA"):
        minicheck.require_nltk_data()
    monkeypatch.setenv("NLTK_DATA", str(tmp_path))
    with pytest.raises(SystemExit, match="punkt_tab"):
        minicheck.require_nltk_data()
    (tmp_path / "tokenizers" / "punkt_tab").mkdir(parents=True)
    minicheck.require_nltk_data()


def test_deepeval_writes_one_note_level_verdict_with_the_reason(record):
    metric = FakeMetric(0.4, "a contradiction")
    out = deepeval.run(record, model="gpt-4o-mini", version="9.9", make_metric=lambda model: metric)
    assert metric.measured.input == record["source"]
    assert metric.measured.actual_output == record["note_text"]
    assert len(out) == 1
    assert (out[0]["tool"], out[0]["tool_version"], out[0]["model"]) == ("deepeval", "9.9", "gpt-4o-mini")
    assert (out[0]["metric"], out[0]["value"], out[0]["detail"]) == ("summarization", 0.4, "a contradiction")
    assert "claim_id" not in out[0]


def test_deepeval_with_no_score_is_an_error_not_a_zero(record):
    with pytest.raises(RuntimeError, match="no score for cl-aaaa1111"):
        deepeval.run(record, model="m", version="1", make_metric=lambda model: FakeMetric(None))


def test_deepeval_requires_the_openai_key(monkeypatch):
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    with pytest.raises(SystemExit, match="OPENAI_API_KEY"):
        deepeval.require_key()
    monkeypatch.setenv("OPENAI_API_KEY", "set")
    deepeval.require_key()


def test_a_failing_tool_is_a_named_failure_and_the_other_tools_still_run(record):
    def boom(r):
        raise RuntimeError("vendor 500")

    verdicts, failures = cli.run_tools(
        [record], {"deepeval": boom, "minicheck": lambda r: minicheck.run(r, model="m", scorer=lambda d, c: ([1, 1], [1.0, 1.0]))}
    )
    assert failures == ["deepeval failed on cl-aaaa1111: vendor 500"]
    assert len(verdicts) == 2
