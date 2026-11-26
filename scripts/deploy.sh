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
# busy for the whole wait and nothing was touched, 4 untracked source files.
set -euo pipefail

# A working tree with untracked source files is a tree git has never seen. The
# build would succeed and the deploy would install code nobody can reproduce from
# HEAD. That happened twice on 2026-11-16: diff.rs and sensitivity.rs both shipped
# in binaries while sitting untracked, and both broke a clone at HEAD without
# breaking a single build.
if [ "${KALLISTI_ALLOW_UNTRACKED:-0}" != "1" ]; then
  untracked="$(git -C "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" \
    status --porcelain --untracked-files=normal 2>/dev/null \
    | awk '$1 == "??" && $2 ~ /\.(rs|toml)$/ { print $2 }')"
  if [ -n "$untracked" ]; then
    echo "ABORT: untracked source files; add them first or set KALLISTI_ALLOW_UNTRACKED=1" >&2
    printf '  %s\n' $untracked >&2
    exit 4
  fi
fi

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
#
# An expired lease is not a running turn. A worker that died mid-turn leaves its
# row at `running` with a lease that has already lapsed, and counting that as busy
# made this script wait five minutes and then refuse to deploy forever. Only a row
# whose lease is still live can be a turn somebody is watching.
running() {
  if [ ! -f "$DB" ]; then echo 0; return; fi
  # A row that is `running` counts as busy even if its lease has lapsed.
  #
  # This used to exclude a lapsed lease, on the theory that a dead worker leaves a
  # stale `running` row that nobody is watching. The theory was wrong in the case
  # that matters: the lease is renewed by the turn's own heartbeat, and a turn
  # waiting on one long tool call emitted no stream output and so stopped renewing
  # while it was still working. Excluding lapsed leases read that live turn as idle,
  # the restart killed it, and the boot sweep settled it as "the daemon stopped
  # before this turn finished" - a reply thrown away, and a Retry that had to start
  # over. On 2026-11-26 at 04:11 UTC that is exactly what happened to a fifteen-round
  # turn.
  #
  # A genuinely dead worker is the rarer case and the boot sweep already handles it.
  # Waiting a little longer for a turn that is really gone costs a delay; restarting
  # under a turn that is really alive costs the operator their answer.
  # `kind != 'tool'` because a tool job is a background PROCESS, not a chat turn.
  # It carries no lease (its own sweep checks whether the pid survived) and it can
  # legitimately outlive any deploy. Counting those made this script refuse to
  # deploy forever behind one stuck `p...` row.
  sqlite3 "file:$DB?mode=ro" \
    "SELECT COUNT(*) FROM jobs WHERE status = 'running' AND kind != 'tool';" \
    2>/dev/null || echo 0
}

if [ "$build" -eq 1 ]; then
  say "building release"
  # An installed binary embeds the path of every source file it can panic from,
  # and that path carries the login of whoever built it. Remapped to `/build` and
  # `/cargo` so a local build matches what the release workflow ships. Expanded
  # here because cargo does not expand `$HOME` inside `rustflags`.
  ( cd "$REPO" && \
    RUSTFLAGS="--remap-path-prefix=$HOME=/build --remap-path-prefix=$HOME/.cargo=/cargo" \
    cargo build --release )
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

# Keep the install prefix bounded. Every deploy writes a full copy of the
# previous binary, and nothing used to prune them: 159 files and 3.7 GB by
# build 183. Five timestamped backups is enough to step back through a bad
# ship, and the oldest is kept as the pre-release anchor nothing else replaces.
KEEP_BAK=5
if [ -d "$(dirname "$BIN")" ]; then
  # shellcheck disable=SC2012
  ls -1t "$(dirname "$BIN")"/"$(basename "$BIN")".bak-* 2>/dev/null \
    | tail -n +$((KEEP_BAK + 1)) \
    | while IFS= read -r old; do
        [ "$(basename "$old")" = "$(basename "$BIN").bak-p6-20261019" ] && continue
        rm -f -- "$old"
      done
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
