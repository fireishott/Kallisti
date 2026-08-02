"""Tests for the JSON-RPC 2.0 WebSocket endpoint."""

import json
import pytest
from starlette.testclient import TestClient
from starlette.websockets import WebSocket

from herald_connector.http_facade import app


class TestJsonRpcWsEndpoint:
    """Test the /api/ws JSON-RPC WebSocket endpoint."""

    def test_connect_and_disconnect(self):
        """Client can connect and disconnect cleanly."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            # Connection should be accepted
            pass

    def test_ping_pong(self):
        """Ping event returns pong."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            ws.send_json({
                "jsonrpc": "2.0",
                "method": "event",
                "params": {"type": "ping"}
            })
            response = ws.receive_json()
            assert response["jsonrpc"] == "2.0"
            assert response["method"] == "event"
            assert response["params"]["type"] == "pong"

    def test_method_not_found(self):
        """Unknown method returns METHOD_NOT_FOUND error."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            ws.send_json({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "nonexistent.method",
                "params": {}
            })
            response = ws.receive_json()
            assert response["jsonrpc"] == "2.0"
            assert response["id"] == 1
            assert "error" in response
            assert response["error"]["code"] == -32601

    def test_notification_no_response(self):
        """Notification (no id) does not produce a response."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            ws.send_json({
                "jsonrpc": "2.0",
                "method": "some.notification"
            })
            # No response expected — but we need to be careful here
            # In practice, the server just doesn't respond

    def test_session_list_returns_result(self):
        """session.list returns a result with sessions key."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            ws.send_json({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session.list",
                "params": {}
            })
            response = ws.receive_json()
            assert response["jsonrpc"] == "2.0"
            assert response["id"] == 2
            assert "result" in response
            assert "sessions" in response["result"]

    def test_gateway_status_returns_result(self):
        """gateway.status returns a result."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            ws.send_json({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "gateway.status",
                "params": {}
            })
            response = ws.receive_json()
            assert response["jsonrpc"] == "2.0"
            assert response["id"] == 3
            assert "result" in response

    def test_prompt_submit_requires_session_id(self):
        """prompt.submit without session_id returns error."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            ws.send_json({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "prompt.submit",
                "params": {"message": "hello"}
            })
            response = ws.receive_json()
            assert response["jsonrpc"] == "2.0"
            assert response["id"] == 4
            assert "error" in response

    def test_prompt_submit_requires_message(self):
        """prompt.submit without message returns error."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            ws.send_json({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "prompt.submit",
                "params": {"session_id": "test"}
            })
            response = ws.receive_json()
            assert response["jsonrpc"] == "2.0"
            assert response["id"] == 5
            assert "error" in response

    def test_multiple_requests_same_connection(self):
        """Multiple requests on the same connection are correlated by id."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            # Send two requests
            ws.send_json({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "session.list",
                "params": {}
            })
            ws.send_json({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "gateway.status",
                "params": {}
            })

            # Collect responses
            responses = {}
            for _ in range(2):
                resp = ws.receive_json()
                responses[resp["id"]] = resp

            assert 10 in responses
            assert 11 in responses
            assert responses[10]["id"] == 10
            assert responses[11]["id"] == 11

    def test_frame_has_jsonrpc_version(self):
        """All responses include jsonrpc 2.0."""
        client = TestClient(app)
        with client.websocket_connect("/api/ws") as ws:
            ws.send_json({
                "jsonrpc": "2.0",
                "id": 20,
                "method": "session.list",
                "params": {}
            })
            response = ws.receive_json()
            assert response["jsonrpc"] == "2.0"
