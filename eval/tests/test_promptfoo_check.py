"""The run/skip decision of bin/promptfoo-check, against throwaway git repos."""

import shutil
import subprocess
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[2] / "bin" / "promptfoo-check"


def git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    """A repo whose origin/main holds distill-video.md and a source file."""
    (tmp_path / "bin").mkdir()
    shutil.copy(SCRIPT, tmp_path / "bin" / "promptfoo-check")
    (tmp_path / "borg" / "patterns").mkdir(parents=True)
    (tmp_path / "borg" / "patterns" / "distill-video.md").write_text("# video\n")
    (tmp_path / "borg" / "src").mkdir()
    (tmp_path / "borg" / "src" / "lib.rs").write_text("// lib\n")
    git(tmp_path, "init", "-q", "-b", "main")
    git(tmp_path, "add", ".")
    git(tmp_path, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base")
    git(tmp_path, "update-ref", "refs/remotes/origin/main", "HEAD")
    return tmp_path


def decide(repo: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(repo / "bin" / "promptfoo-check"), "--decide"], capture_output=True, text=True, check=False
    )


def test_an_unchanged_pattern_skips(repo):
    done = decide(repo)
    assert done.returncode == 0
    assert done.stdout.startswith("promptfoo: skipped: no borg/patterns/*.md differs from origin/main")


def test_an_edited_distill_video_runs(repo):
    (repo / "borg" / "patterns" / "distill-video.md").write_text("# video, sharper\n")
    done = decide(repo)
    assert done.returncode == 0
    assert done.stdout.startswith("promptfoo: running: differs from origin/main: borg/patterns/distill-video.md")


def test_a_committed_pattern_change_ahead_of_origin_runs(repo):
    (repo / "borg" / "patterns" / "distill-video.md").write_text("# video, committed\n")
    git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "edit")
    assert "running" in decide(repo).stdout


def test_a_new_untracked_pattern_runs(repo):
    (repo / "borg" / "patterns" / "distill-new.md").write_text("# new\n")
    assert "borg/patterns/distill-new.md" in decide(repo).stdout


def test_a_change_outside_the_patterns_skips(repo):
    (repo / "borg" / "src" / "lib.rs").write_text("// changed\n")
    assert "skipped" in decide(repo).stdout


def test_a_missing_base_fails_loudly(repo):
    git(repo, "update-ref", "-d", "refs/remotes/origin/main")
    done = decide(repo)
    assert done.returncode == 2
    assert "is not a commit" in done.stderr
