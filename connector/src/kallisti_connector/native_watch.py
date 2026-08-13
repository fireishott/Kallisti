"""Native-completion to push bridge.

The iOS app submits ``prompt.submit`` directly to Hermes in the native
world, so the connector needs its own watcher to notice turn completions
and fire APNs / Live Activity pushes.  This module provides:

* ``NativeWatchRegistry`` -- maps session_ids to device push tokens.
* ``NativeWatchTokens`` -- self-refreshing access token for the native
  gateway WebSocket.
* ``run_watcher`` -- long-running asyncio task that holds one persistent
  WS connection to the native gateway and fires pushes on terminal events.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import time
from pathlib import Path

logger = logging.getLogger(__name__)

CONFIG_PATH = Path(
    os.environ.get(
        "KALLISTI_NATIVE_WATCH_CONFIG",
        str(Path.home() / ".config" / "kallisti-native-watch.json"),
    )
)

# The connector and the native gateway run on the same host, so loopback is
# both the correct default and the safest -- this traffic should never leave
# the machine. Overridable for split deployments; not a baked-in LAN IP.
NATIVE_GATEWAY_HOST = os.environ.get("KALLISTI_NATIVE_GATEWAY_HOST", "127.0.0.1")
NATIVE_GATEWAY_PORT = int(os.environ.get("KALLISTI_NATIVE_GATEWAY_PORT", "9119"))


class NativeWatchRegistry:
    """session_id -> [(device_token, token_kind), ...]. In-memory only."""

    def __init__(self) -> None:
        self._watchers: dict[str, list[tuple[str, str]]] = {}

    def watch(
        self, *, session_id: str, device_token: str, token_kind: str
    ) -> None:
        entries = self._watchers.setdefault(session_id, [])
        if (device_token, token_kind) not in entries:
            entries.append((device_token, token_kind))

    def watchers_for(self, session_id: str) -> list[tuple[str, str]]:
        return list(self._watchers.get(session_id, []))

    def unwatch(
        self, *, session_id: str, device_token: str, token_kind: str
    ) -> None:
        """Remove a specific watcher entry.  Cleans up the session key if
        the last watcher for that session is removed."""
        entries = self._watchers.get(session_id)
        if entries is None:
            return
        try:
            entries.remove((device_token, token_kind))
        except ValueError:
            return
        if not entries:
            del self._watchers[session_id]

    def clear_session(self, *, session_id: str) -> None:
        """Build 72: drop every watcher for a session after its turn went
        terminal via polling, so a finished session is never re-fired."""
        self._watchers.pop(session_id, None)


class NativeWatchTokens:
    """Self-rotating access token from refresh_token stored in CONFIG_PATH."""

    def __init__(self, session) -> None:
        self._session = session
        self._access_token: str | None = None
        self._refresh_token: str | None = None
        self._load()

    def _load(self) -> None:
        try:
            data = json.loads(CONFIG_PATH.read_text())
        except FileNotFoundError:
            logger.warning(
                "Native watch config not found at %s — "
                "watcher will start but cannot connect until "
                "the one-time login script is run.",
                CONFIG_PATH,
            )
            return
        self._refresh_token = data.get("refresh_token")

    @property
    def is_configured(self) -> bool:
        """False until the one-time login has stored a refresh token."""
        return bool(self._refresh_token)

    def invalidate_access_token(self) -> None:
        """Force the next access_token() call to refresh. Called when the
        gateway rejects the current token, so a stale one isn't retried
        for the life of the process."""
        self._access_token = None

    async def access_token(self) -> str:
        """Return a valid access token, refreshing lazily on first use.

        KNOWN LIMITATION (Build 1): tokens are only refreshed when the
        cached value is None (first call or after an explicit reset).
        If the token expires between WS reconnects, the reconnect loop
        will burn 1-30s of backoff before the next attempt refreshes.
        A future build should track the refresh timestamp and proactively
        refresh when the token approaches expiry (e.g. at 50 minutes).
        """
        if self._access_token is None:
            if self._refresh_token is None:
                raise RuntimeError(
                    "Native watch cannot authenticate — "
                    f"no refresh token loaded from {CONFIG_PATH}. "
                    "Run the one-time login script first."
                )
            await self._refresh()
        return self._access_token

    async def _refresh(self) -> None:
        url = (
            f"http://{NATIVE_GATEWAY_HOST}:{NATIVE_GATEWAY_PORT}"
            "/auth/native/refresh"
        )
        # httpx: post() is awaited and returns a Response. The aiohttp
        # `async with session.post(...) as resp` idiom raises TypeError here
        # ('coroutine' has no __aexit__), and `.status`/`await .json()` are
        # likewise aiohttp spellings -- httpx uses .status_code / .json().
        resp = await self._session.post(
            url, json={"refresh_token": self._refresh_token}
        )
        if resp.status_code != 200:
            raise RuntimeError(
                f"native watch token refresh failed: {resp.status_code}"
            )
        body = resp.json()
        self._access_token = body["access_token"]
        self._refresh_token = body["refresh_token"]
        # PERSIST the rotated refresh token. The gateway rotates refresh
        # tokens on every refresh and Portal runs reuse-detection, so the old
        # token is dead after this call. If we only updated memory, the next
        # connector restart would load the revoked token from CONFIG_PATH and
        # 401 forever. Write the new one back so restarts stay healthy.
        try:
            import json as _json
            import os

            CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
            tmp = CONFIG_PATH.with_suffix(".json.tmp")
            tmp.write_text(_json.dumps({"refresh_token": self._refresh_token}))
            os.replace(tmp, CONFIG_PATH)
            os.chmod(CONFIG_PATH, 0o600)
        except Exception:
            logger.warning(
                "native watch: rotated refresh token could not be persisted",
                exc_info=True,
            )


# Build 72: session.status poll cadence. The gateway routes turn.end /
# turn.error ONLY to the session-owning transport (tui_gateway/server.py
# write_json), which in native mode is the iOS app's WS - never this watcher's
# separate peer. Polling session.status (read-only, works for any live session)
# is the reliable terminal detection for native turns.
POLL_INTERVAL_SECS = float(os.environ.get("KALLISTI_NATIVE_WATCH_POLL_SECS", "4"))
_POLL_AGENT_RUNNING_RE = re.compile(r"Agent Running:\s*(Yes|No)", re.IGNORECASE)


def _session_status_is_running(output: str) -> bool | None:
    """Parse session.status text output. True=still running, False=idle,
    None=unparseable (caller should keep polling)."""
    m = _POLL_AGENT_RUNNING_RE.search(output or "")
    if not m:
        return None
    return m.group(1).strip().lower() == "yes"


async def run_watcher(
    registry: NativeWatchRegistry,
    tokens: NativeWatchTokens,
    send_completion_push,
    end_live_activity,
    terminal_event_types: tuple[str, ...] = ("turn.end", "turn.error"),
) -> None:
    """Long-running background task -- start once at connector boot.

    Holds one persistent WebSocket connection to the native gateway and
    fires pushes when a terminal event matches a watched session.

    ``terminal_event_types`` is confirmed against ``tui_gateway/server.py``:
    the gateway's ``_emit`` call sites produce ``turn.end`` on completion
    and ``turn.error`` on failure. There is no ``turn.complete`` event --
    an earlier version of this module matched that name and therefore
    never fired a single push.

    Frame shape, from ``_event_frame`` in the same module::

        {"jsonrpc": "2.0", "method": "event",
         "params": {"type": ..., "session_id": ..., "payload": {...}}}
    """
    import websockets  # local import to keep module loadable without websockets dep

    backoff = 1.0
    warned_unconfigured = False
    while True:
        if not tokens.is_configured:
            # Expected state until the one-time login has been run. Say so
            # once, then idle quietly -- this used to raise and log a full
            # traceback every few seconds forever.
            if not warned_unconfigured:
                logger.warning(
                    "Native watch idle: no credential at %s. "
                    "Push notifications for native-gateway turns are off "
                    "until the one-time login script is run.",
                    CONFIG_PATH,
                )
                warned_unconfigured = True
            await asyncio.sleep(60)
            continue
        warned_unconfigured = False
        try:
            access_token = await tokens.access_token()
            ticket_url = (
                f"http://{NATIVE_GATEWAY_HOST}:{NATIVE_GATEWAY_PORT}"
                "/api/auth/ws-ticket"
            )
            ticket_resp = await tokens._session.post(
                ticket_url,
                headers={"Authorization": f"Bearer {access_token}"},
            )
            if ticket_resp.status_code != 200:
                # A 401 here means the access token went stale; drop it so
                # the next pass refreshes rather than looping on a dead one.
                if ticket_resp.status_code == 401:
                    tokens.invalidate_access_token()
                raise RuntimeError(
                    f"ws-ticket mint failed: HTTP {ticket_resp.status_code}"
                )
            ticket = ticket_resp.json()["ticket"]

            uri = (
                f"ws://{NATIVE_GATEWAY_HOST}:{NATIVE_GATEWAY_PORT}"
                f"/api/ws?ticket={ticket}"
            )
            async with websockets.connect(uri) as ws:
                backoff = 1.0

                async def fire_terminal(session_id: str) -> None:
                    """Shared terminal handling: push each watcher + remote-end
                    the Live Activity unconditionally (Build 34 behaviour)."""
                    watchers = registry.watchers_for(session_id)
                    if watchers:
                        logger.info(
                            "Native turn terminal for session=%s — notifying %d device(s)",
                            session_id,
                            len(watchers),
                        )
                    for device_token, token_kind in watchers:
                        try:
                            if token_kind == "liveActivity":
                                await end_live_activity(
                                    device_token, status="completed"
                                )
                            else:
                                await send_completion_push(
                                    device_token,
                                    session_id=session_id,
                                )
                        except Exception:
                            logger.exception(
                                "push delivery failed for session=%s "
                                "device=%s",
                                session_id,
                                device_token[:8],
                            )
                    # Build 34 (fix): remote-end the Live Activity / Dynamic
                    # Island on every terminal event, even when the app only
                    # registered an "alert" watch (which is all it does
                    # today).  The end_live_activity adapter resolves the
                    # ActivityKit push token from connector state, so this is
                    # safe to call unconditionally: no token, no activity, or
                    # an already-ended activity are all silent no-ops.  Before
                    # this, a backgrounded or killed app left the lock screen
                    # stuck on "Thinking..." forever because the only watcher
                    # kind the app registers never triggered the end-push.
                    try:
                        await end_live_activity(
                            "state-resolved-token", status="completed"
                        )
                    except Exception:
                        logger.debug(
                            "live activity end-push error (non-fatal)",
                            exc_info=True,
                        )

                # Build 72: poll every watched session's status so native turns
                # (whose terminal events never reach this peer) still get
                # remote-ended. Responses come back as JSON-RPC frames with the
                # matching id in the main read loop below.
                #
                # Build 73 fix (leak guard): pending_polls grew unbounded when
                # the gateway WS wedged (responses never arrived) and registry
                # entries never cleared, so every cycle polled MORE dead
                # sessions -> 12k-msg flood + 700MB RSS. Caps + expiry below.
                poll_counter = 0
                pending_polls: dict[str, str] = {}  # rpc id -> session_id
                pending_poll_time: dict[str, float] = {}  # rpc id -> monotonic sent
                MAX_PENDING_POLLS = 100
                POLL_ANSWER_TIMEOUT_S = 30.0
                MAX_UNANSWERED_CYCLES = 3
                # Per-session consecutive-unanswered counter; sessions that stop
                # answering get remote-ended and dropped so they don't pile up.
                unanswered: dict[str, int] = {}
                # Per-session last observed `Agent Running` value (True/False).
                # Used to dedupe terminal fires: a push is sent only on the
                # first True→False transition (or the first False observation
                # when we never saw True) so iOS re-registering the watch for
                # a follow-up turn in the same session doesn't spam a duplicate
                # 'Turn complete' notification. Cleared on WS reconnect so a
                # fresh watcher doesn't inherit stale state from before the
                # drop.
                last_observed_running: dict[str, bool] = {}

                async def poll_loop() -> None:
                    nonlocal poll_counter
                    while True:
                        try:
                            await asyncio.sleep(POLL_INTERVAL_SECS)
                            now = time.monotonic()
                            # Expire stale unanswered polls (gateway wedged or
                            # session gone) and count them against the session.
                            for rid in list(pending_poll_time):
                                if now - pending_poll_time[rid] > POLL_ANSWER_TIMEOUT_S:
                                    sid = pending_polls.pop(rid, None)
                                    pending_poll_time.pop(rid, None)
                                    if sid is None:
                                        continue
                                    unanswered[sid] = unanswered.get(sid, 0) + 1
                                    if unanswered[sid] >= MAX_UNANSWERED_CYCLES:
                                        # Skip if we already pushed for this
                                        # terminal (e.g. a prior successful
                                        # poll observed the True→False
                                        # transition before the gateway went
                                        # silent).
                                        if last_observed_running.get(sid) is False:
                                            unanswered.pop(sid, None)
                                            continue
                                        logger.warning(
                                            "Native watch poll: session=%s unresponsive "
                                            "(%d unanswered) - remote-ending",
                                            sid,
                                            unanswered[sid],
                                        )
                                        await fire_terminal(sid)
                                        registry.clear_session(session_id=sid)
                                        unanswered.pop(sid, None)
                                        last_observed_running[sid] = False
                            for sid in list(registry._watchers.keys()):
                                # Already being dropped by the expiry above.
                                if unanswered.get(sid, 0) >= MAX_UNANSWERED_CYCLES:
                                    continue
                                # Response pipe is stuck: stop growing the dict
                                # instead of leaking another entry per cycle.
                                if len(pending_polls) >= MAX_PENDING_POLLS:
                                    break
                                poll_counter += 1
                                rid = f"watch-poll-{poll_counter}"
                                pending_polls[rid] = sid
                                pending_poll_time[rid] = time.monotonic()
                                await ws.send(
                                    json.dumps(
                                        {
                                            "jsonrpc": "2.0",
                                            "id": rid,
                                            "method": "session.status",
                                            "params": {"session_id": sid},
                                        }
                                    )
                                )
                        except Exception:
                            logger.debug(
                                "native watch poll send failed (non-fatal)",
                                exc_info=True,
                            )

                poll_task = asyncio.create_task(poll_loop())
                try:
                    async for raw in ws:
                        try:
                            frame = json.loads(raw)
                        except (ValueError, TypeError):
                            continue
                        if not isinstance(frame, dict):
                            continue

                        # JSON-RPC response to one of our status polls.
                        rpc_id = frame.get("id")
                        if rpc_id in pending_polls:
                            sid = pending_polls.pop(rpc_id)
                            pending_poll_time.pop(rpc_id, None)
                            unanswered.pop(sid, None)  # answered — reset counter
                            error = frame.get("error")
                            result = frame.get("result") or {}
                            output = (
                                result.get("output") if isinstance(result, dict) else ""
                            )
                            running = None if error else _session_status_is_running(output)
                            # Bug fix: previously fired a push on every
                            # `running=False` observation, which spammed the
                            # lock screen whenever iOS re-registered the watch
                            # for a follow-up turn in the same session (the
                            # gap between iOS submitting a prompt and the
                            # gateway flipping `running=True` reads as idle
                            # and was triggering a duplicate 'Turn complete'
                            # push). Require an actual True→False transition
                            # (or a session we never observed running) so we
                            # fire once per real turn end.
                            if running is True:
                                last_observed_running[sid] = True
                                continue
                            if running is False or error is not None:
                                prev = last_observed_running.get(sid)
                                # Fire only on True→False, or on the first
                                # observation of idle (session already ended
                                # before the watcher started polling — iOS
                                # likely has the result via its own WS, but
                                # firing once here covers the missed-window
                                # case). Skip repeated False observations
                                # (last_observed already False) so a
                                # re-registration for a follow-up turn
                                # doesn't fire prematurely.
                                if prev is False:
                                    logger.info(
                                        "Native watch poll: session=%s still idle "
                                        "(running=False err=%s) — skip duplicate",
                                        sid,
                                        bool(error),
                                    )
                                else:
                                    logger.info(
                                        "Native watch poll: session=%s terminal "
                                        "(running=%s err=%s)",
                                        sid,
                                        running,
                                        bool(error),
                                    )
                                    await fire_terminal(sid)
                                    registry.clear_session(session_id=sid)
                                last_observed_running[sid] = False
                            continue

                        params = frame.get("params")
                        if (
                            not isinstance(params, dict)
                            or params.get("type") not in terminal_event_types
                        ):
                            continue
                        session_id = params.get("session_id")
                        if not session_id:
                            continue
                        # Same dedupe as the poll path: the WS-routed
                        # turn.end/turn.error event carries no
                        # running-flag history of its own, so key on the
                        # session's last observed running state and skip
                        # if we've already pushed for this terminal. The
                        # gateway normally routes these events to the
                        # iOS-owning transport rather than this peer
                        # (see Build 72 comment near run_watcher), but
                        # keep the guard so a future routing change
                        # can't reintroduce duplicates.
                        if last_observed_running.get(session_id) is False:
                            logger.info(
                                "Native watch WS terminal for session=%s already fired — skip duplicate",
                                session_id,
                            )
                            continue
                        last_observed_running[session_id] = False
                        await fire_terminal(session_id)
                finally:
                    poll_task.cancel()
        except Exception:
            logger.exception(
                "native watch connection dropped, reconnecting in %.1fs",
                backoff,
            )
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 30.0)
