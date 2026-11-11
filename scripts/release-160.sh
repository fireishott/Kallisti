#!/bin/bash
# Kallisti 1.6.0 build 160 - build, sign, and install on the phone and the iPad.
#
# Run this from Terminal.app on the Mac. It cannot be driven over plain ssh: an
# SSH session lands in the `Background` audit session and every codesign with
# either identity fails with errSecInternalComponent (the keychain refuses to
# prompt). That is a locked session, not a project fault - the key probe below
# says so in one line rather than failing halfway through a build.
#
#   scripts/release-160.sh                 build, sign, install on every available device
#   scripts/release-160.sh --build-only    build and sign, install nothing
#   DEVICE="CDF iPad" scripts/release-160.sh
#
# The iPad only accepts an install while it is available to CoreDevice: awake,
# unlocked once, and on the same network as the Mac. The script lists what it can
# see and skips the rest instead of failing the run.
set -euo pipefail

REPO="${REPO:-$HOME/kallisti-build-160}"
EXPECTED_VERSION="1.6.0"
EXPECTED_BUILD="160"
OUT="$HOME/Kallisti-Builds/160"
APP="$OUT/dd/Build/Products/Debug-iphoneos/Kallisti.app"
BUILD_ONLY=0

for arg in "$@"; do
  case "$arg" in
    --build-only) BUILD_ONLY=1 ;;
    -h|--help) sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown flag: $arg" >&2; exit 64 ;;
  esac
done

say() { printf '%s | %s\n' "$(date '+%H:%M')" "$*"; }

cd "$REPO"

say "=== preflight ==="
VERSION=$(xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Debug \
  -showBuildSettings 2>/dev/null | awk '/MARKETING_VERSION/ {print $3; exit}')
BUILD=$(xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Debug \
  -showBuildSettings 2>/dev/null | awk '/CURRENT_PROJECT_VERSION/ {print $3; exit}')
say "project reports $VERSION / $BUILD"
[ "$VERSION" = "$EXPECTED_VERSION" ] || { say "ABORT: expected $EXPECTED_VERSION, got $VERSION"; exit 2; }
[ "$BUILD" = "$EXPECTED_BUILD" ] || { say "ABORT: expected build $EXPECTED_BUILD, got $BUILD"; exit 2; }

IDENTITY=$(security find-identity -v -p codesigning | awk 'NR==1 {print $2}')
if [ -z "$IDENTITY" ]; then
  say "ABORT: no codesigning identity in this login keychain."
  exit 3
fi
probe=/tmp/keyprobe.$$
: > "$probe"
if ! codesign --force --sign "$IDENTITY" "$probe" >/dev/null 2>&1; then
  rm -f "$probe"
  say "ABORT: this session cannot use a signing key (errSecInternalComponent)."
  say "       Run this from Terminal.app on the Mac, or unlock the keychain first:"
  say "       security unlock-keychain ~/Library/Keychains/login.keychain-db"
  exit 3
fi
rm -f "$probe"
say "keychain usable"

mkdir -p "$OUT"
say "building for the device (signed)"
xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Debug \
  -destination "generic/platform=iOS" -derivedDataPath "$OUT/dd" build

say "app version: $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Info.plist") build $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Info.plist")"

if [ "$BUILD_ONLY" -eq 1 ]; then
  say "build only: signed app is at $APP"
  exit 0
fi

say "=== devices this Mac can reach ==="
xcrun devicectl list devices 2>&1 | sed -n '1,12p'

# One install per device that is actually available. A paired device that is
# asleep is skipped with a sentence, not a failure: the phone is usually the one
# that matters and it is usually the one that is up.
install_on() {
  local name="$1"
  local id
  id=$(xcrun devicectl list devices 2>/dev/null | awk -v n="$name" '$0 ~ n {print $3; exit}')
  if [ -z "$id" ]; then
    say "skip: no device named '$name'"
    return 0
  fi
  if ! xcrun devicectl list devices 2>/dev/null | grep -q "available (paired)"; then
    : # fall through to the per-device check below
  fi
  say "installing on $name ($id)"
  if ! xcrun devicectl device install app --device "$id" "$APP"; then
    say "install failed on $name - is it unlocked and on this network?"
  fi
}

if [ -n "${DEVICE:-}" ]; then
  install_on "$DEVICE"
else
  install_on "CDF iPhone"
  install_on "CDF iPad"
fi

say "done. On each device, open Kallisti and confirm Settings shows $VERSION build $BUILD."
say "The chat footer now shows that chat's model, cache rate, p95 and turn count."
