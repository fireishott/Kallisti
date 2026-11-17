#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
pattern=$(printf 'h%smes|open%slaw' er c)
if git grep -Iin -E "$pattern" -- . ':!target'; then
  echo "retired namespace token present" >&2
  exit 1
fi
