#!/bin/bash
# Armed Eris TestFlight upload for Kallisti build 87.
#
# Waits until the codesign gate is genuinely open (Aqua session + screen unlocked +
# login keychain readable), then runs upload-testflight-eris-89.sh EXACTLY once.
# Polls ONLY non-prompting gate checks - never a real codesign probe - so a locked
# console never sprays keychain password dialogs.
#
# Runs from the Aqua launchd agent com.fih.kallisti-testflight-eris89 so it lives in
# the GUI session and can actually sign once the screen is unlocked. After the upload
# script runs, this agent removes its plist and boots itself out so it cannot re-fire
# on the next login.
set -u

REPO="$HOME/Herald"
UPLOAD="$REPO/scripts/upload-testflight-eris-89.sh"
LABEL="com.fih.kallisti-testflight-eris89"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
DIR="$HOME/.hermes/kallisti-deploy"
LOG="$DIR/eris89-upload.log"

mkdir -p "$DIR"
exec > >(tee -a "$LOG") 2>&1
say() { printf "%s | %s\n" "$(date '+%H:%M:%S')" "$*"; }

say "=== armed Eris upload: Kallisti 89 (gate-wait -> upload once) ==="

[ -x "$UPLOAD" ] || { say "ABORT: $UPLOAD missing or not executable"; exit 2; }

# Wake the display once so it cannot sleep out from under the build. Bounded to 1h so
# it self-terminates even if this script is orphaned.
CAFFEINATE_PID=""
caffeinate -d -u -t 3600 &
CAFFEINATE_PID=$!
say "caffeinate -d -u started (pid $CAFFEINATE_PID)"

gate_open() {
  # 1) Aqua (GUI) session only - an SSH/Background session can never codesign.
  [ "$(launchctl managername 2>/dev/null)" = "Aqua" ] || return 1
  # 2) Screen unlocked. Non-prompting and checked FIRST so the keychain check below
  #    only runs once the console is already unlocked (never while it is locked).
  if ioreg -n Root -d1 2>/dev/null | grep -qi 'IOConsoleLocked.*Yes'; then
    return 1
  fi
  # 3) login keychain readable == console logged in + keychain unlocked. Safe here:
  #    with the screen already unlocked the login keychain is not lock-on-sleep locked.
  security show-keychain-info "$HOME/Library/Keychains/login.keychain-db" >/dev/null 2>&1
}

tries=0
until gate_open; do
  tries=$(( tries + 1 ))
  if [ $(( tries % 6 )) -eq 1 ]; then
    mgr=$(launchctl managername 2>/dev/null)
    locked=$(ioreg -n Root -d1 2>/dev/null | grep -i 'IOConsoleLocked' | head -1)
    say "waiting for codesign gate (heartbeat, ~$(( (tries / 6) + 1 ))min: managername=$mgr; $locked)"
  fi
  sleep 10
done

say "codesign gate OPEN (Aqua + screen unlocked + login keychain readable) - running upload exactly once"
"$UPLOAD" 2>&1
rc=$?
say "upload script exit=$rc"

[ -n "$CAFFEINATE_PID" ] && kill "$CAFFEINATE_PID" 2>/dev/null || true
say "stopped caffeinate (pid $CAFFEINATE_PID)"

# Remove the plist FIRST so the agent cannot re-fire on next login even if bootout races.
rm -f "$PLIST"
say "removed $PLIST; booting out $LABEL"
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
exit "$rc"
