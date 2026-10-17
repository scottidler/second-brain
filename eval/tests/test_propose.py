import json
import subprocess
from pathlib import Path

import pytest

from propose.adapter import PatternAdapter, feedback_text
from propose.reflect import FabricProposer, extract, render
from propose.sb import Sb, SbError


class FakeRun:
    """Records argv and answers like sb (or fabric) would."""

    def __init__(self, stdout: str, returncode: int = 0, stderr: str = "") -> None:
        self.calls: list[list[str]] = []
        self.inputs: list[str | None] = []
        self.stdout, self.returncode, self.stderr = stdout, returncode, stderr

    def __call__(self, argv, **kwargs):
        self.calls.append(list(argv))
        self.inputs.append(kwargs.get("input"))
        return subprocess.CompletedProcess(argv, self.returncode, self.stdout, self.stderr)


def score_json(*traces):
    return json.dumps({"traces": list(traces), "complete": True})


def trace(name, score, feedback=(), errors=()):
    return {
        "trace": name,
        "score": score,
        "feedback": list(feedback),
        "errors": list(errors),
        "candidate-note": f"note of {name}",
        "source-head": f"source of {name}",
    }


def test_sb_score_passes_config_traces_and_patterns_dir(tmp_path):
    run = FakeRun(score_json(trace("t1", 0.5)))
    sb = Sb(binary="/x/sb", config="/c/borg.yml", patterns_dir=tmp_path, run=run)
    sb.score("distill-video", tmp_path / "c.md", ["t1", "t2"])
    assert run.calls == [
        [
            "/x/sb",
            "borg",
            "--config",
            "/c/borg.yml",
            "eval",
            "--candidate-pattern",
            str(tmp_path / "c.md"),
            "--pattern",
            "distill-video",
            "--traces",
            "t1",
            "t2",
            "--format",
            "json",
            "--patterns-dir",
            str(tmp_path),
        ]
    ]


def test_sb_write_candidate_passes_a_bare_flag_without_an_out_dir(tmp_path):
    run = FakeRun(json.dumps({"file": "/r/c.md"}))
    Sb(run=run).write_candidate("distill-video", tmp_path / "c.md", ["t1"], ["h1"], None)
    assert run.calls[0][-3:] == ["--format", "json", "--write-candidate"]
    Sb(run=run).write_candidate("distill-video", tmp_path / "c.md", ["t1"], ["h1"], Path("/out"))
    assert run.calls[1][-2:] == ["--write-candidate", "/out"]


def test_an_sb_failure_raises_with_its_stderr():
    run = FakeRun("", returncode=1, stderr="Error: every trace errored on an arm")
    with pytest.raises(SbError, match="every trace errored"):
        Sb(run=run).split("distill-video", 4)


def test_the_adapter_scores_in_batch_order_and_an_errored_trace_scores_zero(tmp_path):
    run = FakeRun(score_json(trace("t2", None, errors=["candidate distill: boom"]), trace("t1", 0.75)))
    adapter = PatternAdapter(Sb(run=run), "distill-video", tmp_path)
    batch = adapter.evaluate(["t1", "t2"], {"distill-video": "a pattern"}, capture_traces=True)
    assert batch.scores == [0.75, 0.0]
    assert [t["trace"] for t in batch.trajectories] == ["t1", "t2"]
    written = Path(run.calls[0][run.calls[0].index("--candidate-pattern") + 1])
    assert written.read_text() == "a pattern"
    assert adapter.evaluate(["t1", "t2"], {"distill-video": "a pattern"}).trajectories is None


def test_the_adapter_refuses_a_score_missing_a_trace(tmp_path):
    adapter = PatternAdapter(Sb(run=FakeRun(score_json(trace("t1", 1.0)))), "distill-video", tmp_path)
    with pytest.raises(RuntimeError, match="no score for"):
        adapter.evaluate(["t1", "t2"], {"distill-video": "p"})


def test_the_reflective_dataset_carries_source_note_and_findings(tmp_path):
    adapter = PatternAdapter(Sb(run=FakeRun(score_json(trace("t1", 0.0, feedback=["coverage floor: x"])))), "p", tmp_path)
    batch = adapter.evaluate(["t1"], {"p": "text"}, capture_traces=True)
    records = adapter.make_reflective_dataset({"p": "text"}, batch, ["p"])["p"]
    assert records == [
        {
            "Inputs": "start of the source:\nsource of t1",
            "Generated Outputs": "note of t1",
            "Feedback": "score 0.000\n- coverage floor: x",
        }
    ]
    assert "no finding" in feedback_text(trace("t", 1.0))


def test_the_proposer_sends_the_prompt_to_fabric_and_takes_the_tagged_pattern():
    run = FakeRun("Here you go.\n<pattern>\n# NEW\n```yaml\nsummary: x\n```\n</pattern>\n")
    proposer = FabricProposer(model="gemini-2.5-pro", run=run)
    dataset = {"p": [{"Inputs": "src", "Generated Outputs": "note", "Feedback": "- claim 1 judged unsupported"}]}
    out = proposer({"p": "# OLD"}, dataset, ["p"])
    assert out == {"p": "# NEW\n```yaml\nsummary: x\n```\n"}
    assert proposer.proposed == 1
    assert run.calls == [["fabric", "-m", "gemini-2.5-pro"]]
    assert "# OLD" in run.inputs[0] and "claim 1 judged unsupported" in run.inputs[0]


def test_a_reply_without_a_pattern_block_is_an_error_not_an_empty_pattern():
    with pytest.raises(ValueError, match="no <pattern>"):
        extract("I would change the schema.")
    with pytest.raises(ValueError, match="empty"):
        extract("<pattern>\n\n</pattern>")
    assert "fewer claims" in render("x", [])


def test_gepa_runs_the_custom_proposer_through_the_adapter_end_to_end(tmp_path):
    """A real gepa.optimize over a fake sb: the proposer must be reached and
    its rewrite scored. A missing adapter attribute once made every
    reflection fail silently."""
    import gepa

    class ScoringSb:
        def score(self, pattern, candidate, traces):
            better = "BETTER" in Path(candidate).read_text()
            return {"traces": [trace(t, 0.9 if better else 0.5) for t in traces]}

    proposed = []

    def proposer(candidate, dataset, components):
        proposed.append(dataset)
        return {c: candidate[c] + "\nBETTER\n" for c in components}

    adapter = PatternAdapter(ScoringSb(), "distill-video", tmp_path)
    result = gepa.optimize(
        seed_candidate={"distill-video": "seed"},
        trainset=["t1", "t2"],
        valset=["t1", "t2"],
        adapter=adapter,
        custom_candidate_proposer=proposer,
        reflection_minibatch_size=2,
        max_metric_calls=8,
        display_progress_bar=False,
        raise_on_exception=True,
    )
    assert proposed, "the custom proposer was never called"
    assert "BETTER" in result.best_candidate["distill-video"]
