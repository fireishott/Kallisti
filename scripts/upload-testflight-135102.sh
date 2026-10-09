#!/bin/bash
# TestFlight pipeline for Kallisti 135.102 (inline images in Kallisti with the DSH backend).
#
# Why manual signing: the automatic export path (exportArchive) failed on 135.101 with
# "exportArchive Failed to Use Accounts" - it needs an Xcode account session that an
# SSH/launchd run does not have. The archive step itself succeeds, so this script
# archives, then re-signs the payload with the Apple Distribution identity and the
# App Store profiles, then uploads with the ASC API key (no keychain needed for upload).
set -euo pipefail

REPO="$HOME/Herald"
BUILD="135.102"
EXPECTED_HEAD="c8871baf"
OUT="$HOME/Hermes-iOS-Builds/testflight-$BUILD"
ARCHIVE="$OUT/Kallisti-$BUILD.xcarchive"
PAYLOAD="$OUT/manual-payload"
IPA="$OUT/Kallisti-$BUILD.ipa"
LOG="$OUT/upload.log"
PROFILES="$HOME/Library/MobileDevice/Provisioning Profiles"
DIST="663AF799645F8B5A9DCB66037519ADF3AE18FECE"
KEY="$HOME/.appstoreconnect/private_keys/AuthKey_32NT26772F.p8"
KEY_ID="32NT26772F"
ISSUER="69a6de93-5191-47e3-e053-5b8c7c11a4d1"

mkdir -p "$OUT"
exec > >(tee -a "$LOG") 2>&1
say() { printf "%s | %s\n" "$(date '+%H:%M:%S')" "$*"; }

say "=== TestFlight: Kallisti $BUILD (archive -> App Store re-sign -> upload) ==="
cd "$REPO"

git diff --quiet || { say "ABORT: working tree is dirty"; exit 2; }
[ "$(git rev-parse --short HEAD)" = "$EXPECTED_HEAD" ] || {
  say "ABORT: wrong revision $(git rev-parse --short HEAD), expected $EXPECTED_HEAD"; exit 2; }
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' Herald/Resources/Info.plist)" = "$BUILD" ] || {
  say "ABORT: app plist is not $BUILD"; exit 2; }
for p in Kallisti_App_Store_v2 Kallisti_Intents_App_Store_v2 \
         Kallisti_NotificationService_App_Store_v2 Kallisti_Widgets_App_Store_v2; do
  [ -f "$PROFILES/$p.mobileprovision" ] || { say "ABORT: profile $p missing"; exit 3; }
done
[ -f "$KEY" ] || { say "ABORT: ASC key missing"; exit 3; }
security find-identity -v -p codesigning | grep -q "$DIST" || {
  say "ABORT: distribution identity $DIST not available in this session"; exit 3; }
say "preflight ok (clean tree at $EXPECTED_HEAD, profiles + ASC key + distribution identity present)"

rm -rf "$ARCHIVE" "$PAYLOAD" "$IPA"
say "archiving Release (this is the slow step)"
xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Release \
  -destination generic/platform=iOS -archivePath "$ARCHIVE" \
  DEVELOPMENT_TEAM=58U7UPFS53 CODE_SIGN_STYLE=Automatic archive

APP_SRC="$ARCHIVE/Products/Applications/Kallisti.app"
[ -d "$APP_SRC" ] || { say "ABORT: archive app missing"; exit 4; }
say "archive built"

mkdir -p "$PAYLOAD/Payload"
ditto "$APP_SRC" "$PAYLOAD/Payload/Kallisti.app"
APP="$PAYLOAD/Payload/Kallisti.app"

sign_target() {
  local target="$1" profile="$2" label="$3"
  local prov="$PROFILES/$profile"
  [ -d "$target" ] || { say "ABORT: $label bundle missing"; exit 5; }
  [ -f "$prov" ] || { say "ABORT: $label profile missing"; exit 5; }
  cp "$prov" "$target/embedded.mobileprovision"
  security cms -D -i "$prov" -o /tmp/kallisti-profile-$label.plist
  plutil -extract Entitlements xml1 -o /tmp/kallisti-entitlements-$label.plist /tmp/kallisti-profile-$label.plist
  codesign --force --sign "$DIST" --entitlements /tmp/kallisti-entitlements-$label.plist "$target"
  codesign --verify --strict --verbose=2 "$target" >/dev/null 2>&1
  say "  signed $label"
}

say "re-signing for App Store"
sign_target "$APP/PlugIns/KallistiIntents.appex" "Kallisti_Intents_App_Store_v2.mobileprovision" "Intents"
sign_target "$APP/PlugIns/KallistiNotificationService.appex" "Kallisti_NotificationService_App_Store_v2.mobileprovision" "NotificationService"
sign_target "$APP/PlugIns/KallistiWidgets.appex" "Kallisti_Widgets_App_Store_v2.mobileprovision" "Widgets"
sign_target "$APP" "Kallisti_App_Store_v2.mobileprovision" "Kallisti"
codesign --verify --strict --deep --verbose=2 "$APP"

/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$APP/Info.plist" | grep -qx "$BUILD" || {
  say "ABORT: signed bundle is not $BUILD"; exit 6; }
codesign -d --entitlements :- "$APP" 2>/dev/null | grep -q '<string>production</string>' || {
  say "ABORT: production push entitlement missing"; exit 6; }
say "signed bundle verified ($BUILD, production push entitlement present)"

ditto -c -k --keepParent "$PAYLOAD/Payload" "$IPA"
say "ipa=$IPA"
say "ipa_sha256=$(shasum -a 256 "$IPA" | awk '{print $1}')"
say "uploading to App Store Connect"
xcrun altool --upload-app -f "$IPA" -t ios --apiKey "$KEY_ID" --apiIssuer "$ISSUER" --apiKeyFile "$KEY" --verbose
say "UPLOAD_SUCCEEDED build=$BUILD"
say "next: verify processingState via ~/.hermes/kallisti-deploy/asc_build_status.py"
