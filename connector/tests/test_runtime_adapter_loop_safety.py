import asyncio
import threading

import pytest

from herald_connector.runtime_adapter import _run_blocking


def test_run_blocking_works_off_the_loop():
    async def coro():
        return "ok"
    assert _run_blocking(coro()) == "ok"


def test_run_blocking_rejects_loop_thread():
    """When called on the event-loop thread, _run_blocking must raise clearly."""
    async def coro():
        return "ok"

    async def run_on_loop():
        with pytest.raises(RuntimeError, match="asyncio.to_thread"):
            _run_blocking(coro())

    asyncio.run(run_on_loop())


def test_run_blocking_survives_to_thread():
    """asyncio.to_thread() should protect the call — the helper must not reject."""
    async def coro():
        return "ok"

    async def run_on_loop():
        return await asyncio.to_thread(lambda: _run_blocking(coro()))

    result = asyncio.run(run_on_loop())
    assert result == "ok"
