"""Tests for _hermes_agent_version async caching.

D2 fix: failure must not be cached permanently; must run off-loop.
"""

from __future__ import annotations

import asyncio
import subprocess
import time
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from herald_connector.client import HeraldConnector


@pytest.fixture
def connector():
    """Create a minimal HeraldConnector for testing."""
    c = HeraldConnector.__new__(HeraldConnector)
    c._hermes_agent_version_cache = None
    c._hermes_agent_version_failed_at = 0.0
    c._AGENT_VERSION_RETRY_SECONDS = 60.0
    return c


def _mock_to_thread_success(stdout, stderr=""):
    """Return a mock asyncio.to_thread that calls the function synchronously and returns its result."""
    async def _to_thread(fn, *args):
        return fn(*args)
    return _to_thread


def _mock_to_thread_with_results(*results):
    """Return a mock asyncio.to_thread that returns successive results on each call."""
    call_count = 0
    nonlocal_results = list(results)

    async def _to_thread(fn, *args):
        nonlocal call_count
        idx = call_count
        call_count += 1
        result = nonlocal_results[idx]
        if isinstance(result, Exception):
            raise result
        return result

    return _to_thread


class TestAgentVersionNotCachedOnFailure:
    """D2: first probe timeout must not permanently poison the cache."""

    @pytest.mark.asyncio
    async def test_failure_then_success(self, connector):
        """First probe raises TimeoutExpired, second succeeds → returns version."""
        # First call: TimeoutExpired; second: "0.19.0"
        mock_thread = _mock_to_thread_with_results(
            subprocess.TimeoutExpired(cmd=["hermes"], timeout=30),
            "0.19.0",
        )

        with patch("asyncio.to_thread", side_effect=mock_thread):
            # First call — should fail and set negative cache
            result1 = await connector._hermes_agent_version()
            assert result1 is None

            # Expire the TTL
            connector._hermes_agent_version_failed_at = time.monotonic() - 61

            # Second call — should succeed
            result2 = await connector._hermes_agent_version()
            assert result2 == "0.19.0"

    @pytest.mark.asyncio
    async def test_success_cached_permanently(self, connector):
        """Success is cached for process lifetime."""
        call_count = 0

        async def mock_thread(fn, *args):
            nonlocal call_count
            call_count += 1
            return "0.19.0"

        with patch("asyncio.to_thread", side_effect=mock_thread):
            result1 = await connector._hermes_agent_version()
            assert result1 == "0.19.0"
            assert call_count == 1

            # Second call — should use cache, no additional subprocess
            result2 = await connector._hermes_agent_version()
            assert result2 == "0.19.0"
            assert call_count == 1


class TestAgentVersionNegativeCacheTTL:
    """D2: failures are retried after TTL, not cached forever."""

    @pytest.mark.asyncio
    async def test_two_failures_within_ttl_only_one_probe(self, connector):
        """Two failures within TTL window → only one subprocess call."""
        probe_count = 0

        async def mock_thread(fn, *args):
            nonlocal probe_count
            probe_count += 1
            raise subprocess.TimeoutExpired(cmd=["hermes"], timeout=30)

        with patch("asyncio.to_thread", side_effect=mock_thread):
            # First failure
            result1 = await connector._hermes_agent_version()
            assert result1 is None
            assert probe_count == 1

            # Second call within TTL — should NOT probe again
            result2 = await connector._hermes_agent_version()
            assert result2 is None
            assert probe_count == 1  # still only one probe

    @pytest.mark.asyncio
    async def test_failure_then_success_after_ttl(self, connector):
        """After TTL expires, probe runs again and can succeed."""
        call_count = 0

        async def mock_thread(fn, *args):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                raise subprocess.TimeoutExpired(cmd=["hermes"], timeout=30)
            return "0.20.0"

        with patch("asyncio.to_thread", side_effect=mock_thread):
            # First failure
            await connector._hermes_agent_version()
            assert call_count == 1

            # Expire the TTL
            connector._hermes_agent_version_failed_at = time.monotonic() - 61

            # Should probe again and succeed
            result = await connector._hermes_agent_version()
            assert result == "0.20.0"
            assert call_count == 2


class TestAgentVersionOutputParsing:
    """Tests for version string parsing from stdout/stderr."""

    @pytest.mark.asyncio
    async def test_reads_stderr(self, connector):
        """Version only on stderr → still parsed."""
        async def mock_thread(fn, *args):
            return "0.19.0"

        with patch("asyncio.to_thread", side_effect=mock_thread):
            result = await connector._hermes_agent_version()
            assert result == "0.19.0"

    @pytest.mark.asyncio
    async def test_empty_output(self, connector):
        """stdout='' → None, no IndexError."""
        async def mock_thread(fn, *args):
            return None

        with patch("asyncio.to_thread", side_effect=mock_thread):
            result = await connector._hermes_agent_version()
            assert result is None

    @pytest.mark.asyncio
    async def test_banner_before_version(self, connector):
        """Non-version lines before the version line → still parsed."""
        async def mock_thread(fn, *args):
            return "0.19.0"

        with patch("asyncio.to_thread", side_effect=mock_thread):
            result = await connector._hermes_agent_version()
            assert result == "0.19.0"
