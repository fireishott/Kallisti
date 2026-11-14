"""Background process registry for the Live tab (build 135.17).

Exposes a single ``BackgroundProcessRegistry`` singleton that tracks
long-running subprocesses the connector spawns on behalf of the iOS
app.  Each registered process is identified by a UUID and lives in
memory until the process exits (with a small grace period so the
terminal event reaches subscribers before the row is GC'd).

Wire shape
-----------
Every public event is a JSON object with these fields:

    {
        "id": "<uuid>",
        "name": "<short label>",
        "command": ["argv0", "argv1", ...],
        "pid": <int> | null,
        "status": "starting" | "running" | "completed" | "failed" | "killed",
        "exitCode": <int> | null,
        "startedAt": "<iso8601>",
        "finishedAt": "<iso8601>" | null,
        "stream": "stdout" | "stderr" | "system",
        "chunk": "<utf-8 chunk, may be empty for snapshots>",
        "outputTail": "<utf-8 ring buffer of stdout+stderr, capped>"
    }

Status "starting" + pid=null is the initial snapshot, "running" is
emitted as soon as the pid is known, "completed"/"failed"/"killed"
are terminal.  Every ``stream`` chunk is followed by an updated
``outputTail`` snapshot so a fresh subscriber can render the latest
output even if it missed earlier deltas.

Auto-tracking
-------------
``tracked_subprocess_exec`` is a thin wrapper around
``asyncio.create_subprocess_exec`` that registers the spawned
process with the global registry before returning.  Existing call
sites that should be visible in the Live tab (gateway logs stream,
gateway restart, hermes update apply, etc.) adopt this wrapper
without further changes.

Why not the gateway? The Hermes gateway is a separate process
running on a different host (fih-ai-host).  The MBP connector is
the iOS app's nearest long-lived peer, so this registry lives here
and surfaces over the same HTTP/SSE facade the iOS app already
authenticates against.
"""

from __future__ import annotations

import asyncio
import logging
import time
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, AsyncIterator, Iterable, Optional

logger = logging.getLogger("herald.background_processes")

# Cap the per-process output tail so a runaway process (e.g. a chatty
# dev server) can't blow up SSE memory.  48 KiB matches the iOS
# TerminalOutputView maxChars (CanvasView.swift TerminalOutputView).
_OUTPUT_TAIL_MAX = 48 * 1024

# Terminal-row retention after a process exits, so a late iOS
# subscriber still sees the terminal event in the snapshot.
_TERMINAL_RETENTION_SEC = 300.0

# How often the registry prunes old terminal rows.
_GC_INTERVAL_SEC = 30.0

# Heartbeat cadence: the iOS SSE pipeline uses this to detect dead
# connections through proxies that buffer idle streams.
_KEEPALIVE_SEC = 15.0


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


class _safe_ignore:
    """Tiny context manager that swallows a specific exception class.

    Used so the registry's unsubscribe / cleanup paths don't crash
    if the caller has already removed the queue (race with the
    registry's own dead-subscriber sweep).
    """

    def __init__(self, *exc_types: type[BaseException]) -> None:
        self._exc_types = exc_types

    def __enter__(self) -> "_safe_ignore":
        return self

    def __exit__(self, exc_type, exc, tb) -> bool:  # noqa: ANN001
        return exc_type is not None and issubclass(exc_type, self._exc_types)


@dataclass
class TrackedProcess:
    """In-memory record for a single tracked subprocess."""

    id: str
    name: str
    command: list[str]
    pid: Optional[int] = None
    status: str = "starting"  # starting | running | completed | failed | killed
    exit_code: Optional[int] = None
    started_at: str = field(default_factory=_now_iso)
    finished_at: Optional[str] = None
    _output_tail: str = ""
    _subscribers: list[asyncio.Queue[dict[str, Any]]] = field(default_factory=list)
    _proc: Optional[asyncio.subprocess.Process] = None
    _tail_task: Optional[asyncio.Task[None]] = None
    _completed: bool = False

    def snapshot(self, *, stream: str = "system", chunk: str = "") -> dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "command": list(self.command),
            "pid": self.pid,
            "status": self.status,
            "exitCode": self.exit_code,
            "startedAt": self.started_at,
            "finishedAt": self.finished_at,
            "stream": stream,
            "chunk": chunk,
            "outputTail": self._output_tail,
        }

    def _append_output(self, chunk: str) -> None:
        if not chunk:
            return
        self._output_tail = (self._output_tail + chunk)[-_OUTPUT_TAIL_MAX:]

    def publish(self, event: dict[str, Any]) -> None:
        """Broadcast an event snapshot to every active subscriber."""
        dead: list[asyncio.Queue[dict[str, Any]]] = []
        for q in self._subscribers:
            try:
                q.put_nowait(event)
            except asyncio.QueueFull:
                # Subscriber is too slow; drop and let the iOS client
                # reconnect with a fresh snapshot via /v1/canvas/processes.
                dead.append(q)
        for q in dead:
            with _safe_ignore(ValueError):
                self._subscribers.remove(q)

    def subscribe(self) -> asyncio.Queue[dict[str, Any]]:
        q: asyncio.Queue[dict[str, Any]] = asyncio.Queue(maxsize=256)
        self._subscribers.append(q)
        return q

    def unsubscribe(self, q: asyncio.Queue[dict[str, Any]]) -> None:
        with _safe_ignore(ValueError):
            self._subscribers.remove(q)


class BackgroundProcessRegistry:
    """Process-global registry. Use ``get_registry()`` to access."""

    def __init__(self) -> None:
        self._procs: dict[str, TrackedProcess] = {}
        self._lock = asyncio.Lock()
        self._gc_task: Optional[asyncio.Task[None]] = None

    async def start(self) -> None:
        if self._gc_task is None or self._gc_task.done():
            self._gc_task = asyncio.create_task(self._gc_loop(), name="bg-proc-gc")
            logger.info("background-process registry started")

    async def stop(self) -> None:
        if self._gc_task is not None:
            self._gc_task.cancel()
            with _safe_ignore(asyncio.CancelledError):
                await self._gc_task
            self._gc_task = None

    def register(
        self,
        *,
        command: list[str],
        name: str,
        proc: asyncio.subprocess.Process,
    ) -> TrackedProcess:
        record = TrackedProcess(
            id=str(uuid.uuid4()),
            name=name or (command[0] if command else "process"),
            command=list(command),
            pid=proc.pid,
        )
        record._proc = proc
        self._procs[record.id] = record
        record.publish(record.snapshot(stream="system", chunk=""))
        return record

    async def kill(self, process_id: str) -> bool:
        record = self._procs.get(process_id)
        if record is None or record._proc is None:
            return False
        proc = record._proc
        if proc.returncode is not None:
            return True
        try:
            proc.terminate()
        except ProcessLookupError:
            pass
        try:
            await asyncio.wait_for(proc.wait(), timeout=5.0)
        except asyncio.TimeoutError:
            with _safe_ignore(ProcessLookupError):
                proc.kill()
            with _safe_ignore(asyncio.CancelledError, Exception):
                try:
                    await proc.wait()
                except Exception:  # pragma: no cover
                    pass
        return True

    def list_snapshots(self) -> list[dict[str, Any]]:
        return [r.snapshot() for r in self._procs.values()]

    def get(self, process_id: str) -> Optional[TrackedProcess]:
        return self._procs.get(process_id)

    async def stream(self) -> AsyncIterator[dict[str, Any]]:
        """Yield one event snapshot per tracked-process change.

        Each subscriber starts with the current snapshot for every
        process, then receives deltas until the iterator is closed.
        A keepalive sentinel (``{"event": "keepalive"}``) is yielded
        every ``_KEEPALIVE_SEC`` so SSE proxies don't drop the
        connection on idle.
        """
        queues: list[tuple[TrackedProcess, asyncio.Queue[dict[str, Any]]]] = []
        async with self._lock:
            for proc in self._procs.values():
                queues.append((proc, proc.subscribe()))
            for proc, _ in queues:
                yield proc.snapshot()

        try:
            while True:
                wait_tasks: list[asyncio.Task[Any]] = []
                for _, q in queues:
                    wait_tasks.append(asyncio.create_task(q.get()))
                keepalive = asyncio.create_task(asyncio.sleep(_KEEPALIVE_SEC))
                pending = wait_tasks + [keepalive]
                done, pending = await asyncio.wait(
                    pending, return_when=asyncio.FIRST_COMPLETED
                )
                for t in pending:
                    t.cancel()
                emitted = False
                for t in wait_tasks:
                    if t in done:
                        event = t.result()
                        yield event
                        emitted = True
                if not emitted:
                    yield {"event": "keepalive"}
        finally:
            for proc, q in queues:
                proc.unsubscribe(q)

    async def attach_stdout_tail(self, record: TrackedProcess) -> None:
        proc = record._proc
        if proc is None or proc.stdout is None:
            return
        record._tail_task = asyncio.create_task(
            self._tail_loop(record), name=f"bg-proc-tail-{record.id}"
        )

    async def _tail_loop(self, record: TrackedProcess) -> None:
        proc = record._proc
        if proc is None:
            return
        try:
            if record.status == "starting":
                record.status = "running"
                record.publish(record.snapshot())
            assert proc.stdout is not None
            while True:
                try:
                    line = await proc.stdout.readline()
                except Exception:  # pragma: no cover
                    break
                if not line:
                    break
                try:
                    text = line.decode("utf-8", errors="replace")
                except Exception:  # pragma: no cover
                    text = repr(line)
                record._append_output(text)
                record.publish(record.snapshot(stream="stdout", chunk=text))
            if proc.stderr is not None:
                try:
                    err_bytes = await proc.stderr.read()
                except Exception:  # pragma: no cover
                    err_bytes = b""
                if err_bytes:
                    text = err_bytes.decode("utf-8", errors="replace")
                    record._append_output(text)
                    record.publish(record.snapshot(stream="stderr", chunk=text))
            with _safe_ignore(asyncio.CancelledError):
                rc = await proc.wait()
            record.exit_code = rc
            record.finished_at = _now_iso()
            if record.status not in {"killed", "failed"}:
                record.status = "completed" if rc == 0 else "failed"
            record._completed = True
            record.publish(record.snapshot(stream="system", chunk=""))
        except Exception:  # pragma: no cover
            logger.exception("background-process tail loop crashed for %s", record.id)
        finally:
            record._tail_task = None

    async def _gc_loop(self) -> None:
        while True:
            try:
                await asyncio.sleep(_GC_INTERVAL_SEC)
            except asyncio.CancelledError:
                return
            await self._gc_once()

    async def _gc_once(self) -> None:
        expired: list[str] = []
        for pid, record in list(self._procs.items()):
            if not record._completed:
                continue
            try:
                finished = datetime.fromisoformat(record.finished_at or _now_iso())
            except ValueError:
                expired.append(pid)
                continue
            age = (datetime.now(timezone.utc) - finished).total_seconds()
            if age >= _TERMINAL_RETENTION_SEC:
                expired.append(pid)
        for pid in expired:
            self._procs.pop(pid, None)
        if expired:
            logger.debug("evicted %d completed background processes", len(expired))


_REGISTRY: Optional[BackgroundProcessRegistry] = None


def get_registry() -> BackgroundProcessRegistry:
    global _REGISTRY
    if _REGISTRY is None:
        _REGISTRY = BackgroundProcessRegistry()
    return _REGISTRY


async def tracked_subprocess_exec(
    *,
    name: str,
    args: Iterable[str],
    **kwargs: Any,
) -> tuple[asyncio.subprocess.Process, TrackedProcess]:
    """Spawn a subprocess that is automatically visible in the Live tab.

    Mirrors ``asyncio.create_subprocess_exec`` but routes the
    resulting process through the background-process registry.  Use
    this anywhere a long-running command is spawned on behalf of
    the iOS app so the user can see its live stdout in the Canvas
    Live tab.
    """
    cmd = list(args)
    proc = await asyncio.create_subprocess_exec(*cmd, **kwargs)
    record = get_registry().register(command=cmd, name=name, proc=proc)
    await get_registry().attach_stdout_tail(record)
    return proc, record
