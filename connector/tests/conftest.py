"""Shared pytest configuration for the connector suite.

The hook below makes a collection error impossible to overlook.  A test file
that fails to import is not "zero tests failing" — it is an entire target
whose coverage silently vanished, which is how five reply-path regressions
rode into Build 41 unnoticed while ``test_connector.py``,
``test_stream_contract.py`` and ``test_streaming.py`` sat uncollected behind
a ModuleNotFoundError.

See also ``test_suite_integrity.py``, which catches the same failure even
when the broken file is explicitly ``--ignore``d.
"""

from __future__ import annotations

import pytest


def pytest_collectreport(report: pytest.CollectReport) -> None:
    """Print a loud banner for any module that failed to import."""
    if report.failed:
        print(  # noqa: T201 - deliberate: must survive -q
            f"\n!!! COLLECTION ERROR in {report.nodeid or '<unknown>'} — "
            f"this file's tests did NOT run !!!"
        )


def pytest_terminal_summary(terminalreporter, exitstatus, config) -> None:  # noqa: ANN001, ARG001
    """Restate collection errors at the very bottom of the run."""
    errors = terminalreporter.stats.get("error", [])
    collection_errors = [r for r in errors if getattr(r, "when", None) == "collect"]
    if not collection_errors:
        return

    terminalreporter.write_sep("!", "UNCOLLECTED TEST FILES", red=True, bold=True)
    for report in collection_errors:
        terminalreporter.write_line(f"  {report.nodeid}", red=True)
    terminalreporter.write_line(
        "A test file that fails to import blocks its whole target. "
        "Port it or delete it — do not leave it uncollected.",
        red=True,
    )
