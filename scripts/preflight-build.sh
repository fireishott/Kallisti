#!/bin/bash
# Refuses to build/ship from a base that silently dropped the authoritative line.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
anchor=$(cat .build-anchor)
git merge-base --is-ancestor "$anchor" HEAD \
  || { echo "FATAL: HEAD does not descend from the authoritative anchor ($anchor). You are on a stale base — see 2026-08-03 mass revert."; exit 1; }
ver=$(grep -Eo '__version__ = "[0-9.]+"' connector/src/herald_connector/__init__.py | grep -Eo '[0-9.]+')
python3 - "$ver" <<'PY' || { echo "FATAL: connector version regressed below 0.9.5 ($ver)"; exit 1; }
import sys
v = tuple(map(int, sys.argv[1].split(".")))
sys.exit(0 if v >= (0, 9, 5) else 1)
PY
echo "preflight OK: anchored, connector $ver"
