# Daily Fix Log - Nightly Reconciliation

Each entry records a fix pushed directly to devices during the day.
The nightly build reconciles these into the nightly branch + TestFlight.

Format: `- YYYY-MM-DD HH:MM PT | build | description`
- 2026-08-21 07:08 PT | e8463a52 | nightly 133.3 installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 08:11 PT | e8463a52 | 133.4: notes ink auto-scales to fit column when sidebar present (was clipping at right edge)
- 2026-08-21 08:22 PT | 1cba468b | 1cba468b 133.4: autoscale v2 - contentSize=target/zoom coordinate math fix, paper spans content coords, left anchor (installed both devices)
- 2026-08-21 11:22 PT | e3b2943a | 133.5 UAT: phantom new-chat ghost fixes (probe outbox before requeue, cap server-turn watch 75s, label remote turns, stable new-chat id) installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 11:37 PT | 7809a097 | 135.5 TESTFLIGHT RELEASE: promoted nightly->main (e0782af5), phantom new-chat ghost fixes, delivery UUID 5cab2423, VALID
- 2026-08-22 11:19 PT | 233bd523 | 135.10: fix connect() zombie-hang watchdog (device sleep silently suspends URLRequest, isConnecting latched forever -> permanent Connecting screen until force-quit); fix PencilCanvasRepresentable updateUIView churn corrupting the open color/attribute popover during reasoning-stream re-renders (blank gray void bug)
