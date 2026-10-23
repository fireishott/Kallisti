#!/bin/bash
# Armed Eris TestFlight upload for Kallisti build 90.
#
# Waits until the codesign gate is genuinely open (Aqua session + screen unlocked +
# login keychain readable), then runs upload-testflight-eris-90.sh EXACTLY once.
# Polls ONLY non-prompting gate checks - never a real codesign probe - so a locked
# console never sprays keychain password dialogs.
#
# Runs from the Aqua launchd agent com.fih.kallisti-testflight-eris90 so it lives in
# the GUI session and can actually sign once the screen is unlocked. After the upload
# script runs, this agent removes its plist and boots itself out so it cannot re-fire
# on the next login.
set -u

REPO="$HOME/Herald"
UPLOAD="$REPO/scripts/upload-testflight-eris-90.sh"
LABEL="com.fih.kallisti-testflight-eris90"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
DIR="$HOME/.hermes/kallisti-deploy"
LOG="$DIR/eris90-upload.log"

mkdir -p "$DIR"
exec > >(tee -a "$LOG") 2>&1
say() { printf "%s | %s\n" "$(date '+%H:%M:%S')" "$*"; }

say "=== armed Eris upload: Kallisti 90 (gate-wait -> upload once) ==="

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

# --- keychain prep: the gate proved the session, this proves the keys ------
# The distribution identity lives only in kallisti_signing.keychain-db, which has
# a lock-on-idle policy. codesign resolves to it, so it has to be open before the
# upload script signs anything or every signature raises errSecInternalComponent.
ENVF="$HOME/.hermes/.env"
if [ -f "$ENVF" ]; then set -a; . "$ENVF"; set +a; fi
if [ -n "${MBP_KEYCHAIN_PW:-}" ]; then
  security unlock-keychain -p "$MBP_KEYCHAIN_PW" "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null \
    && say "keychain: login unlocked"
  security set-keychain-settings -t 7200 "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null || true
fi
SPW=$(security find-generic-password -a "$USER" -s kallisti-nightly-signing -w 2>/dev/null || true)
if [ -n "$SPW" ]; then
  if security unlock-keychain -p "$SPW" "$HOME/Library/Keychains/kallisti_signing.keychain-db" 2>/dev/null; then
    security set-keychain-settings -t 7200 "$HOME/Library/Keychains/kallisti_signing.keychain-db" 2>/dev/null || true
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$SPW" \
      "$HOME/Library/Keychains/kallisti_signing.keychain-db" >/dev/null 2>&1 || true
    say "keychain: kallisti_signing unlocked"
  else
    say "keychain: kallisti_signing unlock FAILED"
  fi
  unset SPW
else
  say "keychain: kallisti-nightly-signing item not readable in this session"
fi
cp /bin/echo /tmp/kctest90
if codesign --force --keychain "$HOME/Library/Keychains/kallisti_signing.keychain-db" \
     --sign 663AF799645F8B5A9DCB66037519ADF3AE18FECE /tmp/kctest90 >/dev/null 2>&1; then
  say "codesign probe: ok (one probe, never a loop)"
else
  say "codesign probe: FAILED - not running the upload, the signature would not stick"
  rm -f /tmp/kctest90
  rm -f "$PLIST"
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  exit 9
fi
rm -f /tmp/kctest90

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
