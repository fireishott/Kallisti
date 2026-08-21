# Daily Fix Log - Nightly Reconciliation

Each entry records a fix pushed directly to devices during the day.
The nightly build reconciles these into the nightly branch + TestFlight.

Format: `- YYYY-MM-DD HH:MM PT | build | description`
- 2026-08-20 13:28 PT | a2eea3b4 | test entry - nightly pipeline setup
- 2026-08-20 14:13 PT | 76b703ed | 132.1: thought bubble reset on new note, ALL note attachments (photos/scans/files) included in enrichment prompt, OCR reading-order sort + per-stroke groundwork
- 2026-08-20 15:22 PT | ff4d6310 | 132.2: fix notes ruled lines stopping short on right side in portrait - bounds KVO self-heal in PencilCanvasRepresentable; dev-signed direct install on CDF iPad + CDF iPhone
- 2026-08-20 15:47 PT | a81d2b0f | 132.3: launch-surface flap fix (never re-show after connected), smart completion notifications (real reply text in push+inbox via session history), inbox select-all + bulk dismiss, in-app banner suppression
- 2026-08-20 15:58 PT | 43d4762a | 132.4: retire launch surface once connected + activeModel known - model refresh no longer holds cold start on Connected, <model>
- 2026-08-20 19:25 PT | a3f04936 | 133.0: context-aware note enrichment - model reads drawing/attachments as source of truth, enriches by note type (study/meeting/shopping/personal/doodle), web research + hyperlinks enabled, callback isolation maintained
- 2026-08-20 21:00 PT | bc64423f | Bug 1: enrichment 2x->4x drawing render; Bug 5: composer keyboard after Settings->Chat; driver scale + status resync. 133.1 installed both devices
- 2026-08-20 22:02 PT | 16b5b2f8 | 133.2: enrichment renders markdown (clickable hyperlinks + inline images when model emits them); writing pad sidebar resize (132.2 bounds observer) ships to devices; connector note.enrich prompt synced with hyperlink+inline-image rules
