#!/bin/bash
# Kallisti 1.6.0 build 159 - the interactive half of the release.
#
# Why this is not automated end to end: an SSH session on this Mac lands in the
# `Background` audit session, `security show-keychain-info` answers "User
# interaction is not allowed", and every codesign attempt with either identity
# (Apple Development, or the Distribution cert) fails with errSecInternalComponent.
# That is a locked keychain, not a project fault - a probe that signs an empty file
# fails the same way. So this script does everything that does not need a key and
# then stops, rather than failing halfway through a signing step.
#
# Run from Terminal.app on the Mac (or after `security unlock-keychain`), not over
# plain ssh.
set -euo pipefail

REPO="${REPO:-$HOME/kallisti-build-159}"
DEVICE_ID="${DEVICE_ID:-8BE18B66-1D74-5495-82FA-F8A74B505947}"   # CDF iPhone
OUT="$HOME/Kallisti-Builds/159"
ARCHIVE="$OUT/Kallisti-159.xcarchive"
APP="$OUT/Debug-ios/Kallisti.app"

say() { printf '%s | %s\n' "$(date '+%H:%M')" "$*"; }

cd "$REPO"

say "=== preflight ==="
VERSION=$(xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Debug \
  -showBuildSettings 2>/dev/null | awk '/MARKETING_VERSION/ {print $3; exit}')
BUILD=$(xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Debug \
  -showBuildSettings 2>/dev/null | awk '/CURRENT_PROJECT_VERSION/ {print $3; exit}')
say "project reports $VERSION / $BUILD"
[ "$VERSION" = "1.6.0" ] || { say "ABORT: expected 1.6.0, got $VERSION"; exit 2; }
[ "$BUILD" = "159" ] || { say "ABORT: expected 159, got $BUILD"; exit 2; }

# The keychain is the only thing that can stop this, so check it before building.
if ! codesign --force --sign "$(security find-identity -v -p codesigning | awk 'NR==1 {print $2}')" /tmp/keyprobe.$$ >/dev/null 2>&1; then
  : > /tmp/keyprobe.$$
  if ! codesign --force --sign "$(security find-identity -v -p codesigning | awk 'NR==1 {print $2}')" /tmp/keyprobe.$$ >/dev/null 2>&1; then
    rm -f /tmp/keyprobe.$$
    say "ABORT: this session cannot use a signing key (errSecInternalComponent)."
    say "       Run from Terminal.app on the Mac, or unlock the login keychain first:"
    say "       security unlock-keychain ~/Library/Keychains/login.keychain-db"
    exit 3
  fi
fi
rm -f /tmp/keyprobe.$$
say "keychain usable"

mkdir -p "$OUT"
say "building for the device (signed)"
xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Debug \
  -destination "generic/platform=iOS" -derivedDataPath "$OUT/dd" build

say "app version: $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$OUT/dd/Build/Products/Debug-iphoneos/Kallisti.app/Info.plist") build $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$OUT/dd/Build/Products/Debug-iphoneos/Kallisti.app/Info.plist")"

say "installing on $DEVICE_ID"
xcrun devicectl device install app --device "$DEVICE_ID" \
  "$OUT/dd/Build/Products/Debug-iphoneos/Kallisti.app"

say "done. Open the app and confirm Settings shows 1.6.0 build 159 and that Search,"
say "Processes and Memory open against the paired daemon."
