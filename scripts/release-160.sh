#!/bin/bash
# Kallisti 1.6.0 build 160 - sign and upload to TestFlight.
#
# Run from Terminal.app on the Mac, or through the GUI-session launcher
# (scripts/tf-160-launchd.sh) when the only shell available is an ssh session.
#
# Why signing is not left to xcodebuild: an SSH session lands in the `Background`
# audit session, and every `codesign` there fails with errSecInternalComponent
# because the keychain refuses to prompt. The manual path below builds unsigned,
# then signs each bundle explicitly with the distribution identity and the
# entitlements inside its provisioning profile - the same steps the build 135
# release used, kept in one file so the next release is not rediscovered.
#
#   scripts/release-160.sh             archive, sign, upload
#   scripts/release-160.sh --dry-run   everything except the upload
#
# TestFlight processes the build in a few minutes; the build number is what rises,
# so 160 must not have been uploaded before.
set -euo pipefail

REPO="${REPO:-$HOME/kallisti-build-160}"
EXPECTED_VERSION="1.6.0"
EXPECTED_BUILD="160"
OUT="$HOME/Kallisti-Builds/160"
PROFILES="$HOME/Library/MobileDevice/Provisioning Profiles"
DIST_ID="663AF799645F8B5A9DCB66037519ADF3AE18FECE"   # iPhone Distribution: C Freeman
ASC_KEY_ID="32NT26772F"
ASC_ISSUER="69a6de93-5191-47e3-e053-5b8c7c11a4d1"
ASC_KEY="$HOME/.appstoreconnect/private_keys/AuthKey_${ASC_KEY_ID}.p8"
LOG="$OUT/release.log"
DRY_RUN=0

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown flag: $arg" >&2; exit 64 ;;
  esac
done

mkdir -p "$OUT"
exec > >(tee -a "$LOG") 2>&1
say() { printf '%s | %s\n' "$(date '+%H:%M:%S')" "$*"; }

cd "$REPO"

say "=== preflight ==="
VERSION=$(xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Release \
  -showBuildSettings 2>/dev/null | awk '/MARKETING_VERSION/ {print $3; exit}')
BUILD=$(xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Release \
  -showBuildSettings 2>/dev/null | awk '/CURRENT_PROJECT_VERSION/ {print $3; exit}')
say "project reports $VERSION / $BUILD"
[ "$VERSION" = "$EXPECTED_VERSION" ] || { say "ABORT: expected $EXPECTED_VERSION, got $VERSION"; exit 2; }
[ "$BUILD" = "$EXPECTED_BUILD" ] || { say "ABORT: expected build $EXPECTED_BUILD, got $BUILD"; exit 2; }
[ -f "$ASC_KEY" ] || { say "ABORT: App Store Connect key missing at $ASC_KEY"; exit 2; }
for p in Kallisti_App_Store_v2 Kallisti_Intents_App_Store_v2 \
         Kallisti_NotificationService_App_Store_v2 Kallisti_Widgets_App_Store_v2; do
  [ -f "$PROFILES/$p.mobileprovision" ] || { say "ABORT: profile missing: $p"; exit 2; }
done
say "preflight ok"

DD="$OUT/dd"
say "=== build (unsigned: xcodebuild cannot sign over ssh) ==="
xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Release \
  -destination "generic/platform=iOS" -derivedDataPath "$DD" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build

SRC="$DD/Build/Products/Release-iphoneos/Kallisti.app"
[ -d "$SRC" ] || SRC="$DD/Build/Products/Debug-iphoneos/Kallisti.app"
[ -d "$SRC" ] || { say "ABORT: no built Kallisti.app under $DD"; exit 3; }
say "built $SRC"

PAYLOAD="$OUT/payload"
IPA="$OUT/Kallisti-${EXPECTED_VERSION}-${EXPECTED_BUILD}.ipa"
rm -rf "$PAYLOAD" "$IPA"
mkdir -p "$PAYLOAD/Payload"
ditto "$SRC" "$PAYLOAD/Payload/Kallisti.app"
APP="$PAYLOAD/Payload/Kallisti.app"

# One bundle at a time, innermost first, with the entitlements that bundle's
# profile actually grants. Signing the app before its extensions is the classic
# way to ship an IPA whose appex signature no longer matches.
sign_bundle() {
  local target="$1" profile="$2" label="$3"
  local prov="$PROFILES/$profile.mobileprovision"
  [ -d "$target" ] || { say "ABORT: $label bundle missing at $target"; exit 4; }
  cp "$prov" "$target/embedded.mobileprovision"
  security cms -D -i "$prov" -o "/tmp/kallisti-profile-$label.plist"
  plutil -extract Entitlements xml1 -o "/tmp/kallisti-ent-$label.plist" "/tmp/kallisti-profile-$label.plist"
  say "signing $label"
  codesign --force --sign "$DIST_ID" --entitlements "/tmp/kallisti-ent-$label.plist" "$target"
  codesign --verify --strict "$target"
}

say "=== sign ==="
sign_bundle "$APP/PlugIns/KallistiIntents.appex" "Kallisti_Intents_App_Store_v2" "intents"
sign_bundle "$APP/PlugIns/KallistiNotificationService.appex" "Kallisti_NotificationService_App_Store_v2" "notificationservice"
sign_bundle "$APP/PlugIns/KallistiWidgets.appex" "Kallisti_Widgets_App_Store_v2" "widgets"
sign_bundle "$APP" "Kallisti_App_Store_v2" "app"
codesign --verify --strict --deep "$APP"

say "=== checks ==="
GOT_BUILD=$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$APP/Info.plist")
GOT_VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$APP/Info.plist")
say "app: $GOT_VERSION build $GOT_BUILD"
[ "$GOT_BUILD" = "$EXPECTED_BUILD" ] || { say "ABORT: wrong build in the product"; exit 5; }
codesign -d --entitlements :- "$APP" 2>/dev/null | grep -q '<string>production</string>' \
  || { say "ABORT: production push entitlement missing"; exit 5; }

ditto -c -k --keepParent "$PAYLOAD/Payload" "$IPA"
say "ipa=$IPA"
say "ipa_sha256=$(shasum -a 256 "$IPA" | awk '{print $1}')"

if [ "$DRY_RUN" -eq 1 ]; then
  say "dry run: not uploading"
  exit 0
fi

say "=== upload to TestFlight ==="
xcrun altool --upload-app -f "$IPA" -t ios \
  --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER" --apiKeyFile "$ASC_KEY" --verbose
say "UPLOAD_SUCCEEDED build=$EXPECTED_VERSION ($EXPECTED_BUILD)"
say "TestFlight processing takes a few minutes; the build is visible in App Store Connect first."
