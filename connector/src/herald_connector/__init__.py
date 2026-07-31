__all__ = ["__version__"]

# Single source of truth for the connector version.  Surfaced to the app as
# `connectorVersion` (client.py:_detect_connector_version → the host payload)
# and rendered in Settings, so bumping it is the only way to tell from the
# phone which connector is actually running.  Keep in step with
# pyproject.toml; connector/tests/test_connector_version.py enforces that.
#
# 0.5.1 — 2026-07-30 reply-delivery fixes: MEDIA: attachment extraction on the
#         /v1/runs path, session-fork resolution by reply rather than prompt,
#         early conversation binding, and conversation-scoped turn locking.
__version__ = "0.5.1"
