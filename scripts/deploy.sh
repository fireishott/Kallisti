#!/usr/bin/env bash
#
# Install a freshly built daemon without killing a turn that is still running.
#
# The daemon keeps one worker per running job and the state database knows which
# jobs those are, so a deploy can ask. Restarting under a live turn loses it: the
# boot sweep settles the row as interrupted and the operator sees "the daemon
# stopped before this turn finished" for work they did nothing to stop. That
# happened on 2026-10-29 at 14:48 while a two-minute turn was mid-flight.
#
#   scripts/deploy.sh                 build, wait for idle, install, restart
#   scripts/deploy.sh --skip-build    install the binary already in target/release
#   scripts/deploy.sh --wait 600      wait up to ten minutes for idle
#   scripts/deploy.sh --force         restart now, turn or no turn
#   scripts/deploy.sh --dry-run       say what would happen and stop
#
# Exit codes: 0 deployed (or dry run), 2 nothing to install, 3 the daemon stayed
# busy for the whole wait and nothing was touched.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${KALLISTI_BIN:-$HOME/.local/opt/kallisti/bin/eris}"
DB="${KALLISTI_DB:-$HOME/.local/share/kallisti/eris.db}"
UNIT="kallisti-eris.service"
BUILT="$REPO/target/release/eris"

build=1
wait_secs=300
force=0
dry_run=0

while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) build=0 ;;
    --force) force=1 ;;
    --dry-run) dry_run=1 ;;
    --wait) wait_secs="${2:?--wait needs a number of seconds}"; shift ;;
    -h|--help) sed -n '3,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 64 ;;
  esac
  shift
done

say() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*"; }

# How many turns are running right now. A missing database is not a reason to
# refuse: a fresh host has nothing to wait for.
running() {
  if [ ! -f "$DB" ]; then echo 0; return; fi
  sqlite3 "file:$DB?mode=ro" "SELECT COUNT(*) FROM jobs WHERE status = 'running';" 2>/dev/null || echo 0
}

if [ "$build" -eq 1 ]; then
  say "building release"
  ( cd "$REPO" && cargo build --release )
fi
[ -f "$BUILT" ] || { say "ABORT: no binary at $BUILT (build first)"; exit 2; }

if [ "$dry_run" -eq 1 ]; then
  say "would install $( "$BUILT" version ) over $( [ -x "$BIN" ] && "$BIN" version || echo 'nothing' )"
  say "running turns now: $(running)"
  [ "$(running)" -eq 0 ] && say "idle: the restart would be clean" || say "busy: the restart would wait up to ${wait_secs}s"
  exit 0
fi

if [ "$force" -eq 0 ]; then
  waited=0
  while [ "$(running)" -gt 0 ]; do
    if [ "$waited" -ge "$wait_secs" ]; then
      say "ABORT: a turn is still running after ${wait_secs}s; nothing was touched."
      say "       rerun with --force to restart anyway, or --wait N to wait longer."
      exit 3
    fi
    [ "$waited" -eq 0 ] && say "waiting for the running turn to finish (up to ${wait_secs}s)"
    sleep 5
    waited=$((waited + 5))
  done
  [ "$waited" -gt 0 ] && say "turn finished after ${waited}s, deploying"
fi

stamp="$(date +%Y%m%d-%H%M%S)"
if [ -x "$BIN" ]; then
  cp -p "$BIN" "$BIN.bak-$( "$BIN" version 2>/dev/null | awk '{print $3}' )-$stamp" || true
fi

say "stopping $UNIT"
systemctl --user stop "$UNIT"
install -m 755 "$BUILT" "$BIN"
say "starting $UNIT"
systemctl --user start "$UNIT"

for _ in $(seq 1 20); do
  if systemctl --user is-active --quiet "$UNIT"; then break; fi
  sleep 1
done
systemctl --user is-active --quiet "$UNIT" || { say "ABORT: the unit did not come back"; exit 4; }

sleep 2
say "installed $( "$BIN" version )"
if curl -fsS -m 5 http://127.0.0.1:7340/healthz >/dev/null 2>&1; then
  say "health: ready"
else
  say "health: not answering yet (the unit is up; check the journal)"
fi
