"""Build 33 Workstream A: restart operations — connector side.

Decodes the shared contract fixtures in tests/fixtures/restart/ and verifies
the connector's behaviour against them:

  * preflight shape with real (mocked) systemd data
  * idempotent restart (same key → same operation)
  * conflict 409 while an operation is active
  * stale preflight → 409
  * failed restart → typed error (stage/unit/exitStatus/journalExcerpt/retryable/action)
  * health report includes every probe result
  * connector self-restart durability + startup reconciliation
"""

from __future__ import annotations

import asyncio
import datetime
import json
import re
import stat
import time
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from starlette.exceptions import HTTPException
from starlette.testclient import TestClient

import herald_connector.http_facade as facade
from herald_connector.http_facade import FacadeContext, app
from herald_connector.restart_operations import (
    RestartConflictError,
    RestartOperationStore,
    get_restart_store,
)

FIXTURES = Path(__file__).parent / "fixtures" / "restart"

_RFC3339_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")

# 2026-07-31T19:00:00Z as a unix epoch — the value systemctl --timestamp=unix
# would emit for the fixture's observed.execMainStartTimestamp.
_EPOCH_1900 = int(
    datetime.datetime(2026, 7, 31, 19, 0, 0, tzinfo=datetime.timezone.utc).timestamp()
)


def load_fixture(name: str) -> dict:
    return json.loads((FIXTURES / name).read_text())


# ── Shared fixtures ────────────────────────────────────────────────────────


@pytest.fixture(autouse=True)
def clean_facade_state():
    """Isolate module-level facade state between tests."""
    facade._last_canary_result = None
    facade._http_jobs.clear()
    facade._restart_tasks.clear()
    yield
    facade._last_canary_result = None
    facade._http_jobs.clear()
    facade._restart_tasks.clear()


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv("HERMES_MOBILE_CONNECTOR_HOME", str(tmp_path))
    monkeypatch.setenv("HERMES_HOME", "/home/user/.hermes/profiles/ignyte")
    monkeypatch.delenv("HERMES_AGENT_UNIT", raising=False)
    monkeypatch.setenv("HERALD_RESTART_ACTIVE_TIMEOUT", "2")
    monkeypatch.setenv("HERALD_RESTART_POLL_INTERVAL", "0.02")
    return tmp_path


@pytest.fixture
def store(tmp_path, monkeypatch):
    monkeypatch.setenv("HERMES_MOBILE_CONNECTOR_HOME", str(tmp_path))
    return RestartOperationStore(tmp_path / "restart_operations.sqlite3")


@pytest.fixture
def client():
    with TestClient(app) as c:
        yield c


@pytest.fixture(autouse=True)
def auth():
    with patch("herald_connector.http_facade.require_auth", new_callable=AsyncMock):
        yield


@pytest.fixture
def ctx(env):
    ctx = FacadeContext()
    ctx.connector_version = "2.4.1"
    ctx.restart_store = get_restart_store()
    return ctx


@pytest.fixture
def app_env(env, ctx):
    """ctx patched as the live facade context for the whole test."""
    with patch("herald_connector.http_facade.get_context", return_value=ctx):
        yield ctx


# ── systemctl / journalctl mocks ───────────────────────────────────────────


def standard_props(pid: str = "12345", ts: int | None = None) -> dict:
    return {
        "MainPID": pid,
        "ExecMainStartTimestamp": str(ts if ts is not None else _EPOCH_1900),
        "ActiveState": "active",
    }


_SYSTEMCTL_SUBCOMMANDS = ("show", "is-active", "restart", "set-environment")


def fake_systemctl(show_props=None, is_active="active", restart_rc=0, journal_lines=None):
    """Fake for facade._run_subprocess covering systemctl + journalctl.

    systemctl argv is ["systemctl", "--user", [--timestamp=unix], <subcommand>, …]
    — the subcommand is found by scanning argv, not by index.
    """
    props_fn = show_props if callable(show_props) else (lambda: show_props or standard_props())

    def _run(argv, **kwargs):  # noqa: ARG001
        if argv[0] == "systemctl":
            subcmd = next((a for a in argv if a in _SYSTEMCTL_SUBCOMMANDS), None)
            if subcmd == "show":
                out = "".join(f"{k}={v}\n" for k, v in props_fn().items())
                return MagicMock(returncode=0, stdout=out, stderr="")
            if subcmd == "is-active":
                rc = 0 if is_active == "active" else 3
                return MagicMock(returncode=rc, stdout=is_active + "\n", stderr="")
            if subcmd == "restart":
                return MagicMock(
                    returncode=restart_rc,
                    stdout="",
                    stderr="" if restart_rc == 0 else "Failed to restart unit",
                )
            if subcmd == "set-environment":
                return MagicMock(returncode=0, stdout="", stderr="")
            raise AssertionError(f"unexpected systemctl subcommand: {subcmd}")
        if argv[0] == "journalctl":
            return MagicMock(
                returncode=0,
                stdout="\n".join(journal_lines or []) + "\n",
                stderr="",
            )
        raise AssertionError(f"unexpected command: {argv}")

    return _run


def preflight_version(client, target: str = "hermes") -> str:
    resp = client.get(f"/v1/gw/restart/preflight?target={target}")
    assert resp.status_code == 200, resp.text
    return resp.json()["preflightVersion"]


def wait_for_phase(client, operation_id: str, *phases: str, timeout: float = 10.0) -> dict:
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        resp = client.get(f"/v1/gw/restart/{operation_id}")
        assert resp.status_code == 200, resp.text
        last = resp.json()
        if last["phase"] in phases:
            return last
        time.sleep(0.05)
    raise AssertionError(f"operation {operation_id} did not reach {phases}; last={last}")


# ── RestartOperationStore ──────────────────────────────────────────────────


class TestRestartOperationStore:
    def test_create_roundtrip_matches_operation_fixture(self, store):
        op = store.create_operation(
            "550e8400-e29b-41d4-a716-446655440000",
            "idem-1",
            "hermes",
            "hermes-gateway-ignyte.service",
            preflight_version="1",
            old_pid=12345,
            old_start_ts="2026-07-31T19:00:00Z",
        )
        fixture = load_fixture("operation_accepted.json")
        assert set(op.keys()) == set(fixture.keys())
        assert op["$schema"] == "restart-operation-v1"
        assert op["target"] == "hermes"
        assert op["unit"] == "hermes-gateway-ignyte.service"
        assert op["phase"] == "accepted"
        assert op["completedAt"] is None
        assert op["checks"] == []
        assert op["error"] is None
        assert _RFC3339_RE.match(op["acceptedAt"])
        assert store.get_operation(op["operationId"]) == op
        assert store.get_operation_details(op["operationId"])["oldMainPid"] == 12345

    def test_create_operation_conflict_carries_active_operation(self, store):
        store.create_operation("op-1", "key-1", "hermes", "hermes-gateway-ignyte.service")
        with pytest.raises(RestartConflictError) as exc:
            store.create_operation("op-2", "key-2", "hermes", "hermes-gateway-ignyte.service")
        assert exc.value.operation_id == "op-1"
        assert exc.value.operation["$schema"] == "restart-operation-v1"
        assert exc.value.operation["phase"] in (
            "accepted", "stopping", "starting", "verifying",
        )

    def test_no_conflict_across_targets(self, store):
        store.create_operation("op-1", "key-1", "hermes", "hermes-gateway-ignyte.service")
        op = store.create_operation("op-2", "key-2", "connector", "hermes-mobile-connector.service")
        assert op["operationId"] == "op-2"

    def test_idempotent_replay_returns_same_operation(self, store):
        first = store.create_operation("op-1", "key-1", "hermes", "u")
        replay = store.create_operation("op-2", "key-1", "hermes", "u")
        assert replay["operationId"] == "op-1"
        assert store.get_by_idempotency_key("key-1")["operationId"] == "op-1"
        assert store.get_operation("op-2") is None

    def test_idempotent_replay_after_completion(self, store):
        store.create_operation("op-1", "key-1", "hermes", "u")
        store.complete_operation("op-1", "healthy", checks=[])
        replay = store.create_operation("op-2", "key-1", "hermes", "u")
        assert replay["operationId"] == "op-1"
        assert replay["phase"] == "healthy"

    def test_terminal_operation_allows_new_restart(self, store):
        store.create_operation("op-1", "key-1", "hermes", "u")
        store.complete_operation("op-1", "failed", checks=[], error={
            "stage": "starting", "exitStatus": 1, "journalExcerpt": "x",
            "retryable": True, "action": "retry",
        })
        op = store.create_operation("op-2", "key-2", "hermes", "u")
        assert op["operationId"] == "op-2"
        assert store.get_active_operation("hermes")["operationId"] == "op-2"

    def test_update_phase_appends_checks_and_error(self, store):
        store.create_operation("op-1", "key-1", "hermes", "u")
        store.update_phase("op-1", "stopping")
        store.update_phase("op-1", "verifying", checks=[
            {"name": "a", "passed": True, "detail": "x"},
        ])
        store.update_phase("op-1", "verifying", checks=[
            {"name": "b", "passed": False, "detail": "y"},
        ], error={
            "stage": "verifying", "exitStatus": None, "journalExcerpt": "j",
            "retryable": True, "action": "a",
        })
        op = store.get_operation("op-1")
        assert [c["name"] for c in op["checks"]] == ["a", "b"]
        assert op["error"] == {
            "stage": "verifying", "unit": "u", "exitStatus": None,
            "journalExcerpt": "j", "retryable": True, "action": "a",
        }

    def test_complete_operation_sets_terminal_phase(self, store):
        store.create_operation("op-1", "key-1", "hermes", "u")
        done = store.complete_operation("op-1", "healthy", checks=[
            {"name": "c", "passed": True, "detail": "d"},
        ])
        assert done["phase"] == "healthy"
        assert _RFC3339_RE.match(done["completedAt"])
        assert done["error"] is None
        assert [c["name"] for c in done["checks"]] == ["c"]
        with pytest.raises(ValueError):
            store.update_phase("op-1", "stopping")
        with pytest.raises(ValueError):
            store.update_phase("op-1", "healthy")

    def test_reconcile_stale_operations_marks_non_terminal_failed(self, store):
        store.create_operation("op-1", "key-1", "hermes", "u")
        store.update_phase("op-1", "starting")
        store.create_operation("op-2", "key-2", "connector", "hermes-mobile-connector.service")
        assert store.reconcile_stale_operations() == 2
        op = store.get_operation("op-1")
        assert op["phase"] == "failed"
        assert op["error"]["stage"] == "starting"
        assert op["error"]["retryable"] is True
        assert op["error"]["action"]
        # Terminal operations are untouched.
        assert store.get_operation("op-2")["phase"] == "failed"
        assert store.reconcile_stale_operations() == 0

    def test_db_file_mode_0600_and_wal(self, store):
        assert stat.S_IMODE(store.db_path.stat().st_mode) == 0o600
        conn = store._connect()
        try:
            assert conn.execute("PRAGMA journal_mode").fetchone()[0].lower() == "wal"
            assert conn.execute("PRAGMA foreign_keys").fetchone()[0] == 1
        finally:
            conn.close()

    def test_unknown_operation_raises(self, store):
        with pytest.raises(KeyError):
            store.update_phase("nope", "stopping")


# ── Preflight ──────────────────────────────────────────────────────────────


class TestPreflight:
    def test_preflight_shape_matches_fixture(self, app_env, client):
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=standard_props(pid="12345")),
        ):
            resp = client.get("/v1/gw/restart/preflight?target=hermes")

        assert resp.status_code == 200
        data = resp.json()
        fixture = load_fixture("preflight_ok.json")
        assert set(data.keys()) == set(fixture.keys())
        for key in ("$schema", "target", "profile", "unit", "canRestart", "blocker"):
            assert data[key] == fixture[key]
        assert data["observed"] == fixture["observed"]
        assert data["activeWork"] == fixture["activeWork"]
        assert data["preflightVersion"]  # non-empty; client must echo it back

    def test_preflight_counts_active_work(self, app_env, client):
        facade._http_jobs.update({
            "j1": {"status": "running", "events": [{"type": "tool_activity"}]},
            "j2": {"status": "running", "events": []},
            "j3": {"status": "queued", "events": []},
        })
        try:
            with patch(
                "herald_connector.http_facade._run_subprocess",
                side_effect=fake_systemctl(show_props=standard_props()),
            ):
                resp = client.get("/v1/gw/restart/preflight?target=hermes")
        finally:
            facade._http_jobs.clear()

        assert resp.status_code == 200
        data = resp.json()
        assert data["activeWork"] == {"running": 2, "queued": 1, "voice": 0, "tools": 1}
        assert data["activeWork"] == load_fixture("preflight_active_work.json")["activeWork"]

    def test_preflight_blocked_when_unit_inactive(self, app_env, client):
        props = standard_props()
        props["ActiveState"] = "inactive"
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=props),
        ):
            resp = client.get("/v1/gw/restart/preflight?target=hermes")

        assert resp.status_code == 200
        data = resp.json()
        assert data["canRestart"] is False
        assert "not active" in data["blocker"]

    def test_preflight_blocked_when_unit_missing(self, app_env, client):
        def missing(argv, **kwargs):  # noqa: ARG001
            return MagicMock(returncode=1, stdout="", stderr="Unit not found")

        with patch("herald_connector.http_facade._run_subprocess", side_effect=missing):
            resp = client.get("/v1/gw/restart/preflight?target=hermes")

        assert resp.status_code == 200
        data = resp.json()
        assert data["canRestart"] is False
        assert "hermes-gateway-ignyte.service" in data["blocker"]
        assert data["observed"]["mainPid"] is None

    def test_preflight_rejects_unknown_target(self, app_env, client):
        resp = client.get("/v1/gw/restart/preflight?target=relay")
        assert resp.status_code == 400


# ── Restart endpoint ───────────────────────────────────────────────────────


class TestRestartEndpoint:
    def test_restart_accepts_and_matches_operation_fixture(self, app_env, client):
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=standard_props()),
        ):
            version = preflight_version(client)
            resp = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "idem-1"},
                json={"target": "hermes", "preflightVersion": version},
            )

        assert resp.status_code == 200
        op = resp.json()
        fixture = load_fixture("operation_accepted.json")
        assert set(op.keys()) == set(fixture.keys())
        assert op["target"] == "hermes"
        assert op["unit"] == "hermes-gateway-ignyte.service"
        assert op["phase"] == "accepted"
        assert op["completedAt"] is None
        assert op["checks"] == []
        assert op["error"] is None
        assert _RFC3339_RE.match(op["acceptedAt"])

    def test_restart_same_idempotency_key_returns_same_operation(self, app_env, client):
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=standard_props()),
        ):
            version = preflight_version(client)
            headers = {"Idempotency-Key": "idem-1"}
            body = {"target": "hermes", "preflightVersion": version}
            first = client.post("/v1/gw/restart", headers=headers, json=body)
            second = client.post("/v1/gw/restart", headers=headers, json=body)

        assert first.status_code == 200
        assert second.status_code == 200
        assert first.json()["operationId"] == second.json()["operationId"]

    def test_restart_different_key_while_active_returns_409(self, app_env, client):
        slow_health = AsyncMock(side_effect=lambda: _slow_healthy(5.0))
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=standard_props()),
        ), patch(
            "herald_connector.http_facade._probe_dashboard_health", slow_health
        ):
            version = preflight_version(client)
            first = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "key-1"},
                json={"target": "hermes", "preflightVersion": version},
            )
            second = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "key-2"},
                json={"target": "hermes", "preflightVersion": version},
            )

        assert first.status_code == 200
        assert second.status_code == 409
        body = second.json()
        assert body["error"]["code"] == "RESTART_IN_PROGRESS"
        assert body["error"]["operationId"] == first.json()["operationId"]
        # The 409 carries the existing operation (conflict_in_progress.json shape).
        assert body["operation"]["$schema"] == "restart-operation-v1"
        assert body["operation"]["operationId"] == first.json()["operationId"]
        assert body["operation"]["phase"] in (
            "accepted", "stopping", "starting", "verifying",
        )

    def test_restart_stale_preflight_returns_409(self, app_env, client):
        props = standard_props(pid="12345")
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=props),
        ):
            version = preflight_version(client)
            # The gateway restarts out-of-band → its observed state changes.
            props["MainPID"] = "99999"
            resp = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "key-1"},
                json={"target": "hermes", "preflightVersion": version},
            )

        assert resp.status_code == 409
        body = resp.json()
        assert body["error"]["code"] == "PREFLIGHT_STALE"
        assert body["error"]["preflightVersion"] == version
        assert body["error"]["currentPreflightVersion"] != version

    def test_restart_missing_preflight_version_returns_400(self, app_env, client):
        resp = client.post(
            "/v1/gw/restart",
            headers={"Idempotency-Key": "key-1"},
            json={"target": "hermes"},
        )
        assert resp.status_code == 400

    def test_restart_legacy_path_without_idempotency_key(self, app_env, client):
        gateway_fn = AsyncMock(return_value={"restarting": True, "message": "Restart requested"})
        app_env.gateway_restart = gateway_fn
        resp = client.post("/v1/gw/restart", json={"target": "hermes"})

        assert resp.status_code == 200
        gateway_fn.assert_awaited_once_with("hermes")
        data = resp.json()
        assert data["restarting"] is True
        assert data["target"] == "hermes"

    def test_restart_legacy_path_without_gateway_control(self, app_env, client):
        app_env.gateway_restart = None
        resp = client.post("/v1/gw/restart", json={"target": "hermes"})
        assert resp.status_code == 503

    def test_restart_relay_target_rejected(self, app_env, client):
        resp = client.post("/v1/gw/restart", json={"target": "relay"})
        assert resp.status_code == 400
        assert "relay" in resp.json()["error"]["message"]
        resp2 = client.post(
            "/v1/gw/restart",
            headers={"Idempotency-Key": "key-1"},
            json={"target": "relay", "preflightVersion": "x"},
        )
        assert resp2.status_code == 400

    def test_restart_unknown_target_rejected(self, app_env, client):
        resp = client.post(
            "/v1/gw/restart",
            headers={"Idempotency-Key": "key-1"},
            json={"target": "dashboard", "preflightVersion": "x"},
        )
        assert resp.status_code == 400

    def test_restart_requires_auth(self, env, client):
        with patch(
            "herald_connector.http_facade.require_auth",
            new_callable=AsyncMock,
            side_effect=HTTPException(status_code=401, detail="Invalid or missing access token"),
        ):
            resp = client.post("/v1/gw/restart", json={"target": "hermes"})
        assert resp.status_code == 401


# ── Background execution ───────────────────────────────────────────────────


def _evolving_props():
    """systemctl show returns pid 12345 for the first two queries (preflight +
    staleness check), then 12456 once the restart has happened."""
    state = {"show": 0}
    props = standard_props(pid="12345")

    def _props_fn():
        state["show"] += 1
        return standard_props(pid="12456") if state["show"] >= 3 else props

    return _props_fn


async def _slow_healthy(seconds: float):
    await asyncio.sleep(seconds)
    return True, "detail health ok"


class TestBackgroundExecution:
    def test_reaches_healthy_with_all_checks(self, app_env, client):
        app_env.model_catalog = AsyncMock(return_value={
            "activeModel": {"name": "m0"},
            "models": [{"name": f"m{i}"} for i in range(12)],
        })
        app_env.session_canary = AsyncMock(return_value=(True, "ok"))
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=_evolving_props()),
        ), patch(
            "herald_connector.http_facade._probe_dashboard_health",
            AsyncMock(return_value=(True, "detail health ok")),
        ):
            version = preflight_version(client)
            created = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "h-1"},
                json={"target": "hermes", "preflightVersion": version},
            ).json()
            op = wait_for_phase(client, created["operationId"], "healthy", "failed")

        assert op["phase"] == "healthy"
        fixture = load_fixture("operation_healthy.json")
        assert set(op.keys()) == set(fixture.keys())
        assert op["error"] is None
        assert _RFC3339_RE.match(op["completedAt"])
        names = [c["name"] for c in op["checks"]]
        assert names == [
            "systemctl-is-active", "pid-changed", "hermes-ready",
            "model-catalog", "session-roundtrip",
        ]
        assert all(c["passed"] is True for c in op["checks"])
        assert op["checks"][1]["detail"] == "12345 → 12456"
        assert op["checks"][3]["detail"] == "12 models loaded"
        assert op["checks"][4]["detail"] == "ok"
        app_env.session_canary.assert_awaited_once()

    def test_failed_restart_returns_typed_error(self, app_env, client):
        app_env.session_canary = AsyncMock(return_value=(True, "ok"))
        journal_line = "Jul 31 19:30:30 hermes-gateway[12456]: ERROR config.yaml parse failed"
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(
                show_props=_evolving_props(), journal_lines=[journal_line],
            ),
        ), patch(
            "herald_connector.http_facade._probe_dashboard_health",
            AsyncMock(return_value=(False, "health endpoint returned 503 after 30s")),
        ):
            version = preflight_version(client)
            created = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "f-1"},
                json={"target": "hermes", "preflightVersion": version},
            ).json()
            op = wait_for_phase(client, created["operationId"], "healthy", "failed")

        assert op["phase"] == "failed"
        fixture = load_fixture("operation_failed.json")
        assert set(op.keys()) == set(fixture.keys())
        err = op["error"]
        assert set(err.keys()) == set(fixture["error"].keys())
        assert err["stage"] == "verifying"
        assert err["unit"] == "hermes-gateway-ignyte.service"
        assert err["exitStatus"] is None
        assert err["journalExcerpt"] == journal_line
        assert err["retryable"] is True
        assert err["action"]
        checks = {c["name"]: c for c in op["checks"]}
        assert checks["hermes-ready"]["passed"] is False
        assert checks["hermes-ready"]["detail"] == "health endpoint returned 503 after 30s"
        assert checks["model-catalog"]["passed"] is False
        assert "skipped" in checks["model-catalog"]["detail"]
        assert checks["session-roundtrip"]["passed"] is False
        assert checks["session-roundtrip"]["detail"] == "skipped: hermes not ready"
        # The canary never runs after the verification failure.
        app_env.session_canary.assert_not_awaited()

    def test_failed_restart_on_restart_command(self, app_env, client):
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=standard_props(), restart_rc=1),
        ):
            version = preflight_version(client)
            created = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "f-2"},
                json={"target": "hermes", "preflightVersion": version},
            ).json()
            op = wait_for_phase(client, created["operationId"], "healthy", "failed")

        assert op["phase"] == "failed"
        assert op["error"]["stage"] == "stopping"
        assert op["error"]["exitStatus"] == 1
        assert op["error"]["retryable"] is True
        assert "systemctl restart" in op["error"]["action"]
        # No verification checks ran at all — everything is skipped.
        assert [c["name"] for c in op["checks"]] == facade._RESTART_STEP_NAMES

    def test_connector_self_restart_durable_and_reconciled(self, app_env, client):
        with patch("herald_connector.http_facade._schedule_connector_exit") as schedule, patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=standard_props()),
        ):
            version = preflight_version(client, target="connector")
            resp = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "c-1"},
                json={"target": "connector", "preflightVersion": version},
            )
            assert resp.status_code == 200
            created = resp.json()
            op = wait_for_phase(client, created["operationId"], "stopping")

        assert op["phase"] == "stopping"
        schedule.assert_called_once()
        # Simulate the next boot: startup reconciliation marks the operation
        # failed because the process died before verification completed.
        store = get_restart_store()
        assert store.reconcile_stale_operations() == 1
        op = store.get_operation(created["operationId"])
        assert op["phase"] == "failed"
        assert op["error"]["stage"] == "stopping"
        assert op["error"]["retryable"] is True
        assert "restarted before this operation could be verified" in op["error"]["action"]


# ── Operation status endpoint ──────────────────────────────────────────────


class TestOperationStatus:
    def test_status_returns_operation(self, app_env, client):
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=standard_props()),
        ):
            version = preflight_version(client)
            created = client.post(
                "/v1/gw/restart",
                headers={"Idempotency-Key": "s-1"},
                json={"target": "hermes", "preflightVersion": version},
            ).json()
            # GET within the same patch window — the background executor may
            # still be running (or already finished); the response must always
            # decode to a valid restart-operation-v1 payload.
            resp = client.get(f"/v1/gw/restart/{created['operationId']}")

        assert resp.status_code == 200
        data = resp.json()
        assert data["operationId"] == created["operationId"]
        assert data["$schema"] == "restart-operation-v1"
        assert data["phase"] in ("accepted", "stopping", "starting", "verifying", "healthy", "failed")

    def test_status_unknown_operation_404(self, app_env, client):
        resp = client.get("/v1/gw/restart/550e8400-e29b-41d4-a716-446655440000")
        assert resp.status_code == 404

    def test_preflight_route_is_not_captured_as_operation_id(self, app_env, client):
        """/v1/gw/restart/preflight must win over the {operationId} route."""
        with patch(
            "herald_connector.http_facade._run_subprocess",
            side_effect=fake_systemctl(show_props=standard_props()),
        ):
            resp = client.get("/v1/gw/restart/preflight?target=hermes")
        assert resp.status_code == 200
        assert resp.json()["$schema"] == "restart-preflight-v1"


# ── Gateway status health report ───────────────────────────────────────────


class TestGatewayStatus:
    def test_health_report_includes_all_probe_results(self, app_env, client):
        observed = {
            "main_pid": 12456,
            "exec_main_start_timestamp": "2026-07-31T19:30:10Z",
            "active_state": "active",
        }
        app_env.paired_device_id = "device-1"  # relayConnected
        app_env.model_catalog = AsyncMock(return_value={
            "activeModel": {"name": "m0"},
            "models": [{"name": f"m{i}"} for i in range(12)],
        })
        facade._last_canary_result = True
        try:
            with patch(
                "herald_connector.http_facade._query_unit_observed",
                return_value=observed,
            ), patch(
                "herald_connector.http_facade._probe_dashboard_health",
                AsyncMock(return_value=(True, "detail health ok")),
            ), patch(
                "herald_connector.http_facade._port_open",
                return_value=True,
            ):
                resp = client.get("/v1/gw/status")
        finally:
            facade._last_canary_result = None

        assert resp.status_code == 200
        payload = resp.json()["data"]  # gateway_status wraps once in the handler
        fixture = load_fixture("health_report.json")
        for key, value in fixture.items():
            assert key in payload, f"health report missing key: {key}"
            if key in ("uptimeSeconds", "sampledAt", "overall", "reasons"):
                # Live values, not the fixture's literal.
                continue
            if key in ("connector", "hermes", "host", "jobs"):
                # Nested objects: only assert presence + the keys that
                # are stable.  Live values like uptimeSeconds / pid vary.
                assert payload[key] is not None
                continue
            assert payload[key] == value, f"health report key mismatch: {key}"
        # The v2 envelope nests Hermes fields under `hermes{}`; the
        # top-level `profile`/`unit`/`mainPid`/`execMainStartTimestamp`
        # moved with them.  In this test the systemd unit is mocked
        # with main_pid=12456, but the actual process PID is whatever
        # TestClient spawned; the singleton flag is therefore
        # computed-False in this test.  Assert the field is present
        # and the managedMainPID is what the mock returned.
        assert payload["hermes"]["profile"] == "ignyte"
        assert payload["hermes"]["unit"] == "hermes-gateway-ignyte.service"
        assert payload["hermes"]["pid"] == 12456
        assert payload["hermes"]["dashboardReady"] is True
        assert payload["connector"]["managedMainPID"] == 12456
        # TestClient is not the same PID as the mocked systemd unit,
        # so the singleton flag is False here; the connector's runtime
        # would say True on the production host.
        assert payload["connector"]["singleton"] is False
        assert payload["connectorConnected"] is True
        assert payload["connectorVersion"] == "2.4.1"
        assert isinstance(payload["connector"]["uptimeSeconds"], int)

    def test_health_report_partial_data_when_probes_fail(self, app_env, client):
        with patch(
            "herald_connector.http_facade._query_unit_observed",
            return_value=None,
        ), patch(
            "herald_connector.http_facade._probe_dashboard_health",
            AsyncMock(return_value=(False, "health endpoint unreachable")),
        ), patch(
            "herald_connector.http_facade._port_open",
            return_value=False,
        ):
            resp = client.get("/v1/gw/status")

        assert resp.status_code == 200  # partial data renders, never a 500
        payload = resp.json()["data"]
        assert payload["hermes"]["state"] == "degraded"
        assert payload["hermes"]["dashboardReady"] is False
        assert payload["connector"]["singleton"] is False
        assert payload["connector"]["portsOwned"] is False
        assert "reasons" in payload
        # At least one reason should reference the failed probes.
        assert any("Hermes" in r for r in payload["reasons"]) or any(
            "Connector" in r for r in payload["reasons"]
        )
