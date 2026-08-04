"""MCP server exposing location and health sensor data to Herald agents.

Run as:  herald-mcp
Configure in ~/.hermes/config.yaml:
    mcp_servers:
      herald_mobile:
        command: "/path/to/.venv/bin/herald-mcp"
"""

from __future__ import annotations

import json
import os
from pathlib import Path

from mcp.server.fastmcp import FastMCP
from mcp.server.transport_security import TransportSecuritySettings

from .sensor_store import (
    HEALTH_STALE_AFTER_SECONDS,
    LOCATION_STALE_AFTER_SECONDS,
    SensorStore,
    freshness_metadata,
)
from .state import ConnectorStateStore

mcp = FastMCP(
    "herald",
    instructions="Provides real-time location and health data from the user's phone.",
    host="0.0.0.0",
    transport_security=TransportSecuritySettings(
        enable_dns_rebinding_protection=False,
    ),
)


def _get_store() -> SensorStore:
    state_store = ConnectorStateStore()
    db_path = state_store.state_dir / "sensors.db"
    return SensorStore(db_path)


@mcp.tool()
def get_user_location() -> str:
    """Get the user's current location (latitude, longitude, address).

    Returns the most recent location reading from the user's phone.
    Use this when the user asks about their current location, wants
    nearby recommendations, or needs location-aware assistance.
    """
    store = _get_store()
    try:
        current = store.get_current_location()
        if current is None:
            return json.dumps({"error": "No location data available yet. The user's phone may not have sent a location update."})
        return json.dumps(
            {
                "latitude": current.latitude,
                "longitude": current.longitude,
                "altitude": current.altitude,
                "accuracy": current.accuracy,
                "address": current.address,
                **freshness_metadata(
                    recorded_at=current.recorded_at,
                    updated_at=current.updated_at,
                    stale_after_seconds=LOCATION_STALE_AFTER_SECONDS,
                ),
            }
        )
    finally:
        store.close()


@mcp.tool()
def get_location_history(since: str | None = None, limit: int = 50) -> str:
    """Get the user's recent location history.

    Returns a trail of location readings ordered newest first.
    Use this for queries like "where have I been today" or to understand
    the user's travel patterns.

    Args:
        since: ISO8601 timestamp to filter from (e.g. "2026-04-01T00:00:00Z")
        limit: Maximum number of entries to return (default 50)
    """
    store = _get_store()
    try:
        history = store.get_location_history(since=since, limit=limit)
        return json.dumps(
            {
                "locations": history,
                "count": len(history),
                "current": store.get_location_freshness(),
            }
        )
    finally:
        store.close()


@mcp.tool()
def get_health_summary() -> str:
    """Get a summary of all the user's latest health metrics.

    Returns current values for all tracked health metrics (steps, heart rate,
    calories, sleep, etc). Use this for general health queries or when the
    user asks about their overall health status.
    """
    store = _get_store()
    try:
        summary = store.get_health_summary()
        return json.dumps(summary)
    finally:
        store.close()


@mcp.tool()
def get_health_metric(metric: str, since: str | None = None, limit: int = 50) -> str:
    """Get time-series data for a specific health metric.

    Returns historical samples for the requested metric ordered newest first.
    Available metrics: steps, active_calories, distance_walking, heart_rate,
    resting_heart_rate, blood_oxygen, respiratory_rate, body_mass,
    workout_minutes, stand_hours, sleep_duration.

    Args:
        metric: The metric name (e.g. "steps", "heart_rate", "sleep_duration")
        since: ISO8601 timestamp to filter from (e.g. "2026-04-01T00:00:00Z")
        limit: Maximum number of samples to return (default 50)
    """
    store = _get_store()
    try:
        samples = store.get_health_metric(metric, since=since, limit=limit)
        latest_freshness = store.get_metric_freshness(metric)
        return json.dumps(
            {
                "metric": metric,
                "samples": samples,
                "count": len(samples),
                "latest": latest_freshness,
            }
        )
    finally:
        store.close()


@mcp.tool()
def get_health_metrics_list() -> str:
    """List all available health metrics and their latest values.

    Returns the names of all health metrics that have been recorded,
    along with their most recent values. Use this to discover what
    health data is available before querying specific metrics.
    """
    store = _get_store()
    try:
        metrics = store.get_latest_metrics()
        return json.dumps(
            {
                "metrics": [
                    {
                        "metric": m.metric,
                        "value": m.value,
                        "unit": m.unit,
                        **freshness_metadata(
                            recorded_at=m.recorded_at,
                            updated_at=m.updated_at,
                            stale_after_seconds=HEALTH_STALE_AFTER_SECONDS,
                        ),
                    }
                    for m in metrics
                ],
                "count": len(metrics),
            }
        )
    finally:
        store.close()


ACTIVITY_LABELS = {0: "stationary", 1: "walking", 2: "running", 3: "automotive", 4: "cycling", 5: "unknown"}


@mcp.tool()
def get_user_activity() -> str:
    """Get the user's current physical activity (stationary, walking, running, driving, cycling).

    Returns the latest activity classification from the device's motion sensors,
    with freshness metadata. Stale if older than 15 minutes.
    """
    store = _get_store()
    try:
        row = store._read_conn.execute(
            "SELECT * FROM health_latest WHERE metric = 'user_activity'"
        ).fetchone()
        if row is None:
            return json.dumps({"activity": "unknown", "available": False})
        activity_code = int(row["value"])
        label = ACTIVITY_LABELS.get(activity_code, "unknown")
        meta = freshness_metadata(
            recorded_at=row["recorded_at"],
            updated_at=row["updated_at"],
            stale_after_seconds=LOCATION_STALE_AFTER_SECONDS,
        )
        return json.dumps({"activity": label, "activityCode": activity_code, **meta})
    finally:
        store.close()


@mcp.tool()
def get_sensor_schema() -> str:
    """Return the SQLite schema for the sensor database.

    Shows all table definitions, column types, and indexes.
    Use this to understand the data structure before writing
    custom queries with query_sensor_data.
    """
    store = _get_store()
    try:
        conn = store._conn
        cursor = conn.execute(
            "SELECT sql FROM sqlite_master WHERE type IN ('table', 'index') AND sql IS NOT NULL ORDER BY type, name"
        )
        statements = [row[0] for row in cursor.fetchall()]
        return json.dumps({"schema": statements})
    finally:
        store.close()


@mcp.tool()
def query_sensor_data(sql: str, limit: int = 100) -> str:
    """Run a read-only SQL query against the sensor database.

    Use this for custom analysis, trend queries, aggregations, or
    building dashboards. The database contains tables: location_current,
    location_history, health_samples, health_latest, health_daily.

    Args:
        sql: A SELECT query. Only SELECT statements are allowed.
        limit: Maximum rows to return (default 100, max 1000).

    Example queries:
        - "SELECT metric, AVG(value) FROM health_samples WHERE metric='steps' GROUP BY date(start_at)"
        - "SELECT * FROM location_history ORDER BY recorded_at DESC LIMIT 10"
        - "SELECT metric, value, unit FROM health_latest"
    """
    # Safety checks
    stripped = sql.strip().upper()
    if not stripped.startswith("SELECT"):
        return json.dumps({"error": "Only SELECT queries are allowed."})

    forbidden = {"DROP", "DELETE", "INSERT", "UPDATE", "ALTER", "CREATE", "ATTACH", "DETACH", "PRAGMA"}
    first_words = set(stripped.split()[:3])
    if first_words & forbidden:
        return json.dumps({"error": "Destructive or administrative statements are not allowed."})

    effective_limit = min(max(limit, 1), 1000)

    store = _get_store()
    try:
        # Open a separate read-only connection for user queries.
        # Even if the SQL contains injection (DROP, INSERT, ATTACH, etc.),
        # the read-only mode prevents any writes at the SQLite level.
        import sqlite3
        ro_conn = sqlite3.connect(f"file:{store.db_path}?mode=ro", uri=True)
        ro_conn.row_factory = sqlite3.Row
        try:
            safe_sql = f"SELECT * FROM ({sql.rstrip().rstrip(';')}) LIMIT {effective_limit}"
            cursor = ro_conn.execute(safe_sql)
            columns = [desc[0] for desc in cursor.description] if cursor.description else []
            rows = cursor.fetchall()
            return json.dumps({
                "columns": columns,
                "rows": [dict(zip(columns, row)) for row in rows],
                "count": len(rows),
            })
        finally:
            ro_conn.close()
    except Exception as e:
        return json.dumps({"error": str(e)})
    finally:
        store.close()


@mcp.tool()
def register_push_device(device_token: str, environment: str = "production") -> str:
    """Register an iOS device's APNs token for push notifications.

    Call this when the iOS app receives a device token from APNs.
    The connector will use this token to send push notifications directly
    via APNs instead of routing through the relay.

    Args:
        device_token: The hex-encoded APNs device token from iOS
        environment: "production" or "development" (default: production)
    """
    state_store = ConnectorStateStore()
    state = state_store.load()
    state.device_token = device_token
    state.device_token_environment = environment
    state_store.save(state)
    return json.dumps({
        "status": "ok",
        "device_token": device_token[:8] + "...",
        "environment": environment,
    })


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(prog="herald-mcp")
    parser.add_argument(
        "--transport",
        choices=["stdio", "streamable-http"],
        default="streamable-http",
        help="MCP transport (default: streamable-http)",
    )
    parser.add_argument(
        "--host",
        default=os.environ.get("HERALD_MCP_HOST", "0.0.0.0"),
        help="Bind address for HTTP transport (default: 0.0.0.0)",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("HERALD_MCP_PORT", "8767")),
        help="Port for HTTP transport (default: 8767)",
    )
    args = parser.parse_args()

    if args.transport == "streamable-http":
        mcp.settings.host = args.host
        mcp.settings.port = args.port
        mcp.run(transport="streamable-http")
    else:
        mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
