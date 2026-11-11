#!/bin/bash
# Run the TestFlight release from a GUI-session LaunchAgent.
#
# The problem this solves: `codesign` refuses to use a signing key from an ssh
# session ("errSecInternalComponent", SecurityAgent cannot prompt in the
# Background audit session). A LaunchAgent runs in the user's Aqua session, where
# the keychain prompt can appear - so this stages the very same release script
# under launchd, writes its output to a log, and leaves the upload to it.
#
#   scripts/tf-160-launchd.sh          stage and start; prints the log path
#   scripts/tf-160-launchd.sh --status print the log from the last run
#
# The first run may show a keychain prompt on the Mac ("codesign wants to access
# a key"). Click Always Allow once and the whole release proceeds unattended.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="com.fih.tf160"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Kallisti-Builds/160/launcher.log"
UID_NUM="$(id -u)"

if [ "${1:-}" = "--status" ]; then
  echo "=== launcher ==="; tail -20 "$LOG" 2>/dev/null || echo "no launcher log"
  echo "=== release ==="; tail -20 "$HOME/Kallisti-Builds/160/release.log" 2>/dev/null || echo "no release log"
  exit 0
fi

mkdir -p "$HOME/Kallisti-Builds/160"
cat > "$PLIST" <<PLIST_END
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$REPO/scripts/release-160.sh</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict><key>REPO</key><string>$REPO</string></dict>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict></plist>
PLIST_END

launchctl bootout "gui/$UID_NUM/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$UID_NUM" "$PLIST"
echo "started $LABEL in the GUI session"
echo "logs: $LOG and $HOME/Kallisti-Builds/160/release.log"
