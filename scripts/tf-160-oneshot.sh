#!/bin/bash
# Run the TestFlight release from ONE ssh session, keychain included.
#
# The trap this exists for: a keychain unlock is scoped to the security session
# that performed it. Unlocking over ssh in one connection and building in the
# next fails with errSecInternalComponent, because the second connection is a
# second session and the keychain is locked again, silently.
#
# So: unlock both keychains, set the partition list on the signing one, probe
# codesign, and exec the release inside this same process.
#
#   scripts/tf-160-oneshot.sh
#
# Reads MBP_KEYCHAIN_PW from ~/.hermes/.env for the login keychain. The signing
# keychain's password is the documented one for kallisti_signing.keychain-db.
set -euo pipefail

REPO="${REPO:-$HOME/kallisti-build-160}"
LOGIN_DB="$HOME/Library/Keychains/login.keychain-db"
SIGN_DB="$HOME/Library/Keychains/kallisti_signing.keychain-db"
DIST="663AF799645F8B5A9DCB66037519ADF3AE18FECE"
SIGN_PW="${KALLISTI_SIGNING_PW:-kallisti2026}"

say() { printf '%s | %s\n' "$(date '+%H:%M:%S')" "$*"; }

say "=== session ==="
say "manager: $(launchctl managername 2>/dev/null || echo unknown)"

[ -f "$SIGN_DB" ] || { say "ABORT: $SIGN_DB missing"; exit 2; }

# The login password. Present in the Mac's own env file; never printed.
if [ -z "${MBP_KEYCHAIN_PW:-}" ] && [ -f "$HOME/.hermes/.env" ]; then
  # shellcheck disable=SC1091
  set -a; . "$HOME/.hermes/.env"; set +a
fi
[ -n "${MBP_KEYCHAIN_PW:-}" ] || { say "ABORT: MBP_KEYCHAIN_PW not in the environment or ~/.hermes/.env"; exit 2; }

say "unlocking the login keychain"
security unlock-keychain -p "$MBP_KEYCHAIN_PW" "$LOGIN_DB"
say "unlocking the signing keychain"
security unlock-keychain -p "$SIGN_PW" "$SIGN_DB"
security set-key-partition-list -S apple-tool:,apple: -s -k "$SIGN_PW" "$SIGN_DB" >/dev/null
say "partition list set"

# The probe that saves an archive: sign a copy of /bin/echo before spending five
# minutes on a build that would fail at the first appex.
cp /bin/echo /tmp/sigtest.$$
if codesign --force --sign "$DIST" /tmp/sigtest.$$ >/tmp/sigtest.out 2>&1; then
  say "probe: codesign usable in this session"
else
  say "ABORT: codesign unusable in this session"
  sed -n '1,3p' /tmp/sigtest.out
  rm -f /tmp/sigtest.$$
  exit 3
fi
rm -f /tmp/sigtest.$$

cd "$REPO"
say "=== release ==="
exec bash "$REPO/scripts/release-160.sh"
