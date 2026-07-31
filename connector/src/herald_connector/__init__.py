__all__ = ["__version__"]

# Single source of truth for the connector version.  Surfaced to the app as
# `connectorVersion` (client.py:_detect_connector_version → the host payload)
# and rendered in Settings, so bumping it is the only way to tell from the
# phone which connector is actually running.  Keep in step with
# pyproject.toml; connector/tests/test_connector_version.py enforces that.
#
# 0.5.3 — 2026-07-31 Build 25: attachment persistence, dedup heuristic attempt.
# 0.5.4 — 2026-07-31 Build 26: typed thought/progress classification, validated
#         attachment store, facade download endpoint with security hardening.
# 0.5.5 — 2026-07-31 Build 27: fix live reasoning.available duplication at SSE
#         source; fix attachment DTO key (thumbnailData); preserve messageID and
#         remoteIndex through mergeAttachments.
__version__ = "0.5.5"
