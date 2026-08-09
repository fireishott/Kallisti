from __future__ import annotations

import asyncio
import logging
from typing import Union

logger = logging.getLogger(__name__)

# An item pushed to a subscriber queue: either a legacy wake signal (True)
# or a pre-formatted event dict with seq/type/payload for direct SSE delivery.
FanoutItem = Union[bool, dict]


class EventFanout:
    """In-memory fan-out for SSE subscribers with direct event push.

    Live events are pushed directly to subscriber queues as pre-formatted
    dicts (seq/type/payload) so the SSE writer can yield them without a DB
    round-trip.  The legacy ``wake(True)`` path still exists as a fallback;
    subscribers that receive ``True`` fall back to DB polling.

    Events are STILL persisted to the DB first (at-least-once durability),
    so cursor-based reconnect replays work correctly.
    """

    def __init__(self) -> None:
        self._queues: dict[str, list[asyncio.Queue]] = {}
        self._lock = asyncio.Lock()

    async def subscribe(self, job_id: str) -> asyncio.Queue:
        """Create a subscriber queue for a job. Returns the queue."""
        queue: asyncio.Queue = asyncio.Queue(maxsize=2048)
        async with self._lock:
            self._queues.setdefault(job_id, []).append(queue)
        return queue

    async def unsubscribe(self, job_id: str, queue: asyncio.Queue) -> None:
        """Remove a subscriber queue."""
        async with self._lock:
            queues = self._queues.get(job_id, [])
            if queue in queues:
                queues.remove(queue)
            if not queues:
                self._queues.pop(job_id, None)

    def wake(self, job_id: str) -> None:
        """Signal all subscribers that new events are available (legacy wake).

        Subscribers should fall back to DB polling when they receive ``True``.
        """
        self._broadcast(job_id, True)

    def push(self, job_id: str, event: dict) -> None:
        """Push a pre-formatted event directly to all subscribers.

        The *event* dict must contain ``seq``, ``type``, and ``payload`` keys.
        Subscribers can format and yield the SSE frame directly without a
        DB query, eliminating the DB-polling round-trip for live events.
        """
        self._broadcast(job_id, event)

    def _broadcast(self, job_id: str, item: FanoutItem) -> None:
        """Push an item to all subscriber queues for a job.

        Called from the event-loop thread (via ``call_soon_threadsafe`` when
        the producer is on a different thread).
        """
        queues = self._queues.get(job_id, [])
        if not queues:
            return
        for queue in queues:
            try:
                queue.put_nowait(item)
            except asyncio.QueueFull:
                pass
