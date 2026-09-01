#!/bin/bash
# nightly-build.sh - reconcile day fixes, sync Mark's plugin-engine, build nightly
# Runs nightly (cron). Reconciles releases/daily-log.md into the nightly branch,
# merges origin/plugin-engine when it has new commits, bumps build, archives,
# exports, re-signs, builds UAT IPA. NEVER uploads to TestFlight/ASC (main owns that).
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

# 8. Build UAT IPA (no upload) - MANUAL SIGN PATH
# FIX 2026-08-31: the previous flow (archive CODE_SIGNING_ALLOWED=NO ->
# xcodebuild exportArchive with signingStyle manual) broke because exportArchive
# needs App Store Connect account auth headless ("No Accounts") and the
# automatic-signed archive picked development profiles without the
# App Groups/HealthKit/Push/Siri entitlements. Every nightly since the
# 3-lane split (cfe8bc94) died at re-sign with no resigned IPA.
#
# New flow: build unsigned, copy the .app, sign each target manually with
# its app-store provisioning profile + the distribution cert, zip the IPA.
# No account auth needed, fully deterministic.
echo "--- archive (unsigned) ---"
rm -rf /tmp/Kallisti_nightly.xcarchive /tmp/nightly_export /tmp/nightly_payload
security unlock-keychain -p "$PW" ~/Library/Keychains/login.keychain-db
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PW" ~/Library/Keychains/login.keychain-db
security unlock-keychain -p kallisti2026 ~/Library/Keychains/kallisti_signing.keychain-db
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k kallisti2026 ~/Library/Keychains/kallisti_signing.keychain-db

xcodebuild -project Herald.xcodeproj -scheme Kallisti \
  -destination "generic/platform=iOS" -configuration Release \
  -archivePath /tmp/Kallisti_nightly.xcarchive \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO \
  archive 2>&1 | tail -2

APP_SRC=$(find /tmp/Kallisti_nightly.xcarchive/Products -name "Kallisti.app" -type d | head -1)
mkdir -p /tmp/nightly_payload/Payload
ditto "$APP_SRC" /tmp/nightly_payload/Payload/Kallisti.app
cd /tmp/nightly_payload/Payload

echo "--- sign (manual, app-store profiles) ---"
DIST="663AF799645F8B5A9DCB66037519ADF3AE18FECE"
PAIRS=(
  "Kallisti.app/PlugIns/KallistiIntents.appex|Kallisti_Intents_App_Store.mobileprovision"
  "Kallisti.app/PlugIns/KallistiNotificationService.appex|Kallisti_NotificationService_App_Store.mobileprovision"
  "Kallisti.app/PlugIns/KallistiWidgets.appex|Kallisti_Widgets_App_Store.mobileprovision"
  "Kallisti.app/PlugIns/KallistiControls.appex|Kallisti_Controls_App_Store.mobileprovision"
)
for pair in "${PAIRS[@]}"; do
  target="${pair%%|*}"
  prof="${pair##*|}"
  [ -d "$target" ] || { echo "SKIP $target"; continue; }
  PROV="$HOME/Library/MobileDevice/Provisioning Profiles/$prof"
  cp "$PROV" "$target/embedded.mobileprovision"
  security cms -D -i "$PROV" -o /tmp/prov.plist
  plutil -extract Entitlements xml1 -o /tmp/ent.plist /tmp/prov.plist
  codesign -f -s "$DIST" --entitlements /tmp/ent.plist "$target"
  codesign --verify --strict "$target"
  echo "signed $target"
done

PROV="$HOME/Library/MobileDevice/Provisioning Profiles/Kallisti_App_Store.mobileprovision"
cp "$PROV" Kallisti.app/embedded.mobileprovision
security cms -D -i "$PROV" -o /tmp/prov_main.plist
plutil -extract Entitlements xml1 -o /tmp/ent_main.plist /tmp/prov_main.plist
codesign -f -s "$DIST" --entitlements /tmp/ent_main.plist Kallisti.app
codesign --verify --strict --deep Kallisti.app
codesign -d --entitlements :- Kallisti.app 2>/dev/null | grep -q "aps-environment" || { echo "MISSING aps-environment on MAIN"; exit 1; }
echo "signed main + aps OK"

cd /tmp/nightly_payload
rm -f /tmp/nightly_export/Kallisti_resigned.ipa
mkdir -p /tmp/nightly_export
ditto -c -k --keepParent Payload /tmp/nightly_export/Kallisti_resigned.ipa

echo "--- UAT IPA built (no TestFlight upload - nightly is UAT only, main owns ASC) ---"
echo "IPA: /tmp/nightly_export/Kallisti_resigned.ipa"

echo "--- reset daily log ---"
rm -f "$LOG"
echo "=== NIGHTLY $NEXT COMPLETE ==="
