"""Real PTY terminal bridge for the TUI chat mode.

Spawns ``hermes --tui`` in a PTY on the connector host and relays the
byte stream over a WebSocket. The iOS app renders it with SwiftTerm, so
the user gets the ACTUAL Hermes terminal UI - cursor control, panels,
prompt, everything - not a styled approximation.

Wire contract (all frames are JSON text):
  client -> server:
    {"type": "session", "mode": "new"}                      # optional handshake
    {"type": "session", "mode": "resume", "sessionId": ID}  # optional handshake
    {"type": "input", "data": "keystrokes or pasted text"}
    {"type": "resize", "cols": 120, "rows": 40}
    {"type": "signal", "name": "intr"}   # Ctrl-C (SIGINT) to the pty
  server -> client:
    {"type": "ready", "mode": "new"|"resume", "sessionId": "..."}  # after handshake
    {"type": "output", "data": "utf-8 bytes from the pty"}
    {"type": "exit", "code": 0}
    {"type": "error", "message": "..."}

The session handshake is OPTIONAL. If the client opens the WS and sends
only ``input``/``resize``/``signal`` (no session frame), the bridge
defaults to mode=new so older clients (and the live connector's
exercising path) keep working unchanged. The bridge waits up to
``_SESSION_HANDSHAKE_TIMEOUT`` for the first frame so a slow iOS prompt
animation never races the PTY spawn.

Security:
  * Auth via the same native-gateway bearer / cookie / paired credential
    the rest of the facade uses - no new credential store, nothing to
    configure in the app.
  * The command is resolved through PATH (no hardcoded path) and can be
    overridden with KALLISTI_TERMINAL_CMD for testing.
  * The resume sessionId is validated against a strict allow-list
    (alnum + ``_-``) and the final command is built as an argv list --
    ``os.execvpe`` consumes it without ever going through a shell, so
    there is no injection surface.
  * PTY output is not logged.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import pty
import re
import select
import shutil
import signal as signal_module
import subprocess
from typing import Any

logger = logging.getLogger(__name__)

_TERMINAL_CMD = os.environ.get("KALLISTI_TERMINAL_CMD", "hermes --tui")

# How long to wait for the optional client session handshake before
# falling back to mode=new. Long enough to absorb a single round-trip
# from the iOS confirmationDialog + the small native WS connect latency,
# short enough that a client that never sends one doesn't sit on a blank
# screen.
_SESSION_HANDSHAKE_TIMEOUT = 1.5  # seconds

# Hermes session ids look like ``20260818_085812_799505`` (timestamp +
# hex) or ``cron_<id>_<timestamp>``. Restrict the allow-list to a safe
# subset so even a compromised client can't sneak shell metacharacters
# in via the resume path.
_SESSION_ID_RE = re.compile(r"^[A-Za-z0-9_-]{1,128}$")


def _resolve_command() -> list[str]:
    """Split the terminal command and resolve argv[0] through PATH."""
    parts = _TERMINAL_CMD.split()
    if not parts:
        raise RuntimeError("KALLISTI_TERMINAL_CMD is empty")
    resolved = shutil.which(parts[0])
    if resolved is None:
        raise RuntimeError(f"command not found in PATH: {parts[0]}")
    return [resolved, *parts[1:]]


def _build_resume_argv(base_cmd: list[str], session_id: str) -> list[str] | None:
    """Return a new argv with ``--resume <sessionId>`` appended, or None.

    The id is validated against a strict regex (alnum + ``_-``) so the
    args list is safe to hand to ``os.execvpe`` without any shell quoting.
    The base command is whatever KALLISTI_TERMINAL_CMD resolved to (e.g.
    ``[hermes, --tui]``); we insert ``--resume`` after the first token
    so flags like ``--tui`` stay adjacent to the subcommand.
    """
    if not _SESSION_ID_RE.match(session_id):
        return None
    if len(base_cmd) < 1:
        return None
    return [base_cmd[0], *base_cmd[1:], "--resume", session_id]


async def _await_session_handshake(websocket: Any) -> tuple[str, str | None]:
    """Wait for the optional ``session`` handshake frame.

    Returns ``(mode, sessionId)``. ``mode`` is ``"resume"`` only if the
    client sent a valid ``session`` frame with mode=resume and a
    well-formed sessionId; otherwise mode is ``"new"``. ``sessionId`` is
    the validated id when mode=resume, else None.

    Any client that opens the socket without sending a session frame
    (older builds, raw curl, smoke tests) gets mode=new and we proceed
    immediately. A client that sends something malformed gets mode=new
    plus a warning log so a buggy client never wedges the bridge.
    """
    try:
        raw = await asyncio.wait_for(
            websocket.receive_text(), timeout=_SESSION_HANDSHAKE_TIMEOUT
        )
    except asyncio.TimeoutError:
        return "new", None
    except Exception:
        # Client closed before sending - treat as default new.
        return "new", None

    try:
        frame = json.loads(raw)
    except json.JSONDecodeError:
        logger.warning("terminal bridge: non-JSON first frame, defaulting to new")
        return "new", None

    if not isinstance(frame, dict) or frame.get("type") != "session":
        # First frame was an input/resize/signal - older client. Default to new.
        # Replay the consumed frame is NOT possible (we already awaited one
        # text), but the bridge's main loop will start reading from the
        # next frame so the lost preamble is bounded to one keystroke's
        # worth - acceptable for the back-compat path.
        return "new", None

    mode = frame.get("mode")
    if mode == "resume":
        sid = frame.get("sessionId")
        if isinstance(sid, str) and _SESSION_ID_RE.match(sid):
            return "resume", sid
        logger.warning("terminal bridge: malformed session frame, defaulting to new")
        return "new", None
    if mode == "new":
        return "new", None

    logger.warning("terminal bridge: unknown session mode %r, defaulting to new", mode)
    return "new", None


async def handle_terminal_websocket(websocket: Any) -> None:
    """Serve one terminal session over a WebSocket.

    Auth is validated by the caller (the route wrapper) before this runs.
    """
    await websocket.accept()

    proc: subprocess.Popen[bytes] | None = None
    child_pid = 0
    master_fd: int | None = None
    reader_task: asyncio.Task[None] | None = None
    writer_task: asyncio.Task[None] | None = None

    session_mode = "new"
    session_id: str | None = None

    try:
        # Wait for the optional session handshake BEFORE resolving the
        # command so we can append --resume to the argv list (no shell).
        session_mode, session_id = await _await_session_handshake(websocket)

        try:
            base_cmd = _resolve_command()
        except RuntimeError as exc:
            await websocket.send_text(
                json.dumps({"type": "error", "message": str(exc)})
            )
            await websocket.close(code=4403)
            return

        if session_mode == "resume" and session_id is not None:
            resume_cmd = _build_resume_argv(base_cmd, session_id)
            if resume_cmd is None:
                # Validation failed - degrade safely to a fresh session.
                logger.warning(
                    "terminal bridge: rejected sessionId, falling back to new"
                )
                cmd = base_cmd
                session_mode = "new"
            else:
                cmd = resume_cmd
        else:
            cmd = base_cmd

        # Tell the client which mode we locked in so the UI can show
        # the right banner. We send this AFTER the PTY fork would have
        # happened, but we send it BEFORE the first output frame is
        # possible because output is pumped asynchronously through the
        # reader task that we only start after this point.
        try:
            await websocket.send_text(
                json.dumps(
                    {
                        "type": "ready",
                        "mode": session_mode,
                        "sessionId": session_id,
                    }
                )
            )
        except Exception:
            # Client vanished mid-handshake - clean up.
            return

        # Spawn the command in a fresh PTY. The connector runs as the
        # same user that owns the hermes install, so the terminal inherits
        # the normal environment (PATH, config, keys).
        child_pid, master_fd = pty.fork()
        if child_pid == 0:
            # Child: exec the command, stdio bound to the pty.
            env = os.environ.copy()
            # The connector service runs on a stripped PATH (system dirs
            # only). The hermes CLI lives in the standard user-local bin -
            # append it so PATH resolution works without a hardcoded path.
            user_bin = os.path.expanduser("~/.local/bin")
            env["PATH"] = f"{user_bin}:{env.get('PATH', '')}"
            # hermes --tui resolves its node via _node_bin() which prefers
            # the bundled runtime at ~/.hermes/node/bin. On this host the
            # system node is v20 and the TUI requires >=22.22 - so the
            # bundled runtime must be on PATH for npm install to succeed.
            hermes_node = os.path.expanduser("~/.hermes/node/bin")
            if os.path.isdir(hermes_node):
                env["PATH"] = f"{hermes_node}:{env['PATH']}"
            env["TERM"] = "xterm-256color"
            env["COLORTERM"] = "truecolor"
            try:
                os.execvpe(cmd[0], cmd, env)
            except Exception as exc:  # pragma: no cover - child path
                os.write(2, f"exec failed: {exc}\n".encode())
                os._exit(127)

        async def pump_pty_to_ws() -> None:
            loop = asyncio.get_running_loop()
            assert master_fd is not None
            while True:
                data = await loop.run_in_executor(None, _read_pty, master_fd)
                if data is None:
                    break
                if data:
                    await websocket.send_text(
                        json.dumps({"type": "output", "data": data.decode("utf-8", "replace")})
                    )
            await websocket.send_text(json.dumps({"type": "exit", "code": 0}))

        reader_task = asyncio.create_task(pump_pty_to_ws())

        async for raw in websocket.iter_text():
            try:
                frame = json.loads(raw)
            except json.JSONDecodeError:
                continue
            ftype = frame.get("type")
            if ftype == "input":
                data = str(frame.get("data", ""))
                if master_fd is not None:
                    os.write(master_fd, data.encode())
            elif ftype == "resize":
                cols = int(frame.get("cols", 80))
                rows = int(frame.get("rows", 24))
                if master_fd is not None:
                    try:
                        import fcntl
                        import struct
                        fcntl.ioctl(
                            master_fd,
                            0x5414,  # TIOCSWINSZ
                            struct.pack("HHHH", rows, cols, 0, 0),
                        )
                    except Exception:
                        pass
            elif ftype == "signal":
                if frame.get("name") == "intr" and child_pid > 0:
                    try:
                        os.kill(child_pid, signal_module.SIGINT)
                    except ProcessLookupError:
                        pass
            # Note: ``session`` frames received after the handshake are
            # ignored - the PTY is already bound to whatever was chosen
            # in the handshake window. We don't surface an error because
            # a duplicate session frame from a slow client must not look
            # like a bridge failure.

    except Exception:
        logger.exception("terminal bridge error")
        try:
            await websocket.send_text(json.dumps({"type": "error", "message": "terminal bridge error"}))
        except Exception:
            pass
    finally:
        if reader_task is not None:
            reader_task.cancel()
        if writer_task is not None:
            writer_task.cancel()
        if child_pid and child_pid > 0:
            try:
                os.kill(child_pid, signal_module.SIGKILL)
            except ProcessLookupError:
                pass
        if master_fd is not None:
            try:
                os.close(master_fd)
            except OSError:
                pass


def _read_pty(fd: int) -> bytes | None:
    """Blocking read of one available chunk from the pty master.

    Returns None when the pty is closed / HUP.
    """
    try:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if not ready:
            return b""
        data = os.read(fd, 65536)
        return data
    except OSError:
        return None