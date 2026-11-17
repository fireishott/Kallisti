#!/usr/bin/env bash
# Fail when an operator identifier reaches the tracked tree.
#
# The repository is public and it is also the release source: what lands here is
# what a stranger clones. Name, login, work address and LAN address are not part
# of the product, so they are not part of the tree.
#
# Two identifiers are deliberately absent from this list, because removing them
# breaks a running install rather than protecting anybody:
#
#   net.fihonline.kallisti   the shipped iOS bundle id; the app is signed under
#                            it and APNs delivery addresses it by name.
#   fireishott/Kallisti      the repository the daemon updates itself from.
#                            `release.rs` refuses any other host, `install.sh`
#                            defaults to it, and the README install line resolves
#                            to it: changing it here breaks updates and moves no
#                            remote.
#
# The pattern is assembled rather than written so this file is grep-clean against
# itself, the same reason `check-namespace.sh` does it.
set -euo pipefail
cd "$(dirname "$0")/.."

home_dir=$(printf '/home/%s' 'fihadmin')
lan=$(printf '192\\.168\\.%s\\.' '68')
work=$(printf 'curtis\\.%s' 'freeman')
icloud=$(printf '%s@icloud\\.com' 'fihassistant')
pattern="$home_dir|$lan|$work|$icloud"

if git grep -Iin -E "$pattern" -- . ':!target'; then
  echo "operator identifier present in the tracked tree" >&2
  exit 1
fi
echo "no operator identifiers in the tracked tree"
