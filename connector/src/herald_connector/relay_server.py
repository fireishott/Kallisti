"""Hermes Relay Protocol WebSocket server.

Listens for inbound connections from the Hermes Gateway and exchanges
normalized MessageEvents. This replaces the FastAPI relay with a native
channel inside the connector.

Protocol reference: github.com/NousResearch/hermes-agent gateway/relay/
"""

from __future__ import annotations

import asyncio
import json
import logging
import uuid
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from typing import Any, Callable, Coroutine

import websockets
from websockets.asyncio.server import Server, ServerConnection

logger = logging.getLogger("herald.relay_server")

# ── Capability Descriptor ────────────────────────────────────────

CONTRACT_VERSION = 1

CAPABILITY_DESCRIPTOR: dict[str, Any] = {
    "contract_version": CONTRACT_VERSION,
    "platform": "herald",
    "label": "Herald",
    "max_message_length": 4096,
    "supports_draft_streaming": False,
    "supports_edit": False,
    "supports_threads": False,
    "markdown_dialect": "plain",
    "len_unit": "chars",
    "emoji": "📱",
    "platform_hint": "You are on Herald (iOS). Keep responses concise.",
    "pii_safe": False,
    "supports_context": False,
}

# ── Callback types ───────────────────────────────────────────────

# Called when gateway sends an outbound action (agent response → deliver to iOS).
# Signature: (request_id: str, action: dict) -> SendResult
OutboundHandler = Callable[[str, dict], Coroutine[Any, Any, dict]]

# Called when gateway sends an interrupt (/stop).
# Signature: (session_key: str, reason: str | None) -> None
InterruptHandler = Callable[[str, str | None], Coroutine[Any, Any, None]]


@dataclass
class SendResult:
    success: bool
    message_id: str | None = None
    error: str | None = None
    retryable: bool = False


# ── Relay Server ─────────────────────────────────────────────────

class HeraldRelayServer:
    """WebSocket server implementing the Hermes Relay Protocol.

    The Hermes gateway dials out to this server, receives a CapabilityDescriptor,
    and exchanges normalized MessageEvents bidirectionally.
    """

    def __init__(
        self,
        host: str = "0.0.0.0",
        port: int = 8765,
        outbound_handler: OutboundHandler | None = None,
        interrupt_handler: InterruptHandler | None = None,
    ) -> None:
        self.host = host
        self.port = port
        self._outbound_handler = outbound_handler
        self._interrupt_handler = interrupt_handler
        self._server: Server | None = None
        self._gateway_connection: ServerConnection | None = None
        self._pending_outbound: dict[str, asyncio.Future[dict]] = {}
        self._connected = False

    @property
    def is_gateway_connected(self) -> bool:
        return self._connected and self._gateway_connection is not None

    @staticmethod
    def _encode_frame(frame: dict) -> str:
        """Encode a frame as NDJSON (newline-terminated JSON)."""
        return json.dumps(frame, separators=(",", ":")) + "\n"

    async def start(self) -> None:
        """Start the WebSocket server and listen for gateway connections."""
        self._server = await websockets.serve(
            self._handle_connection,
            self.host,
            self.port,
            max_size=50 * 1024 * 1024,  # 50 MB — large payloads with attachments
        )
        logger.info("Relay server listening on ws://%s:%s/relay", self.host, self.port)

    async def stop(self) -> None:
        """Gracefully shut down the relay server."""
        if self._server is not None:
            self._server.close()
            await self._server.wait_closed()
            self._server = None
        self._connected = False
        self._gateway_connection = None
        # Cancel pending outbound futures
        for future in self._pending_outbound.values():
            if not future.done():
                future.cancel()
        self._pending_outbound.clear()
        logger.info("Relay server stopped")

    async def send_inbound_event(self, event: dict) -> None:
        """Send an inbound MessageEvent to the connected gateway.

        Call this when the iOS app sends a user message that should be
        forwarded to the Hermes agent.
        """
        if not self.is_gateway_connected:
            logger.warning("Cannot send inbound event: no gateway connected")
            return

        try:
            await self._gateway_connection.send(self._encode_frame({"type": "inbound", "event": event}))
            logger.debug("Sent inbound event: message_id=%s", event.get("message_id"))
        except Exception:
            logger.exception("Failed to send inbound event")
            raise

    async def _handle_connection(self, websocket: ServerConnection) -> None:
        """Handle a single gateway WebSocket connection."""
        logger.info("Gateway connected from %s", websocket.remote_address)

        # Only allow one gateway connection at a time
        if self._gateway_connection is not None:
            logger.warning("Rejecting duplicate gateway connection")
            await websocket.close(4409, "Already connected")
            return

        self._gateway_connection = websocket
        self._connected = True

        try:
            # Wait for hello frame
            raw = await asyncio.wait_for(websocket.recv(), timeout=30.0)
            hello = json.loads(raw)

            if hello.get("type") != "hello":
                logger.error("Expected hello frame, got: %s", hello.get("type"))
                await websocket.close(4400, "Expected hello frame")
                return

            platform = hello.get("platform", "unknown")
            bot_id = hello.get("botId", "")
            logger.info("Handshake: platform=%s, botId=%s", platform, bot_id)

            # Send descriptor
            await websocket.send(self._encode_frame({
                "type": "descriptor",
                "descriptor": CAPABILITY_DESCRIPTOR,
            }))
            logger.info("Handshake complete — channel is live")

            # Enter message loop
            await self._message_loop(websocket)

        except websockets.ConnectionClosed:
            logger.info("Gateway disconnected")
        except asyncio.TimeoutError:
            logger.warning("Handshake timeout — no hello received")
            await websocket.close(4408, "Handshake timeout")
        except Exception:
            logger.exception("Error handling gateway connection")
        finally:
            self._connected = False
            self._gateway_connection = None
            # Resolve any pending outbound futures with error
            for req_id, future in list(self._pending_outbound.items()):
                if not future.done():
                    future.set_result({"success": False, "error": "relay transport closed"})
            self._pending_outbound.clear()
            logger.info("Gateway connection cleaned up")

    async def _message_loop(self, websocket: ServerConnection) -> None:
        """Process incoming NDJSON frames from the gateway."""
        buf = ""
        async for chunk in websocket:
            buf += chunk if isinstance(chunk, str) else chunk.decode("utf-8")
            *lines, buf = buf.split("\n")
            for line in lines:
                line = line.strip()
                if not line:
                    continue
                try:
                    frame = json.loads(line)
                except json.JSONDecodeError:
                    logger.warning("Malformed frame skipped: %s", line[:200])
                    continue

                frame_type = frame.get("type")

                if frame_type == "outbound":
                    await self._handle_outbound(websocket, frame)
                elif frame_type == "interrupt":
                    await self._handle_interrupt(frame)
                elif frame_type == "inbound_ack":
                    logger.debug("Inbound ACK: bufferId=%s", frame.get("bufferId"))
                elif frame_type == "going_idle":
                    await websocket.send(self._encode_frame({"type": "going_idle_ack"}))
                else:
                    logger.debug("Unknown frame type ignored: %s", frame_type)

    async def _handle_outbound(self, websocket: ServerConnection, frame: dict) -> None:
        """Handle an outbound action from the gateway (agent response → iOS)."""
        request_id = frame.get("requestId", str(uuid.uuid4()))
        action = frame.get("action", {})

        logger.info("Outbound action: requestId=%s, action_type=%s",
                     request_id, action.get("type", "send"))

        result: dict
        if self._outbound_handler is not None:
            try:
                result = await self._outbound_handler(request_id, action)
            except Exception as exc:
                logger.exception("Outbound handler error")
                result = {"success": False, "error": str(exc)}
        else:
            result = {"success": False, "error": "No outbound handler configured"}

        # Send result back to gateway
        try:
            await websocket.send(self._encode_frame({
                "type": "outbound_result",
                "requestId": request_id,
                "result": result,
            }))
        except Exception:
            logger.exception("Failed to send outbound_result")

    async def _handle_interrupt(self, frame: dict) -> None:
        """Handle an interrupt (/stop) from the gateway."""
        session_key = frame.get("session_key", "")
        reason = frame.get("reason")
        logger.info("Interrupt: session_key=%s, reason=%s", session_key, reason)

        if self._interrupt_handler is not None:
            try:
                await self._interrupt_handler(session_key, reason)
            except Exception:
                logger.exception("Interrupt handler error")


def build_message_event(
    text: str,
    *,
    device_installation_id: str,
    user_name: str = "User",
    device_name: str = "iPhone",
    message_id: str | None = None,
    message_type: str = "text",
) -> dict:
    """Build a MessageEvent dict for sending inbound to the gateway.

    This is the normalized event shape that the Hermes gateway expects.
    """
    return {
        "text": text,
        "message_type": message_type,
        "message_id": message_id or str(uuid.uuid4()),
        "source": {
            "platform": "herald",
            "chat_id": device_installation_id,
            "chat_type": "dm",
            "chat_name": device_name,
            "user_id": device_installation_id,
            "user_name": user_name,
            "thread_id": None,
        },
    }
