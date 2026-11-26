#!/usr/bin/env bash
#
# Move the daemon's build number on, the way `project.yml` moves the app's.
#
# The daemon's build number used to be a literal in `admin.rs`. It drifted four
# releases behind while `--version` and the Settings card kept saying 219, so the
# running binary and the number that identifies it disagreed. It now lives in the
# `BUILD` file at the repository root, and this is the one command that moves it.
#
#   scripts/bump-build.sh        increment and print the new number
#   scripts/bump-build.sh 235    set an exact number
#   scripts/bump-build.sh --show print without changing anything
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FILE="$REPO/BUILD"

[ -f "$FILE" ] || { echo "ABORT: no BUILD file at $FILE" >&2; exit 2; }
current="$(tr -dc '0-9' < "$FILE")"
[ -n "$current" ] || { echo "ABORT: BUILD holds no number" >&2; exit 2; }

case "${1:---next}" in
  --show) echo "$current"; exit 0 ;;
  --next|"") next=$((current + 1)) ;;
  *[!0-9]*) echo "ABORT: not a number: $1" >&2; exit 64 ;;
  *) next="$1" ;;
esac

[ "$next" -gt "$current" ] || {
  echo "ABORT: $next is not above the current build $current" >&2; exit 2
}
printf '%s\n' "$next" > "$FILE"
echo "$next"
