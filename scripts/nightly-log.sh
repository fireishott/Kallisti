#!/bin/bash
# nightly-log.sh - record a direct-device fix for the nightly reconciliation
# Usage: nightly-log.sh "Short description of the fix"
# Appends to ~/Herald/releases/daily-log.md with timestamp + build info.
set -euo pipefail

DESC="$*"
if [ -z "$DESC" ]; then
  echo "usage: nightly-log.sh \"fix description\""
  exit 1
fi

LOG=~/Herald/releases/daily-log.md
mkdir -p "$(dirname "$LOG")"
[ -f "$LOG" ] || cat > "$LOG" <<'HEADER'
# Daily Fix Log - Nightly Reconciliation

Each entry records a fix pushed directly to devices during the day.
The nightly build reconciles these into the nightly branch + TestFlight.

Format: `- YYYY-MM-DD HH:MM PT | build | description`
HEADER

STAMP="$(date '+%Y-%m-%d %H:%M PT')"
BUILD="$(cd ~/Herald && git log --oneline -1 2>/dev/null | awk '{print $1}')"

echo "- $STAMP | $BUILD | $DESC" >> "$LOG"
echo "logged: $DESC"
echo "file: $LOG"
