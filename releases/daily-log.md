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
- 2026-08-22 12:16 PT | d52ca286 | 135.11: break checkpoint write/re-render feedback loop (re-entrancy latch + throttle + no redundant state writes) + memory-warning/willTerminate safety save
- 2026-08-22 13:01 PT | 2d38d3e3 | 135.12: memory-leak diagnostics + OCR churn reduction - in-app phys_footprint logger (30s + memory-warning), cap recognition render at 2000px longest side (was unbounded 4x full-canvas), skip OCR when drawing content unchanged
- 2026-08-22 13:15 PT | 46885045 | 135.13: FIX instant-close - removed the 135.11 didReceiveMemoryWarning observer that called persistDrawing+OCR render at the moment iOS demanded free memory (guaranteed Jetsam kill); willTerminate now writes blob directly without OCR; observers removed on disappear
- 2026-08-22 13:38 PT | 6039888f | 135.14: FIX checkpoint ballooning - pruneOldCheckpoints was never called, so every snapshot (5min auto + backgrounding + restore + manual) copied FULL drawing + ALL attachment blobs (multi-MB screen recordings) into a new bundle that accumulated unbounded = disk-write storm + memory pressure. Now prunes to latest 10 after every snapshot.
- 2026-08-22 13:53 PT | 94f4b7f5 | 135.15: FIX chats flying off screen - userScrollTimer was declared but never scheduled, so any accidental drag left isUserScrolling=true forever (streaming piled up off-screen, Jump-to-Latest arrow blocked). Drag end now schedules a 2.5s grace timer that releases scroll ownership so auto-follow resumes and the arrow always works.
- 2026-08-22 14:08 PT | 13005c05 | 135.16: FIX Queue sheet row buttons dead - .contentShape(Rectangle()) + .onTapGesture on the row container swallowed every tap so Send/Edit/Delete buttons inside never fired. Switched to simultaneousGesture so the row actions receive taps.
- 2026-08-22 14:36 PT | 7c661569 | 135.17: stop checkpoint from melting long-note devices (metadata-only snapshots - no attachment blob copies) + live background-task viewer in Canvas Live tab (SSE process stream + Stop button)
- 2026-08-22 14:49 PT | 62a3a93d | 135.18: REMOVE entire checkpoint system from notes (auto loop, backgrounding snapshots, manual UI, restore, Settings picker) - was causing instant-close on opening saved notes and device melt
- 2026-08-22 15:10 PT | 7ec0a399 | 135.19: FIX camera dead buttons - Use Photo/Retake left fullScreenCover presented (showCamera never reset + system picker never dismissed); app froze until force-quit
- 2026-08-22 15:26 PT | ec4141aa | 135.20: FIX notes instant-close - revisions.json embedded FULL drawing blob per revision (100MB+); note-open loaded/decoded the whole file -> 3GB Jetsam kill. Now metadata-only + migrate-on-load + cap 30 revisions
- 2026-08-22 15:28 PT | ba890482 | 135.21: FIX custom color picker (PKToolPicker attribute palette) vanishing - updateUIView forced becomeFirstResponder/setVisible on every parent re-render while the palette popover was open; now never touches responder/visibility when picker is visible
- 2026-08-22 15:33 PT | 0759c546 | 135.22: FIX notes instant-close for the bloated legacy notes - pre-flight migration shrinks oversized revisions.json (huge embedded drawingData blobs) BEFORE decoding so note-open never loads 100-300MB into memory
- 2026-08-28 17:09 PT | 257f5c3a | notes enrichment image visibility (vision prompt relax + content-first title + enrich-sourced smart title) + Live Activity terminal choke-point (stuck Responding fix)
