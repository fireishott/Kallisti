"""
Gateway lifecycle control: restart, update, config reload.

Exposes restart/update/config operations as both REST endpoints
and a WebSocket control channel. Uses the same patterns as the
existing connector management but adds structured progress reporting.

IMPORTANT: The self-restart path (restart relay) acknowledges the
command THEN signals shutdown — the client knows the restart is in
flight when the connection closes cleanly. It reconnects after
the estimated restart window.
"""

from __future__ import annotations

import asyncio
import logging
import os
import signal
import time
from typing import Optional

logger = logging.getLogger("herald.relay.gateway_control")

RESTART_ESTIMATED_SECONDS = 15


class GatewayController:
    """Controls the Herald relay, connector, and Hermes agent processes."""

    def __init__(
        self,
        settings,
        *,
        connector_rpc=None,
        active_job_count=None,
        get_broadcaster=None,
    ):
        self.settings = settings
        self._restart_lock = asyncio.Lock()
        self._start_time = time.time()

        # Injectable callbacks
        self._connector_rpc = connector_rpc
        self._active_job_count = active_job_count or (lambda: 0)
        self._get_broadcaster = get_broadcaster

    # ── Restart ────────────────────────────────────────────────────────

    async def restart(self, target: str = "relay") -> dict:
        """Gracefully restart a component.

        Args:
            target: "relay" | "connector" | "hermes"

        Returns:
            dict with restart outcome. For target="relay", this may
            never actually return — the process terminates.
        """
        async with self._restart_lock:
            if target == "relay":
                return await self._restart_relay()
            elif target == "connector":
                return await self._restart_connector()
            elif target == "hermes":
                return await self._restart_hermes()
            else:
                raise ValueError(f"Unknown restart target: {target}")

    async def _restart_relay(self) -> dict:
        """Graceful relay restart via SIGTERM.

        Sequence:
        1. Push restart_progress to broadcasters
        2. Sleep briefly for frame flush
        3. SIGTERM → Docker/process manager restarts the container
        """
        # Push progress to any connected control WS clients
        broadcaster = self._get_broadcaster() if self._get_broadcaster else None
        if broadcaster:
            jobs_remaining = self._active_job_count()
            try:
                await broadcaster({
                    "type": "restart_progress",
                    "stage": "draining",
                    "target": "relay",
                    "jobs_remaining": jobs_remaining,
                    "estimated_restart_seconds": RESTART_ESTIMATED_SECONDS,
                })
            except Exception:
                logger.debug("Broadcaster push failed during restart", exc_info=True)

        # Allow time for the frame to flush to clients
        await asyncio.sleep(0.5)

        # Schedule SIGTERM — uvicorn handles graceful drain
        loop = asyncio.get_running_loop()
        loop.call_later(0.5, lambda: os.kill(os.getpid(), signal.SIGTERM))

        return {
            "restarting": True,
            "target": "relay",
            "estimated_restart_seconds": RESTART_ESTIMATED_SECONDS,
            "message": "Shutdown initiated; process manager will restart.",
        }

    async def _restart_connector(self) -> dict:
        """Restart the connector process via connector RPC."""
        if not self._connector_rpc:
            return {
                "restarting": False,
                "target": "connector",
                "error": "No connector RPC available",
            }
        try:
            result = await self._connector_rpc("connector.restart", timeout_seconds=10.0)
            return {"restarting": True, "target": "connector", "result": result}
        except Exception as exc:
            logger.warning("Connector restart RPC failed: %s", exc)
            return {"restarting": False, "target": "connector", "error": str(exc)}

    async def _restart_hermes(self) -> dict:
        """Restart the Hermes agent via connector RPC."""
        if not self._connector_rpc:
            return {
                "restarting": False,
                "target": "hermes",
                "error": "No connector RPC available",
            }
        try:
            result = await self._connector_rpc("hermes.restart", timeout_seconds=10.0)
            return {"restarting": True, "target": "hermes", "result": result}
        except Exception as exc:
            logger.warning("Hermes restart RPC failed: %s", exc)
            return {"restarting": False, "target": "hermes", "error": str(exc)}

    # ── Update ──────────────────────────────────────────────────────────

    async def update_check(self) -> dict:
        """Check GitHub for newer Herald releases."""
        import json
        import urllib.error
        import urllib.request

        current = self.settings.version
        try:
            req = urllib.request.Request(
                f"https://api.github.com/repos/{os.environ.get('GITHUB_REPO', 'owner/repo')}/releases/latest",
                headers={"Accept": "application/vnd.github+json"},
            )
            with urllib.request.urlopen(req, timeout=10.0) as resp:
                latest = json.loads(resp.read().decode())
            latest_tag = (latest.get("tag_name") or "").lstrip("v")
            available = latest_tag != current

            return {
                "available": available,
                "current": current,
                "latest": latest_tag,
                "url": latest.get("html_url", ""),
                "body": (latest.get("body") or "")[:500],
            }
        except Exception as exc:
            logger.warning("Update check failed: %s", exc)
            return {
                "available": False,
                "current": current,
                "error": str(exc),
            }

    async def update(self) -> dict:
        """Signal that an update is requested.

        Writes an update-requested sentinel file that a host-side
        watcher (cron/systemd timer) can pick up to run docker-compose
        pull + up. The relay container itself cannot self-update
        without the Docker socket.
        """
        sentinel = os.environ.get("HERALD_UPDATE_SENTINEL", "/tmp/herald-update-requested")
        try:
            with open(sentinel, "w") as f:
                f.write(str(int(time.time())))
            return {
                "updating": True,
                "message": "Update requested. Host-side watcher will pull and restart.",
            }
        except Exception as exc:
            logger.warning("Failed to write update sentinel: %s", exc)
            return {
                "updating": False,
                "error": f"Cannot write update sentinel: {exc}",
            }

    # ── Model switch ────────────────────────────────────────────────────

    async def model_switch(self, model: str) -> dict:
        """Switch the active model via connector RPC."""
        if not self._connector_rpc:
            return {"switched": False, "error": "No connector RPC available"}
        try:
            result = await self._connector_rpc(
                "model.set",
                params={"model": model},
                timeout_seconds=10.0,
            )
            return {"switched": True, "model": model, "result": result}
        except Exception as exc:
            logger.warning("Model switch RPC failed: %s", exc)
            return {"switched": False, "model": model, "error": str(exc)}

    async def profile_switch(self, profile: str) -> dict:
        """Switch the active Hermes profile via connector RPC.

        This calls ``profile.set`` on the connector, which writes the new
        profile name to ``config.yaml`` and restarts the Hermes gateway
        so the new profile takes effect immediately.
        """
        if not self._connector_rpc:
            return {"switched": False, "error": "No connector RPC available"}
        try:
            result = await self._connector_rpc(
                "profile.set",
                params={"name": profile},
                timeout_seconds=15.0,
            )
            return {"switched": True, "profile": profile, "result": result}
        except Exception as exc:
            logger.warning("Profile switch RPC failed: %s", exc)
            return {"switched": False, "profile": profile, "error": str(exc)}

    # ── Config reload ───────────────────────────────────────────────────

    async def config_reload(self) -> dict:
        """Reload mutable runtime configuration from environment.

        Since Settings is a frozen dataclass, this reads env vars
        for the subset of fields that are safe to change at runtime
        and returns what changed.
        """
        import os

        changed: dict[str, tuple[str, str]] = {}
        reloadable = {
            "HERALD_LOG_LEVEL": "log_level",
            "HERALD_TELEMETRY_INTERVAL": "telemetry_interval",
            "SSE_KEEPALIVE_SECONDS": "sse_keepalive_seconds",
            "CONNECTOR_RPC_TIMEOUT_SECONDS": "connector_rpc_timeout_seconds",
        }

        for env_key, field_name in reloadable.items():
            new_val = os.getenv(env_key, "").strip()
            if new_val:
                old_val = str(getattr(self.settings, field_name, ""))
                if new_val != old_val:
                    changed[field_name] = (old_val, new_val)

        if changed:
            logger.info("Config reload: changed %s", list(changed.keys()))
        else:
            logger.info("Config reload: no changes detected")

        return {
            "reloaded": True,
            "changed": {k: {"old": v[0], "new": v[1]} for k, v in changed.items()},
        }
