"""Tests for git diff capture used to detect file changes made by Hermes.

Covers:
  - capture_snapshot records changed/untracked files
  - capture_diff detects newly changed files vs pre-snapshot
  - capture_diff returns None when no new changes
  - capture_diff handles untracked (new) files
  - capture_diff correctly ignores pre-existing changes
  - capture_snapshot returns None for non-git directories
  - _classify_status maps porcelain codes correctly
  - _count_diff_lines counts additions and deletions
"""

from __future__ import annotations

import asyncio
import subprocess
from pathlib import Path

from herald_connector.git_diff import (
    _classify_status,
    _count_diff_lines,
    capture_diff,
    capture_snapshot,
)


def _init_git_repo(path: Path) -> None:
    """Create a fresh git repo with an initial commit."""
    subprocess.run(["git", "init"], cwd=str(path), capture_output=True, check=True)
    subprocess.run(["git", "config", "user.email", "test@test.com"], cwd=str(path), capture_output=True, check=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=str(path), capture_output=True, check=True)
    (path / "README.md").write_text("# Hello\n")
    subprocess.run(["git", "add", "."], cwd=str(path), capture_output=True, check=True)
    subprocess.run(["git", "commit", "-m", "init"], cwd=str(path), capture_output=True, check=True)


def test_snapshot_captures_modified_files(tmp_path):
    """A snapshot should record files that have uncommitted changes."""
    _init_git_repo(tmp_path)

    # Modify an existing file
    (tmp_path / "README.md").write_text("# Modified\n")

    snapshot = asyncio.run(capture_snapshot(str(tmp_path)))
    assert snapshot is not None
    assert "README.md" in snapshot.entries


def test_snapshot_captures_untracked_files(tmp_path):
    """A snapshot should include untracked (new) files."""
    _init_git_repo(tmp_path)

    # Create a new untracked file
    (tmp_path / "new_file.py").write_text("print('hello')\n")

    snapshot = asyncio.run(capture_snapshot(str(tmp_path)))
    assert snapshot is not None
    assert "new_file.py" in snapshot.entries
    assert snapshot.entries["new_file.py"].strip() == "??"


def test_snapshot_empty_for_clean_repo(tmp_path):
    """A clean repo should produce a snapshot with no entries."""
    _init_git_repo(tmp_path)

    snapshot = asyncio.run(capture_snapshot(str(tmp_path)))
    assert snapshot is not None
    assert len(snapshot.entries) == 0


def test_snapshot_returns_none_for_non_git_dir(tmp_path):
    """A non-git directory should return None."""
    snapshot = asyncio.run(capture_snapshot(str(tmp_path)))
    assert snapshot is None


def test_diff_detects_new_changes(tmp_path):
    """capture_diff should detect files changed after the snapshot was taken."""
    _init_git_repo(tmp_path)

    # Take snapshot when repo is clean
    pre = asyncio.run(capture_snapshot(str(tmp_path)))
    assert pre is not None
    assert len(pre.entries) == 0

    # Simulate Hermes modifying a file
    (tmp_path / "README.md").write_text("# Updated by Hermes\n")

    result = asyncio.run(capture_diff(str(tmp_path), pre))
    assert result is not None
    assert len(result["files"]) == 1
    assert result["files"][0]["path"] == "README.md"
    assert result["files"][0]["status"] == "modified"
    assert result["files"][0]["additions"] > 0
    assert "file changed" in result["summary"]


def test_diff_detects_new_files(tmp_path):
    """capture_diff should detect newly created files."""
    _init_git_repo(tmp_path)

    pre = asyncio.run(capture_snapshot(str(tmp_path)))

    # Simulate Hermes creating a new file
    (tmp_path / "agent_output.py").write_text("def hello():\n    return 'world'\n")

    result = asyncio.run(capture_diff(str(tmp_path), pre))
    assert result is not None
    assert any(f["path"] == "agent_output.py" for f in result["files"])
    assert any(f["status"] == "added" for f in result["files"])


def test_diff_ignores_preexisting_changes(tmp_path):
    """If a file was already modified before the snapshot, it should be excluded."""
    _init_git_repo(tmp_path)

    # Pre-existing change
    (tmp_path / "README.md").write_text("# Already modified\n")

    # Snapshot includes the pre-existing change
    pre = asyncio.run(capture_snapshot(str(tmp_path)))
    assert "README.md" in pre.entries

    # No additional changes after snapshot
    result = asyncio.run(capture_diff(str(tmp_path), pre))
    assert result is None  # No new changes


def test_diff_detects_additional_edits_to_already_dirty_file(tmp_path):
    """If Hermes edits a file that was already dirty, we should still capture it."""
    _init_git_repo(tmp_path)

    (tmp_path / "README.md").write_text("# Already modified\n")
    pre = asyncio.run(capture_snapshot(str(tmp_path)))
    assert pre is not None
    assert "README.md" in pre.entries

    (tmp_path / "README.md").write_text("# Already modified again\n")

    result = asyncio.run(capture_diff(str(tmp_path), pre))
    assert result is not None
    assert len(result["files"]) == 1
    assert result["files"][0]["path"] == "README.md"
    assert result["files"][0]["status"] == "modified"
    assert "Already modified again" in result["files"][0]["patch"]


def test_diff_detects_mix_of_new_and_existing(tmp_path):
    """When some files were already changed, only new changes should be included."""
    _init_git_repo(tmp_path)

    # Pre-existing change
    (tmp_path / "README.md").write_text("# Already modified\n")

    pre = asyncio.run(capture_snapshot(str(tmp_path)))

    # Hermes creates a new file (but README was already dirty)
    (tmp_path / "hermes_output.txt").write_text("Generated content\n")

    result = asyncio.run(capture_diff(str(tmp_path), pre))
    assert result is not None
    assert len(result["files"]) == 1
    assert result["files"][0]["path"] == "hermes_output.txt"


def test_diff_returns_none_when_pre_is_none():
    """If pre-snapshot is None (non-git dir), diff should return None."""
    result = asyncio.run(capture_diff("/tmp", None))
    assert result is None


def test_classify_status():
    """Porcelain status codes should map to human-friendly strings."""
    assert _classify_status("??") == "added"
    assert _classify_status(" M") == "modified"
    assert _classify_status("M ") == "modified"
    assert _classify_status("A ") == "added"
    assert _classify_status("D ") == "deleted"
    assert _classify_status("R ") == "renamed"
    assert _classify_status("MM") == "modified"


def test_count_diff_lines():
    """Should correctly count +/- lines in a unified diff."""
    patch = """\
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,4 @@
 unchanged line
-deleted line
+added line
+another added
 context
"""
    additions, deletions = _count_diff_lines(patch)
    assert additions == 2
    assert deletions == 1


def test_count_diff_lines_empty():
    """An empty patch should return zero counts."""
    assert _count_diff_lines("") == (0, 0)


def test_diff_summary_format(tmp_path):
    """The summary string should follow git-style format."""
    _init_git_repo(tmp_path)

    pre = asyncio.run(capture_snapshot(str(tmp_path)))

    (tmp_path / "a.txt").write_text("line1\nline2\n")
    (tmp_path / "b.txt").write_text("content\n")

    result = asyncio.run(capture_diff(str(tmp_path), pre))
    assert result is not None
    assert "2 files changed" in result["summary"]
    assert "insertion" in result["summary"]
