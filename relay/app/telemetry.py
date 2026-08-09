"""
Gateway telemetry collection and broadcast.

Collects system stats, job metrics, connector health, and alerts.
Fans out to WebSocket subscribers and SSE clients.

Uses psutil for system stats and the existing job_events table for
performance metrics.
"""

from __future__ import annotations

import asyncio
import logging
import os
import time
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Optional

from sqlalchemy import func, text

logger = logging.getLogger("herald.relay.telemetry")

# ── Alert thresholds ────────────────────────────────────────────────────
SWAP_WARNING_GB = 4.0
MEMORY_CRITICAL_PCT = 90.0
CONNECTOR_HEARTBEAT_LOST_SECONDS = 60
ERROR_RATE_WARNING = 0.05  # 5% error rate triggers an alert


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


@dataclass
class TelemetrySnapshot:
    """Immutable snapshot of gateway state at a point in time."""

    gateway: dict
    connection: dict
    model: dict
    performance: dict
    sessions: dict
    system: dict
    network: dict
    alerts: list


class TelemetryService:
    """Collects, caches, and broadcasts gateway telemetry.

    Designed to be stored on app.state.telemetry and started during
    the FastAPI lifespan.
    """

    def __init__(
        self,
        settings,
        db_session_factory,
        *,
        get_connector_state=None,
        get_connector_version=None,
        get_hermes_version=None,
        get_hermes_profiles=None,
        get_active_profile=None,
        get_current_model=None,
        get_active_job_count=None,
        get_active_session_count=None,
        get_ws_connection_count=None,
        get_sse_connection_count=None,
    ):
        self.settings = settings
        self.db_factory = db_session_factory
        self._subscribers: list[asyncio.Queue] = []
        self._latest: Optional[TelemetrySnapshot] = None
        self._collector_task: Optional[asyncio.Task] = None
        self._start_time = time.time()
        self._alert_since: dict[str, float] = {}

        # Injectable callbacks so the service stays decoupled from main.py
        self._get_connector_state = get_connector_state or (lambda: "disconnected")
        self._get_connector_version = get_connector_version or (lambda: None)
        self._get_hermes_version = get_hermes_version or (lambda: None)
        self._get_hermes_profiles = get_hermes_profiles or (lambda: [])
        self._get_active_profile = get_active_profile or (lambda: None)
        self._get_current_model = get_current_model or (lambda: None)
        self._get_active_job_count = get_active_job_count or (lambda: 0)
        self._get_active_session_count = get_active_session_count or (lambda: 0)
        self._get_ws_connection_count = get_ws_connection_count or (lambda: 0)
        self._get_sse_connection_count = get_sse_connection_count or (lambda: 0)

    # ── Lifecycle ───────────────────────────────────────────────────────

    async def start(self, interval: float = 5.0) -> None:
        """Start periodic telemetry collection."""
        if self._collector_task is not None:
            return
        self._start_time = time.time()
        self._collector_task = asyncio.create_task(
            self._collect_loop(interval), name="telemetry-collector"
        )

    async def stop(self) -> None:
        """Stop the collector task."""
        if self._collector_task is not None:
            self._collector_task.cancel()
            try:
                await self._collector_task
            except asyncio.CancelledError:
                pass
            self._collector_task = None

    async def _collect_loop(self, interval: float) -> None:
        """Collect telemetry every *interval* seconds."""
        while True:
            try:
                snapshot = await self._collect()
                self._latest = snapshot
                await self._fanout(snapshot)
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception("Telemetry collection failed")
            await asyncio.sleep(interval)

    # ── Collection ──────────────────────────────────────────────────────

    async def _collect(self) -> TelemetrySnapshot:
        """Gather all telemetry data."""
        now = time.time()
        alerts: list[dict] = []

        # ── System stats ────────────────────────────────────────────────
        try:
            import psutil

            mem = psutil.virtual_memory()
            disk = psutil.disk_usage("/")
            cpu = psutil.cpu_percent(interval=None)
            swap_used_gb = psutil.swap_memory().used / (1024**3)
            memory_used_gb = mem.used / (1024**3)
            memory_total_gb = mem.total / (1024**3)
            disk_used_gb = disk.used / (1024**3)
            disk_total_gb = disk.total / (1024**3)

            # Network I/O
            net_io = psutil.net_io_counters()
            inbound_mbps = (net_io.bytes_recv / (1024**2)) if net_io else 0.0
            outbound_mbps = (net_io.bytes_sent / (1024**2)) if net_io else 0.0

            # Alerts
            if swap_used_gb > SWAP_WARNING_GB:
                key = "swap_high"
                if key not in self._alert_since:
                    self._alert_since[key] = now
                alerts.append(
                    {
                        "level": "warning",
                        "message": f"Swap usage at {swap_used_gb:.1f}GB (>{SWAP_WARNING_GB}GB threshold)",
                        "since": datetime.fromtimestamp(
                            self._alert_since[key], tz=timezone.utc
                        ).isoformat(),
                    }
                )
            else:
                self._alert_since.pop("swap_high", None)

            if mem.percent > MEMORY_CRITICAL_PCT:
                alerts.append(
                    {
                        "level": "critical",
                        "message": f"Memory usage at {mem.percent:.1f}%",
                    }
                )
        except ImportError:
            cpu = 0.0
            memory_used_gb = 0.0
            memory_total_gb = 0.0
            swap_used_gb = 0.0
            disk_used_gb = 0.0
            disk_total_gb = 0.0
            inbound_mbps = 0.0
            outbound_mbps = 0.0
        except Exception:
            logger.warning("System stat collection failed", exc_info=True)
            cpu = 0.0
            memory_used_gb = 0.0
            memory_total_gb = 0.0
            swap_used_gb = 0.0
            disk_used_gb = 0.0
            disk_total_gb = 0.0
            inbound_mbps = 0.0
            outbound_mbps = 0.0

        # ── Connector health ────────────────────────────────────────────
        connector_state = self._get_connector_state()
        if connector_state != "connected" and connector_state != "disconnected":
            connector_state = "disconnected"

        # ── Performance from DB ─────────────────────────────────────────
        perf = await self._compute_performance()

        # ── Connector heartbeat lost alert ──────────────────────────────
        # (only meaningful when we have a connector session with last_heartbeat tracking)
        if connector_state == "disconnected":
            key = "connector_lost"
            if key not in self._alert_since:
                self._alert_since[key] = now
            alerts.append(
                {
                    "level": "error",
                    "message": "Connector disconnected",
                    "since": datetime.fromtimestamp(
                        self._alert_since[key], tz=timezone.utc
                    ).isoformat(),
                }
            )
        else:
            self._alert_since.pop("connector_lost", None)

        return TelemetrySnapshot(
            gateway={
                "version": self.settings.version,
                "uptime_seconds": int(now - self._start_time),
                "started_at": datetime.fromtimestamp(
                    self._start_time, tz=timezone.utc
                ).isoformat(),
                "environment": self.settings.environment,
            },
            connection={
                "state": connector_state,
                "connector_version": self._get_connector_version(),
                "hermes_version": self._get_hermes_version(),
                "hermes_profiles": self._get_hermes_profiles(),
                "active_profile": self._get_active_profile(),
            },
            model={
                "name": self._get_current_model(),
            },
            performance=perf,
            sessions={
                "active": self._get_active_session_count(),
            },
            system={
                "cpu_percent": round(cpu, 1),
                "memory_used_gb": round(memory_used_gb, 1),
                "memory_total_gb": round(memory_total_gb, 1),
                "disk_used_gb": round(disk_used_gb, 1),
                "disk_total_gb": round(disk_total_gb, 1),
                "swap_used_gb": round(swap_used_gb, 1),
            },
            network={
                "inbound_mbps": round(inbound_mbps, 2),
                "outbound_mbps": round(outbound_mbps, 2),
                "active_ws_connections": self._get_ws_connection_count(),
                "active_sse_connections": self._get_sse_connection_count(),
            },
            alerts=alerts,
        )

    async def _compute_performance(self) -> dict:
        """Compute performance metrics from the job_events table."""
        db = self.db_factory()
        try:
            now = _utcnow()
            cutoff = now - timedelta(hours=24)

            # Active / queued jobs from message_jobs
            active = db.query(func.count(MessageJob.id)).filter(
                MessageJob.status == "processing"
            ).scalar() or 0
            queued = db.query(func.count(MessageJob.id)).filter(
                MessageJob.status == "queued"
            ).scalar() or 0

            # Jobs completed in last 24h
            from .models import MessageJob

            completed_24h = db.query(func.count(MessageJob.id)).filter(
                MessageJob.status.in_(["completed", "failed"]),
                MessageJob.created_at >= cutoff,
            ).scalar() or 0

            # Error rate: failed / total completed in 24h
            failed_24h = db.query(func.count(MessageJob.id)).filter(
                MessageJob.status == "failed",
                MessageJob.created_at >= cutoff,
            ).scalar() or 0
            # Also count errors in job_events
            event_errors_24h = db.query(func.count(JobEvent.id)).filter(
                JobEvent.type == "job.error",
                JobEvent.created_at >= cutoff,
            ).scalar() or 0
            total_completed = completed_24h + event_errors_24h
            error_rate = (failed_24h + event_errors_24h) / max(total_completed, 1)

            # TTFB: average time from job creation to first 'job.started' event
            from .models import JobEvent

            ttfb_rows = (
                db.query(
                    func.avg(
                        func.extract("epoch", JobEvent.created_at)
                        - func.extract("epoch", MessageJob.created_at)
                    )
                )
                .join(MessageJob, JobEvent.job_id == MessageJob.id)
                .filter(
                    JobEvent.type == "job.started",
                    JobEvent.created_at >= cutoff,
                )
                .first()
            )
            avg_ttfb_ms = round((ttfb_rows[0] or 0) * 1000) if ttfb_rows else 0

            return {
                "active_jobs": active,
                "queued_jobs": queued,
                "jobs_completed_24h": completed_24h,
                "avg_ttfb_ms": avg_ttfb_ms,
                "error_rate_24h": round(error_rate, 4),
            }
        except Exception:
            logger.warning("Performance metric query failed", exc_info=True)
            return {
                "active_jobs": 0,
                "queued_jobs": 0,
                "jobs_completed_24h": 0,
                "avg_ttfb_ms": 0,
                "error_rate_24h": 0.0,
            }
        finally:
            db.close()

    # ── Subscriber management ───────────────────────────────────────────

    async def subscribe(self) -> asyncio.Queue:
        """Subscribe to telemetry pushes. Returns a queue receiving snapshots."""
        queue: asyncio.Queue = asyncio.Queue(maxsize=32)
        self._subscribers.append(queue)
        # Send latest snapshot immediately if available
        if self._latest is not None:
            try:
                queue.put_nowait(self._latest)
            except asyncio.QueueFull:
                pass
        return queue

    async def unsubscribe(self, queue: asyncio.Queue) -> None:
        """Unsubscribe from telemetry pushes."""
        try:
            self._subscribers.remove(queue)
        except ValueError:
            pass

    async def _fanout(self, snapshot: TelemetrySnapshot) -> None:
        """Push a snapshot to all subscribers."""
        dead: list[asyncio.Queue] = []
        for queue in self._subscribers:
            try:
                queue.put_nowait(snapshot)
            except asyncio.QueueFull:
                # Drop oldest and retry for slow subscribers
                try:
                    queue.get_nowait()
                    queue.put_nowait(snapshot)
                except (asyncio.QueueEmpty, asyncio.QueueFull):
                    pass
            except Exception:
                dead.append(queue)
        for queue in dead:
            try:
                self._subscribers.remove(queue)
            except ValueError:
                pass

    def snapshot(self) -> Optional[TelemetrySnapshot]:
        """Get the latest snapshot (non-blocking)."""
        return self._latest
