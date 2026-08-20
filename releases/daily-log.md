# Daily Fix Log - Nightly Reconciliation

Each entry records a fix pushed directly to devices during the day.
The nightly build reconciles these into the nightly branch + TestFlight.

Format: `- YYYY-MM-DD HH:MM PT | build | description`
- 2026-08-20 13:28 PT | a2eea3b4 | test entry - nightly pipeline setup
- 2026-08-20 14:13 PT | 76b703ed | 132.1: thought bubble reset on new note, ALL note attachments (photos/scans/files) included in enrichment prompt, OCR reading-order sort + per-stroke groundwork
- 2026-08-20 15:22 PT | ff4d6310 | 132.2: fix notes ruled lines stopping short on right side in portrait - bounds KVO self-heal in PencilCanvasRepresentable; dev-signed direct install on CDF iPad + CDF iPhone
