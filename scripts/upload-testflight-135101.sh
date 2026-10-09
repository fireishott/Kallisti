#!/bin/bash
set -euo pipefail
REPO="$HOME/Herald"
OUT="$HOME/Hermes-iOS-Builds/testflight-135.101"
ARCHIVE="$OUT/Kallisti-135.101.xcarchive"
EXPORT="$OUT/export"
LOG="$OUT/upload.log"
KEY="$HOME/.appstoreconnect/private_keys/AuthKey_32NT26772F.p8"
KEY_ID="32NT26772F"
ISSUER="69a6de93-5191-47e3-e053-5b8c7c11a4d1"
mkdir -p "$OUT"
exec > >(tee -a "$LOG") 2>&1
say() { printf "%s | %s\n" "$(date +%H:%M:%S)" "$*"; }
say "=== TestFlight archive/upload: Kallisti 135.101 ==="
cd "$REPO"
git diff --quiet || { say "ABORT: working tree is dirty"; exit 2; }
[ "$(git rev-parse --short HEAD)" = "3d0aba90" ] || { say "ABORT: wrong revision $(git rev-parse --short HEAD)"; exit 2; }
[ "$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" Herald/Resources/Info.plist)" = "135.101" ] || { say "ABORT: wrong app build"; exit 2; }
[ -f "$KEY" ] || { say "ABORT: ASC key missing"; exit 2; }
rm -rf "$ARCHIVE" "$EXPORT"
say "archiving"
xcodebuild -project Herald.xcodeproj -scheme Kallisti -configuration Release \
  -destination generic/platform=iOS -archivePath "$ARCHIVE" \
  DEVELOPMENT_TEAM=58U7UPFS53 CODE_SIGN_STYLE=Automatic archive
say "exporting"
xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportPath "$EXPORT" \
  -exportOptionsPlist "$REPO/ExportOptions.plist"
IPA=$(find "$EXPORT" -name *.ipa -type f | head -1)
[ -n "$IPA" ] || { say "ABORT: IPA missing"; exit 3; }
say "ipa=$IPA"
say "ipa_sha256=$(shasum -a 256 "$IPA" | awk {print })"
say "uploading"
xcrun altool --upload-app -f "$IPA" -t ios --apiKey "$KEY_ID" --apiIssuer "$ISSUER" --apiKeyFile "$KEY" --verbose
say "UPLOAD_SUCCEEDED build=135.101"
