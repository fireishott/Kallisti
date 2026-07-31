__all__ = ["__version__"]

# Single source of truth for the connector version.  Surfaced to the app as
# `connectorVersion` (client.py:_detect_connector_version → the host payload)
# and rendered in Settings, so bumping it is the only way to tell from the
# phone which connector is actually running.  Keep in step with
# pyproject.toml; connector/tests/test_connector_version.py enforces that.
#
# 0.5.2 — 2026-07-31 Build 23: terminal message projection preserves attachment
#         metadata through the SSE done→iOS bridge; explicit deliveryStatus on
#         _relay_message (pending user row is "sent", not "delivered").
__version__ = "0.5.2"
