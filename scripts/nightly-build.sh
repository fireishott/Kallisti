#!/bin/bash
# nightly-build.sh - reconcile day fixes, sync Mark's plugin-engine, build nightly
# Runs nightly (cron). Reconciles releases/daily-log.md into the nightly branch,
# merges origin/plugin-engine when it has new commits, bumps build, archives,
# exports, re-signs, uploads to TestFlight.
set -euo pipefail

PW='11aaxx2wR'
LOG=~/Herald/releases/daily-log.md
cd ~/Herald

echo "=== NIGHTLY BUILD $(date '+%Y-%m-%d %H:%M PT') ==="

# 1. Sync with remote + Mark's plugin-engine
echo "--- fetch ---"
git fetch origin 2>&1 | tail -2

# 2. Make sure we're on nightly
git checkout nightly 2>&1 | tail -1

# 3. Merge plugin-engine when it has new commits (Mark's work)
if git rev-parse --verify origin/plugin-engine >/dev/null 2>&1; then
  BEHIND=$(git rev-list --count HEAD..origin/plugin-engine)
  if [ "$BEHIND" -gt 0 ]; then
    echo "--- merging plugin-engine ($BEHIND commits) ---"
    git merge origin/plugin-engine -m "nightly: merge plugin-engine ($BEHIND commits)" 2>&1 | tail -3
  else
    echo "plugin-engine: no new commits"
  fi
fi

# 4. Reconcile daily log into changelog if there are entries
if [ -f "$LOG" ]; then
  ENTRIES=$(grep -c '^- ' "$LOG" || true)
  if [ "$ENTRIES" -gt 0 ]; then
    echo "--- reconciling $ENTRIES daily fix entries ---"
    # Append daily fixes to CHANGELOG under a NIGHTLY section
    cat >> CHANGELOG.md <<'EOF'

## [Nightly] - $(date '+%Y-%m-%d')

### Fixed (daily reconciliation)

EOF
    grep '^- ' "$LOG" >> CHANGELOG.md
    echo "--- daily log appended to CHANGELOG ---"
  fi
fi

# 5. Determine next build number from current
CUR=$(grep -oE 'CURRENT_PROJECT_VERSION = [0-9.]+' Herald.xcodeproj/project.pbxproj | head -1 | grep -oE '[0-9.]+$')
echo "current build: $CUR"
# Nightly uses the same marketing version, increments build by 0.1 (e.g. 132.0 -> 132.1)
NEXT=$(python3 -c "v='$CUR'; parts=v.split('.'); parts[-1]=str(int(parts[-1])+1); print('.'.join(parts))")
echo "next build: $NEXT"

# 6. Version bump (all 8 files)
python3 - "$CUR" "$NEXT" <<'PY'
import io, sys
old, new = sys.argv[1], sys.argv[2]
files = [
    "Herald.xcodeproj/project.pbxproj",
    "project.yml",
    "Herald/Resources/Info.plist",
    "KallistiControls/Info.plist",
    "KallistiIntents/Info.plist",
    "KallistiNotificationService/Info.plist",
    "KallistiWatch/Info.plist",
    "KallistiWidgets/Info.plist",
]
for p in files:
    src = io.open(p, encoding="utf-8").read()
    if old in src:
        io.open(p, "w", encoding="utf-8").write(src.replace(old, new))
        print(f"  bumped {p}: {old} -> {new}")
PY

# 7. Commit the reconciliation + bump
git add -A
git commit -m "nightly: reconcile daily fixes, bump to $NEXT" 2>&1 | tail -1
git push origin nightly 2>&1 | tail -1

# 8. Build TestFlight
echo "--- archive ---"
rm -rf /tmp/Kallisti_nightly.xcarchive /tmp/nightly_export /tmp/nightly_payload
security unlock-keychain -p "$PW" ~/Library/Keychains/login.keychain-db
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PW" ~/Library/Keychains/login.keychain-db
security unlock-keychain -p kallisti2026 ~/Library/Keychains/kallisti_signing.keychain-db
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k kallisti2026 ~/Library/Keychains/kallisti_signing.keychain-db

if [ ! -f /tmp/ExportOptions-b90-manual.plist ]; then
  cp /tmp/ExportOptionsManual.plist /tmp/ExportOptions-b90-manual.plist
fi

xcodebuild -project Herald.xcodeproj -scheme Kallisti \
  -destination "generic/platform=iOS" -configuration Release \
  -archivePath /tmp/Kallisti_nightly.xcarchive \
  -allowProvisioningUpdates CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO \
  archive 2>&1 | tail -2

echo "--- export ---"
xcodebuild -exportArchive \
  -archivePath /tmp/Kallisti_nightly.xcarchive \
  -exportOptionsPlist /tmp/ExportOptions-b90-manual.plist \
  -allowProvisioningUpdates \
  -exportPath /tmp/nightly_export 2>&1 | tail -2

echo "--- re-sign ---"
mkdir -p /tmp/nightly_payload
ditto -xk /tmp/nightly_export/Kallisti.ipa /tmp/nightly_payload
cd /tmp/nightly_payload/Payload
MAIN="Kallisti.app"
for ext in "$MAIN/PlugIns/"*.appex; do
  [ -d "$ext" ] || continue
  name=$(basename "$ext")
  security cms -D -i "$ext/embedded.mobileprovision" -o /tmp/prov.plist 2>/dev/null
  plutil -extract Entitlements xml1 -o /tmp/ent.plist /tmp/prov.plist
  codesign -f -s "iPhone Distribution: C Freeman (58U7UPFS53)" --entitlements /tmp/ent.plist "$ext"
  codesign --verify --strict "$ext"
done
security cms -D -i "$MAIN/embedded.mobileprovision" -o /tmp/prov_main.plist 2>/dev/null
plutil -extract Entitlements xml1 -o /tmp/ent_main.plist /tmp/prov_main.plist
codesign -f -s "iPhone Distribution: C Freeman (58U7UPFS53)" --entitlements /tmp/ent_main.plist "$MAIN"
codesign --verify --strict --deep "$MAIN"
codesign -d --entitlements :- "$MAIN" 2>/dev/null | grep -q "aps-environment" || { echo "MISSING aps-environment on MAIN"; exit 1; }
cd /tmp/nightly_payload
rm -f /tmp/nightly_export/Kallisti_resigned.ipa
ditto -c -k --keepParent Payload /tmp/nightly_export/Kallisti_resigned.ipa

echo "--- upload ---"
xcrun altool --upload-app \
  -f /tmp/nightly_export/Kallisti_resigned.ipa \
  -t ios \
  --apiKey "32NT26772F" \
  --apiIssuer "69a6de93-5191-47e3-e053-5b8c7c11a4d1" \
  --apiKeyFile "$HOME/.appstoreconnect/private_keys/AuthKey_32NT26772F.p8" 2>&1 | tail -4

echo "--- reset daily log ---"
rm -f "$LOG"
echo "=== NIGHTLY $NEXT COMPLETE ==="
