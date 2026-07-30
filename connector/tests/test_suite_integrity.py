"""Guard: every test module in this directory must import.

A file that raises on import contributes zero tests and zero failures, so a
green run says nothing about it.  That is exactly how the connector lost
coverage of ``test_connector.py``, ``test_stream_contract.py`` and
``test_streaming.py``: they referenced ``herald_connector.hermes_runner`` and
``herald_connector.stream_contract``, both deleted in Herald 2.0 (4873920),
and stayed broken long enough for five reply-path regressions to land in
Build 41 without a single red test.

This guard walks the directory itself rather than trusting collection, so it
still fires when the broken file is skipped with ``--ignore``.  The companion
hook in ``conftest.py`` covers the case where this file is the broken one.
"""

from __future__ import annotations

import importlib
from pathlib import Path

import pytest

TESTS_DIR = Path(__file__).parent


def _test_module_names() -> list[str]:
    """Every test module in this directory, by importable name.

    ``tests/`` has no ``__init__.py``, so pytest's prepend import mode puts
    the directory on sys.path and each file is importable by its stem.
    """
    return sorted(
        path.stem
        for path in TESTS_DIR.glob("test_*.py")
        if path.is_file()
    )


def test_directory_contains_test_modules():
    """Sanity check: the discovery glob itself must not silently return nothing."""
    names = _test_module_names()
    assert len(names) >= 10, f"Suspiciously few test modules discovered: {names}"


@pytest.mark.parametrize("module_name", _test_module_names())
def test_module_imports(module_name: str):
    """Importing the module must not raise.

    Already-imported modules come back from sys.modules, so this does not
    re-run module-level code for files pytest has collected normally.
    """
    try:
        importlib.import_module(module_name)
    except pytest.skip.Exception:
        # A module that skips itself at import time is deliberate, not broken.
        pass
    except Exception as error:  # noqa: BLE001 - reporting is the whole point
        pytest.fail(
            f"{module_name}.py failed to import: {type(error).__name__}: {error}\n"
            f"A test file that cannot import blocks its entire target. "
            f"Port it to the current module layout or delete it."
        )
