"""
Structured log tail and streaming.

Provides filtered, queryable access to Herald's logs via REST
and SSE endpoints. Supports tail mode (last N lines) and stream
mode (SSE of new log lines).

In production, reads from a log file configured via HERALD_LOG_FILE
env var. In development, falls back to reading recent Python log
records from an in-memory ring buffer.
"""

from __future__ import annotations

import asyncio
import logging
import os
import re
from collections import deque
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Optional

logger = logging.getLogger("herald.relay.log_service")

# Standard log levels in severity order
LEVEL_ORDER = {"DEBUG": 10, "INFO": 20, "WARNING": 30, "ERROR": 40, "CRITICAL": 50}


def _level_meets(min_level: str, record_level: str) -> bool:
    """Check if *record_level* meets or exceeds *min_level*."""
    return LEVEL_ORDER.get(record_level, 0) >= LEVEL_ORDER.get(min_level, 20)


class _RingBuffer(logging.Handler):
    """In-memory ring buffer for recent log records.

    Used as a fallback when no log file is configured.
    """

    def __init__(self, capacity: int = 2000):
        super().__init__()
        self._buffer: deque[logging.LogRecord] = deque(maxlen=capacity)

    def emit(self, record: logging.LogRecord) -> None:
        self._buffer.append(record)

    def tail(self, n: int = 200) -> list[logging.LogRecord]:
        items = list(self._buffer)
        return items[-n:]


class LogService:
    """Reads and streams Herald's structured logs.

    When HERALD_LOG_FILE is set, tails that file directly (filesystem-based,
    no Docker dependency). Otherwise falls back to an in-memory ring buffer
    of recent Python log records.
    """

    def __init__(self, settings):
        self.settings = settings
        self._log_file = os.getenv("HERALD_LOG_FILE", "").strip() or None

        # In-memory ring buffer as fallback / supplement
        self._ring = _RingBuffer(capacity=2000)
        self._ring.setLevel(logging.DEBUG)
        self._ring.setFormatter(
            logging.Formatter("%(asctime)s [%(levelname)s] %(name)s: %(message)s")
        )

        # Attach to the herald logger tree
        root = logging.getLogger("herald")
        root.addHandler(self._ring)

    # ── Tail ────────────────────────────────────────────────────────────

    async def tail(
        self,
        tail: int = 200,
        level: str = "INFO",
        since: Optional[str] = None,
    ) -> dict:
        """Return the last *tail* log lines, filtered by level and optional time.

        Args:
            tail: Number of lines to return (max 2000).
            level: Minimum log level.
            since: ISO-8601 timestamp or relative like "1h", "30m", "5m".
        """
        tail = min(max(tail, 1), 2000)

        since_dt: Optional[datetime] = None
        if since:
            since_dt = self._parse_since(since)

        if self._log_file and Path(self._log_file).exists():
            return await self._tail_file(tail, level, since_dt)
        else:
            return self._tail_ring(tail, level, since_dt)

    def _parse_since(self, raw: str) -> Optional[datetime]:
        """Parse a since parameter: ISO-8601 or relative like '1h'."""
        raw = raw.strip()
        # Relative: 1h, 30m, 5m, 24h, 7d
        m = re.match(r"^(\d+)\s*(h|m|d|s)$", raw.lower())
        if m:
            value = int(m.group(1))
            unit = m.group(2)
            if unit == "s":
                delta = timedelta(seconds=value)
            elif unit == "m":
                delta = timedelta(minutes=value)
            elif unit == "h":
                delta = timedelta(hours=value)
            elif unit == "d":
                delta = timedelta(days=value)
            else:
                delta = timedelta(hours=1)
            return datetime.now(timezone.utc) - delta

        # ISO-8601
        try:
            return datetime.fromisoformat(raw)
        except ValueError:
            return None

    async def _tail_file(
        self, tail: int, level: str, since: Optional[datetime]
    ) -> dict:
        """Tail from the configured log file."""
        try:
            with open(self._log_file, "r") as f:
                # Seek to roughly the last N lines without reading the whole file
                f.seek(0, os.SEEK_END)
                file_size = f.tell()
                # Estimate: ~200 bytes per line
                chunk_size = min(file_size, tail * 256)
                f.seek(max(0, file_size - chunk_size))
                # Skip partial first line
                if f.tell() > 0:
                    f.readline()
                lines = f.readlines()
        except Exception as exc:
            logger.warning("Failed to read log file: %s", exc)
            return {"lines": [], "count": 0, "tail": tail, "error": str(exc)}

        # Filter by level
        filtered: list[str] = []
        for line in lines:
            line = line.rstrip("\n\r")
            if not line:
                continue
            if level != "DEBUG":
                if not _level_meets(level, self._line_level(line)):
                    continue
            if since:
                line_ts = self._line_timestamp(line)
                if line_ts and line_ts < since:
                    continue
            filtered.append(line)

        return {"lines": filtered[-tail:], "count": len(filtered), "tail": tail}

    def _tail_ring(
        self, tail: int, level: str, since: Optional[datetime]
    ) -> dict:
        """Tail from the in-memory ring buffer."""
        records = self._ring.tail(tail * 4)  # Fetch more to account for filtering

        # Filter
        filtered: list[str] = []
        formatter = self._ring.formatter
        for record in records:
            if level != "DEBUG" and not _level_meets(level, record.levelname):
                continue
            if since:
                record_dt = datetime.fromtimestamp(
                    record.created, tz=timezone.utc
                )
                if record_dt < since:
                    continue
            if formatter:
                filtered.append(formatter.format(record))
            else:
                filtered.append(record.getMessage())

        return {"lines": filtered[-tail:], "count": len(filtered), "tail": tail}

    @staticmethod
    def _line_level(line: str) -> str:
        """Extract log level from a structured log line."""
        for lvl in ("CRITICAL", "ERROR", "WARNING", "INFO", "DEBUG"):
            if lvl in line:
                return lvl
        return "INFO"

    @staticmethod
    def _line_timestamp(line: str) -> Optional[datetime]:
        """Extract timestamp from a log line (best-effort)."""
        # Match common formats: 2026-07-25T14:32:01 or 2026-07-25 14:32:01
        m = re.search(
            r"(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2})", line
        )
        if m:
            try:
                return datetime.fromisoformat(m.group(1))
            except ValueError:
                pass
        return None

    # ── Stream ──────────────────────────────────────────────────────────

    async def stream(self, level: str = "INFO") -> asyncio.Queue:
        """Stream new log lines as they arrive.

        Returns a queue that receives formatted log lines in real-time.
        The caller subscribes and fans out to SSE/WS clients.
        """
        queue: asyncio.Queue = asyncio.Queue(maxsize=256)

        # Hook into the ring buffer by wrapping its emit
        original_emit = self._ring.emit

        def _streaming_emit(record: logging.LogRecord) -> None:
            original_emit(record)
            if level == "DEBUG" or _level_meets(level, record.levelname):
                formatted = (
                    self._ring.formatter.format(record)
                    if self._ring.formatter
                    else record.getMessage()
                )
                try:
                    queue.put_nowait(formatted)
                except asyncio.QueueFull:
                    pass  # Drop for slow consumers

        self._ring.emit = _streaming_emit  # type: ignore[method-assign]

        return queue
