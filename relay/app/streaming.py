from __future__ import annotations

import asyncio
import logging

logger = logging.getLogger(__name__)


class EventFanout:
    """In-memory fan-out for SSE subscribers.

    Events come from the DB now; this is just the wake-up signal.
    Replaces the destructive pop()-based buffer that lived in main.py.
    """

    def __init__(self) -> None:
        self._queues: dict[str, list[asyncio.Queue]] = {}
        self._lock = asyncio.Lock()

    async def subscribe(self, job_id: str) -> asyncio.Queue:
        """Create a subscriber queue for a job. Returns the queue."""
        queue: asyncio.Queue = asyncio.Queue(maxsize=1024)
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
        """Signal all subscribers for a job that new events are available (no payload)."""
        queues = self._queues.get(job_id, [])
        for queue in queues:
            try:
                queue.put_nowait(True)  # Just a wake signal, not the event data
            except asyncio.QueueFull:
                pass
