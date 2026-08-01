__all__ = ["__version__", "HERALD_PROTOCOL"]
__version__ = "0.9.2"

# Minimum connector version the app MUST negotiate.  The iOS app sends this
# protocol version in every POST /v1/messages request; the connector rejects
# mismatches so a stale TestFlight build cannot silently fall back to an old
# contract.  Bump only when a clean-break protocol change requires coordinated
# app + connector deployment.

HERALD_PROTOCOL = 5

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
# 0.6.1 — 2026-07-31 Build 30: talk-and-ux-fixes, maintenance controls,
#         transcript reducer and device isolation, protocol 3.
# 0.6.2 — 2026-07-31 Build 31: attachment execution envelope (structured
#         /v1/runs attachments + clean-text overrides), per-device tokens,
#         server-acknowledged job cancellation, protocol 4.
# 0.7.0 — 2026-08-01 Build 34: per-connection delivery-store schema validation
#         (the empty-replacement Build 34 incident), deliveryStoreReady health
#         probe, protocol 5.
# 0.8.0 — 2026-08-01 Build 103 WS-A/C/D/E: canonical chat identity via native
#         POST /api/sessions; real Hermes gateway logs from
#         profiles/{profile}/logs/* (path-traversal guarded); truthful live
#         Gateway Status with port ownership, restart count, and CPU
#         interval-sampled telemetry; real Hermes update check via
#         `hermes update --check`; protocol 5 (additive, backward-compatible).
# 0.9.0 — 2026-08-01 Build 104: repair legacy canonical conversation aliases,
# proxy MiMo ASR through the authenticated connector, and make Talk readiness
# reject invalid upstream credentials instead of advertising a false-ready mic.
