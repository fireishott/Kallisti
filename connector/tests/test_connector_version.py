"""The connector version is the only way to identify the running build.

`__version__` is surfaced to the app as `connectorVersion`
(client.py:_detect_connector_version → host payload) and rendered in Herald's
Settings screen. If it is not bumped on deploy, the phone cannot distinguish a
freshly deployed connector from a stale one — which is exactly how a
connector that had never been deployed at all went unnoticed for a whole
build cycle on 2026-07-30.

It had also silently drifted from pyproject.toml (0.4.1 vs 0.5.0), so the
declared package version and the reported one disagreed.
"""

from __future__ import annotations

import pathlib
import re

from herald_connector import __version__


def _pyproject_version() -> str:
    text = (
        pathlib.Path(__file__).resolve().parents[1] / "pyproject.toml"
    ).read_text()
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    assert match, "pyproject.toml has no version field"
    return match.group(1)


def test_version_matches_pyproject():
    assert __version__ == _pyproject_version(), (
        f"__init__.py says {__version__} but pyproject.toml says "
        f"{_pyproject_version()} — the app reports __init__.py, so they must agree"
    )


def test_version_is_semver():
    assert re.fullmatch(r"\d+\.\d+\.\d+", __version__), __version__
