#!/bin/bash
# Finish Herald build 116: archive -> export IPA -> upload to TestFlight.
# Prereq: login keychain unlocked (see unlock step printed by the assistant).
set -euo pipefail
cd /Users/you/Herald

ARCH=build/Herald-116.xcarchive
EXPORT=build/Herald-116-export
KEY_ID=32NT26772F
ISSUER=69a6de93-5191-47e3-e053-5b8c7c11a4d1

echo "== 1/3 archive (signed) =="
xcodebuild archive \
  -project Herald.xcodeproj -scheme Herald -configuration Release \
  -archivePath "$ARCH" -destination 'generic/platform=iOS' \
  -allowProvisioningUpdates DEVELOPMENT_TEAM=58U7UPFS53

echo "== 2/3 export IPA (app-store) =="
rm -rf "$EXPORT"
xcodebuild -exportArchive -archivePath "$ARCH" -exportPath "$EXPORT" \
  -exportOptionsPlist ExportOptions-b116.plist -allowProvisioningUpdates

echo "== 3/3 upload to TestFlight =="
IPA=$(ls "$EXPORT"/*.ipa | head -1)
echo "uploading $IPA"
xcrun altool --upload-app -f "$IPA" -t ios \
  --apiKey "$KEY_ID" --apiIssuer "$ISSUER"

echo "DONE. Build 116 uploaded. It will show in App Store Connect after processing (a few min)."
echo "Next: link build 116 to the 'Herald External Testers' group (15f612a1-b8f4-450a-8891-7447e932fd5a)."
