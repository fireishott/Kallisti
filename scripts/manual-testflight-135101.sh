#!/bin/bash
set -euo pipefail
OUT="$HOME/Hermes-iOS-Builds/testflight-135.101"
ARCHIVE="$OUT/Kallisti-135.101.xcarchive"
PAYLOAD="$OUT/manual-payload"
IPA="$OUT/Kallisti-135.101.ipa"
LOG="$OUT/manual-upload.log"
PROFILES="$HOME/Library/MobileDevice/Provisioning Profiles"
DIST="663AF799645F8B5A9DCB66037519ADF3AE18FECE"
KEY="$HOME/.appstoreconnect/private_keys/AuthKey_32NT26772F.p8"
KEY_ID="32NT26772F"
ISSUER="69a6de93-5191-47e3-e053-5b8c7c11a4d1"
mkdir -p "$OUT"
exec > >(tee -a "$LOG") 2>&1
say() { printf "%s | %s\n" "$(date '+%H:%M:%S')" "$*"; }
say "=== manual App Store sign/upload: Kallisti 135.101 ==="
APP_SRC="$ARCHIVE/Products/Applications/Kallisti.app"
[ -d "$APP_SRC" ] || { say "ABORT: archive app missing"; exit 2; }
[ -f "$KEY" ] || { say "ABORT: ASC key missing"; exit 2; }
rm -rf "$PAYLOAD" "$IPA"
mkdir -p "$PAYLOAD/Payload"
ditto "$APP_SRC" "$PAYLOAD/Payload/Kallisti.app"
APP="$PAYLOAD/Payload/Kallisti.app"
sign_target() {
  local target="$1" profile="$2" label="$3"
  local prov="$PROFILES/$profile"
  [ -d "$target" ] || { say "ABORT: $label bundle missing"; exit 3; }
  [ -f "$prov" ] || { say "ABORT: $label profile missing"; exit 3; }
  cp "$prov" "$target/embedded.mobileprovision"
  security cms -D -i "$prov" -o /tmp/kallisti-profile.plist
  plutil -extract Entitlements xml1 -o /tmp/kallisti-entitlements.plist /tmp/kallisti-profile.plist
  codesign --force --sign "$DIST" --entitlements /tmp/kallisti-entitlements.plist "$target"
  codesign --verify --strict --verbose=2 "$target"
  say "signed $label"
}
sign_target "$APP/PlugIns/KallistiIntents.appex" "Kallisti_Intents_App_Store_v2.mobileprovision" "Intents"
sign_target "$APP/PlugIns/KallistiNotificationService.appex" "Kallisti_NotificationService_App_Store_v2.mobileprovision" "NotificationService"
sign_target "$APP/PlugIns/KallistiWidgets.appex" "Kallisti_Widgets_App_Store_v2.mobileprovision" "Widgets"
sign_target "$APP" "Kallisti_App_Store_v2.mobileprovision" "Kallisti"
codesign --verify --strict --deep --verbose=2 "$APP"
/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$APP/Info.plist" | grep -qx 135.101 || { say "ABORT: wrong IPA build"; exit 4; }
codesign -d --entitlements :- "$APP" 2>/dev/null | grep -q '<string>production</string>' || { say "ABORT: production push entitlement missing"; exit 4; }
ditto -c -k --keepParent "$PAYLOAD/Payload" "$IPA"
say "ipa=$IPA"
say "ipa_sha256=$(shasum -a 256 "$IPA" | awk '{print $1}')"
say "uploading"
xcrun altool --upload-app -f "$IPA" -t ios --apiKey "$KEY_ID" --apiIssuer "$ISSUER" --apiKeyFile "$KEY" --verbose
say "UPLOAD_SUCCEEDED build=135.101"
