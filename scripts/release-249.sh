#!/bin/bash
# Kallisti 1.8.3 build 249 - sign and upload to TestFlight, watch app included.
#
# Build 219 shipped the phone app and its three extensions. This one embeds
# `KallistiWatch` (and its widget extension) inside the phone app's `Watch`
# folder, so TestFlight carries the wrist.
#
# Signing is explicit and innermost-first. Xcode is told to sign nothing: this
# session cannot complete automatic provisioning (the API key is rejected for
# provisioning), and the profiles are already here.
#
#   scripts/release-244.sh             archive, sign, upload
#   scripts/release-244.sh --dry-run   everything except the upload
set -euo pipefail

REPO="${REPO:-$HOME/kallisti-219}"
EXPECTED_VERSION="1.8.3"
EXPECTED_BUILD="249"
OUT="$HOME/Kallisti-Builds/249"
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
    -h|--help) sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown flag: $arg" >&2; exit 64 ;;
  esac
done

mkdir -p "$OUT"
exec > >(tee -a "$LOG") 2>&1
say() { printf '%s | %s\n' "$(date '+%H:%M:%S')" "$*"; }

cd "$REPO"

say "=== preflight ==="
VERSION=$(xcodebuild -project Kallisti.xcodeproj -scheme Kallisti -configuration Release \
  -showBuildSettings 2>/dev/null | awk '/MARKETING_VERSION/ {print $3; exit}')
BUILD=$(xcodebuild -project Kallisti.xcodeproj -scheme Kallisti -configuration Release \
  -showBuildSettings 2>/dev/null | awk '/CURRENT_PROJECT_VERSION/ {print $3; exit}')
say "project reports $VERSION / $BUILD"
[ "$VERSION" = "$EXPECTED_VERSION" ] || { say "ABORT: expected $EXPECTED_VERSION, got $VERSION"; exit 2; }
[ "$BUILD" = "$EXPECTED_BUILD" ] || { say "ABORT: expected build $EXPECTED_BUILD, got $BUILD"; exit 2; }
[ -f "$ASC_KEY" ] || { say "ABORT: App Store Connect key missing at $ASC_KEY"; exit 2; }
for p in Kallisti_App_Store_v2 Kallisti_Intents_App_Store_v2 \
         Kallisti_NotificationService_App_Store_v2 Kallisti_Widgets_App_Store_v2 \
         "Kallisti Watch App Store" "Kallisti Watch Widgets App Store"; do
  [ -f "$PROFILES/$p.mobileprovision" ] || { say "ABORT: profile missing: $p"; exit 2; }
done
say "preflight ok"

ARCHIVE="$OUT/Kallisti-$EXPECTED_BUILD.xcarchive"
say "=== archive (unsigned; every bundle is signed below) ==="
rm -rf "$ARCHIVE"
xcodebuild -project Kallisti.xcodeproj -scheme Kallisti -configuration Release \
  -destination generic/platform=iOS -archivePath "$ARCHIVE" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGN_IDENTITY="" archive

SRC="$ARCHIVE/Products/Applications/Kallisti.app"
[ -d "$SRC" ] || { say "ABORT: no archived Kallisti.app under $ARCHIVE"; exit 3; }
WATCH="$SRC/Watch/KallistiWatch.app"
[ -d "$WATCH" ] || { say "ABORT: the archive carries no Watch/KallistiWatch.app"; exit 3; }
say "archived $SRC"
say "watch bundle present: $WATCH"

PAYLOAD="$OUT/payload"
IPA="$OUT/Kallisti-${EXPECTED_VERSION}-${EXPECTED_BUILD}.ipa"
rm -rf "$PAYLOAD" "$IPA"
mkdir -p "$PAYLOAD/Payload"
ditto "$SRC" "$PAYLOAD/Payload/Kallisti.app"
APP="$PAYLOAD/Payload/Kallisti.app"
WATCHAPP="$APP/Watch/KallistiWatch.app"

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

say "=== sign (innermost first) ==="
sign_bundle "$WATCHAPP/PlugIns/KallistiWatchWidgets.appex" "Kallisti Watch Widgets App Store" "watchwidgets"
sign_bundle "$WATCHAPP" "Kallisti Watch App Store" "watch"
sign_bundle "$APP/PlugIns/KallistiIntents.appex" "Kallisti_Intents_App_Store_v2" "intents"
sign_bundle "$APP/PlugIns/KallistiNotificationService.appex" "Kallisti_NotificationService_App_Store_v2" "notificationservice"
sign_bundle "$APP/PlugIns/KallistiWidgets.appex" "Kallisti_Widgets_App_Store_v2" "widgets"
sign_bundle "$APP" "Kallisti_App_Store_v2" "app"
codesign --verify --strict --deep "$APP"

say "=== checks ==="
GOT_BUILD=$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$APP/Info.plist")
GOT_VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$APP/Info.plist")
WATCH_BUILD=$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$WATCHAPP/Info.plist")
say "app: $GOT_VERSION build $GOT_BUILD; watch build $WATCH_BUILD"
[ "$GOT_BUILD" = "$EXPECTED_BUILD" ] || { say "ABORT: wrong build in the product"; exit 5; }
[ "$WATCH_BUILD" = "$EXPECTED_BUILD" ] || { say "ABORT: wrong build in the watch bundle"; exit 5; }
codesign -d --entitlements :- "$APP" 2>/dev/null | grep -q '<string>production</string>' \
  || { say "ABORT: production push entitlement missing"; exit 5; }
codesign -d --entitlements :- "$WATCHAPP" 2>/dev/null | grep -q 'group.net.fihonline.kallisti' \
  || { say "ABORT: watch app group entitlement missing after signing"; exit 5; }
codesign --verify --deep --strict "$WATCHAPP"

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
