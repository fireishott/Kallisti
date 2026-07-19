# HERALD Rebrand Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename and rebrand the Hermes-iOS fork to HERALD — new identity, bundle IDs, Xcode project, source directories, relay/connector, brand assets, and documentation.

**Architecture:** XcodeGen (`project.yml`) is the source of truth for the Xcode project. We update `project.yml`, rename source directories to match, then run `xcodegen generate` to produce a clean `Herald.xcodeproj`. Relay and connector changes are pure string replacements. Assets are placed from `~/Desktop/herald-rebrand/`.

**Tech Stack:** Swift/SwiftUI (iOS), Python/FastAPI (relay), Python (connector), XcodeGen, bash

## Global Constraints

- Bundle ID: `net.fihonline.herald` (main app), `net.fihonline.herald.Widgets`, `net.fihonline.herald.tests`, `net.fihonline.herald.uitests`
- App group: `group.net.fihonline.herald`
- Version: `MARKETING_VERSION=1.0.0`, `CURRENT_PROJECT_VERSION=1`, `CFBundleShortVersionString=1.0.0`, `CFBundleVersion=1`
- Development team: `58U7UPFS53` (set at build time, not in `project.yml`)
- `DEVELOPMENT_TEAM: ""` stays blank in `project.yml` (open-source convention; override at build time)
- Git history preserved — no squash, no rebase
- Upstream remote stays: `https://github.com/dylan-buck/Hermes-iOS.git`
- Relay API endpoints unchanged (`/v1/` prefix stays)
- Connector MCP tool names unchanged — only display strings updated
- All work in `~/Hermes-iOS/`

---

## Task 1: Update project.yml

**Files:**
- Modify: `project.yml`

**Interfaces:**
- Produces: updated XcodeGen config that `xcodegen generate` will read in Task 2

- [ ] **Step 1: Replace the project name and bundleIdPrefix**

```bash
cd ~/Hermes-iOS
perl -i -pe '
  s/^name: HermesMobile$/name: Herald/;
  s/bundleIdPrefix: io\.hermesmobile/bundleIdPrefix: net.fihonline/;
' project.yml
```

- [ ] **Step 2: Rename target definitions and their source paths**

```bash
perl -i -pe '
  # Target names (must appear at column 2 as YAML keys)
  s/^  HermesMobile:$/  Herald:/;
  s/^  HermesMobileWidgets:$/  HeraldWidgets:/;
  s/^  HermesMobileTests:$/  HeraldTests:/;
  s/^  HermesMobileUITests:$/  HeraldUITests:/;
  # Source paths
  s|path: HermesMobile/HermesMobile\.entitlements|path: Herald/Herald.entitlements|;
  s|path: HermesMobile/Resources/Info\.plist|path: Herald/Resources/Info.plist|;
  s|- path: HermesMobile$|- path: Herald|;
  s|- path: HermesMobileWidgets$|- path: HeraldWidgets|;
  s|- path: HermesMobileTests$|- path: HeraldTests|;
  s|- path: HermesMobileUITests$|- path: HeraldUITests|;
  s|path: HermesMobileWidgets/HermesMobileWidgets\.entitlements|path: HeraldWidgets/HeraldWidgets.entitlements|;
  s|path: HermesMobileWidgets/Info\.plist|path: HeraldWidgets/Info.plist|;
  # Resources path
  s|- path: HermesMobile/Resources|- path: Herald/Resources|;
' project.yml
```

- [ ] **Step 3: Update bundle IDs, product names, and app groups**

```bash
perl -i -pe '
  s/PRODUCT_BUNDLE_IDENTIFIER: io\.hermesmobile\.HermesMobile\.Widgets/PRODUCT_BUNDLE_IDENTIFIER: net.fihonline.herald.Widgets/;
  s/PRODUCT_BUNDLE_IDENTIFIER: io\.hermesmobile\.HermesMobile\.tests/PRODUCT_BUNDLE_IDENTIFIER: net.fihonline.herald.tests/;
  s/PRODUCT_BUNDLE_IDENTIFIER: io\.hermesmobile\.HermesMobile\.uitests/PRODUCT_BUNDLE_IDENTIFIER: net.fihonline.herald.uitests/;
  s/PRODUCT_BUNDLE_IDENTIFIER: io\.hermesmobile\.HermesMobile/PRODUCT_BUNDLE_IDENTIFIER: net.fihonline.herald/;
  s/PRODUCT_NAME: HermesMobileWidgets/PRODUCT_NAME: HeraldWidgets/;
  s/PRODUCT_NAME: HermesMobile/PRODUCT_NAME: Herald/;
  s/INFOPLIST_KEY_CFBundleDisplayName: HermesMobile/INFOPLIST_KEY_CFBundleDisplayName: Herald/;
  s/group\.io\.hermesmobile\.HermesMobile/group.net.fihonline.herald/g;
  s/- target: HermesMobileWidgets/- target: HeraldWidgets/;
  s/TEST_TARGET_NAME: HermesMobile/TEST_TARGET_NAME: Herald/;
' project.yml
```

- [ ] **Step 4: Update version numbers and Info.plist display strings**

```bash
perl -i -pe '
  # Version: 1.0.0 / build 1
  s/MARKETING_VERSION: "1\.3\.0"/MARKETING_VERSION: "1.0.0"/g;
  s/CURRENT_PROJECT_VERSION: "5"/CURRENT_PROJECT_VERSION: "1"/g;
  s/CFBundleShortVersionString: "1\.3\.0"/CFBundleShortVersionString: "1.0.0"/g;
  s/CFBundleVersion: "4"/CFBundleVersion: "1"/g;
  # Info.plist display name and usage strings
  s/CFBundleDisplayName: Hermes$/CFBundleDisplayName: Herald/;
  s/Hermes uses your location/Herald uses your location/g;
  s/Hermes reads your health data/Herald reads your health data/g;
  s/Hermes does not write health data/Herald does not write health data/g;
  s/Hermes uses the camera/Herald uses the camera/g;
  s/Hermes accesses your photo library/Herald accesses your photo library/g;
  s/Hermes uses the microphone/Herald uses the microphone/g;
  s/Hermes uses your location in the background/Herald uses your location in the background/g;
  s/Hermes uses motion data/Herald uses motion data/g;
  s/Hermes uses speech recognition/Herald uses speech recognition/g;
' project.yml
```

- [ ] **Step 5: Verify the result looks correct**

```bash
grep -n "HermesMobile\|io\.hermesmobile\|hermes" ~/Hermes-iOS/project.yml
```

Expected: zero matches. If any remain, fix them manually before proceeding.

- [ ] **Step 6: Commit**

```bash
cd ~/Hermes-iOS
git add project.yml
git commit -m "chore(rebrand): update project.yml for HERALD identity"
```

---

## Task 2: Rename Source Directories

**Files:**
- Rename: `HermesMobile/` → `Herald/`
- Rename: `HermesMobileWidgets/` → `HeraldWidgets/`
- Rename: `HermesMobileTests/` → `HeraldTests/`
- Rename: `HermesMobileUITests/` → `HeraldUITests/`

**Interfaces:**
- Produces: source directories matching the paths in the updated `project.yml`
- Task 3 depends on these directories existing at the new paths

- [ ] **Step 1: Rename the source directories**

```bash
cd ~/Hermes-iOS
mv HermesMobile Herald
mv HermesMobileWidgets HeraldWidgets
mv HermesMobileTests HeraldTests
mv HermesMobileUITests HeraldUITests
```

- [ ] **Step 2: Rename the entitlements files inside the directories**

```bash
mv Herald/HermesMobile.entitlements Herald/Herald.entitlements
mv HeraldWidgets/HermesMobileWidgets.entitlements HeraldWidgets/HeraldWidgets.entitlements
```

- [ ] **Step 3: Verify directories exist at expected paths**

```bash
ls ~/Hermes-iOS/Herald/
ls ~/Hermes-iOS/HeraldWidgets/
ls ~/Hermes-iOS/HeraldTests/
ls ~/Hermes-iOS/HeraldUITests/
ls ~/Hermes-iOS/Herald/Herald.entitlements
ls ~/Hermes-iOS/HeraldWidgets/HeraldWidgets.entitlements
```

Expected: all paths exist with no errors.

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add -A
git commit -m "chore(rebrand): rename source directories HermesMobile→Herald"
```

---

## Task 3: Regenerate Xcode Project

**Files:**
- Delete: `HermesMobile.xcodeproj/`
- Create: `Herald.xcodeproj/` (via xcodegen)

**Interfaces:**
- Depends on: Task 1 (updated project.yml), Task 2 (renamed directories)
- Produces: `Herald.xcodeproj/` that Xcode and xcodebuild can open

- [ ] **Step 1: Run xcodegen**

```bash
cd ~/Hermes-iOS
xcodegen generate
```

Expected: output ends with `✅ Generated Herald.xcodeproj`. A new `Herald.xcodeproj/` directory appears.

- [ ] **Step 2: Remove the old project file**

```bash
rm -rf ~/Hermes-iOS/HermesMobile.xcodeproj
```

- [ ] **Step 3: Verify the new project file exists with correct scheme**

```bash
ls ~/Hermes-iOS/Herald.xcodeproj/
ls ~/Hermes-iOS/Herald.xcodeproj/xcshareddata/xcschemes/
```

Expected: `Herald.xcscheme` (and optionally `HeraldTests.xcscheme`, `HeraldUITests.xcscheme`).

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add -A
git commit -m "chore(rebrand): regenerate Herald.xcodeproj via xcodegen"
```

---

## Task 4: Rename Swift Type Files

**Files:**
- Rename: `Herald/Features/Chat/HermesAvatar.swift` → `Herald/Features/Chat/HeraldAvatar.swift`
- Rename: `Herald/Stores/HermesHostStore.swift` → `Herald/Stores/HeraldHostStore.swift`
- Rename: `Herald/Models/HermesHostStatus.swift` → `Herald/Models/HeraldHostStatus.swift`
- Rename: `Herald/Models/HermesActivityAttributes.swift` → `Herald/Models/HeraldActivityAttributes.swift`
- Rename: `Herald/Models/HermesWidgetData.swift` → `Herald/Models/HeraldWidgetData.swift`
- Rename: `Herald/Services/Protocols/HermesClientProtocol.swift` → `Herald/Services/Protocols/HeraldClientProtocol.swift`
- Rename: `Herald/Services/Protocols/HermesHostServiceProtocol.swift` → `Herald/Services/Protocols/HeraldHostServiceProtocol.swift`
- Rename: `Herald/Services/Live/LiveHermesClient.swift` → `Herald/Services/Live/LiveHeraldClient.swift`
- Rename: `Herald/Services/Live/LiveHermesHostService.swift` → `Herald/Services/Live/LiveHeraldHostService.swift`
- Rename: `Herald/Services/Mocks/MockHermesClient.swift` → `Herald/Services/Mocks/MockHeraldClient.swift`
- Rename: `Herald/Services/Mocks/MockHermesHostService.swift` → `Herald/Services/Mocks/MockHeraldHostService.swift`
- Rename: `HeraldWidgets/HermesActivityAttributes.swift` → `HeraldWidgets/HeraldActivityAttributes.swift`
- Rename: `HeraldWidgets/HermesHealthWidget.swift` → `HeraldWidgets/HeraldHealthWidget.swift`
- Rename: `HeraldWidgets/HermesLiveActivity.swift` → `HeraldWidgets/HeraldLiveActivity.swift`
- Rename: `HeraldWidgets/HermesStatusWidget.swift` → `HeraldWidgets/HeraldStatusWidget.swift`
- Rename: `HeraldWidgets/HermesTimelineProvider.swift` → `HeraldWidgets/HeraldTimelineProvider.swift`
- Rename: `HeraldWidgets/HermesWidgetBundle.swift` → `HeraldWidgets/HeraldWidgetBundle.swift`

**Interfaces:**
- Produces: Swift files at their new paths with the same content (content renames happen in Task 5)

- [ ] **Step 1: Rename the Swift source files**

```bash
cd ~/Hermes-iOS
mv Herald/Services/Support/ResilientHermesClient.swift Herald/Services/Support/ResilientHeraldClient.swift
mv Herald/Features/Chat/HermesAvatar.swift Herald/Features/Chat/HeraldAvatar.swift
mv Herald/Stores/HermesHostStore.swift Herald/Stores/HeraldHostStore.swift
mv Herald/Models/HermesHostStatus.swift Herald/Models/HeraldHostStatus.swift
mv Herald/Models/HermesActivityAttributes.swift Herald/Models/HeraldActivityAttributes.swift
mv Herald/Models/HermesWidgetData.swift Herald/Models/HeraldWidgetData.swift
mv Herald/Services/Protocols/HermesClientProtocol.swift Herald/Services/Protocols/HeraldClientProtocol.swift
mv Herald/Services/Protocols/HermesHostServiceProtocol.swift Herald/Services/Protocols/HeraldHostServiceProtocol.swift
mv Herald/Services/Live/LiveHermesClient.swift Herald/Services/Live/LiveHeraldClient.swift
mv Herald/Services/Live/LiveHermesHostService.swift Herald/Services/Live/LiveHeraldHostService.swift
mv Herald/Services/Mocks/MockHermesClient.swift Herald/Services/Mocks/MockHeraldClient.swift
mv Herald/Services/Mocks/MockHermesHostService.swift Herald/Services/Mocks/MockHeraldHostService.swift
mv HeraldWidgets/HermesActivityAttributes.swift HeraldWidgets/HeraldActivityAttributes.swift
mv HeraldWidgets/HermesHealthWidget.swift HeraldWidgets/HeraldHealthWidget.swift
mv HeraldWidgets/HermesLiveActivity.swift HeraldWidgets/HeraldLiveActivity.swift
mv HeraldWidgets/HermesStatusWidget.swift HeraldWidgets/HeraldStatusWidget.swift
mv HeraldWidgets/HermesTimelineProvider.swift HeraldWidgets/HeraldTimelineProvider.swift
mv HeraldWidgets/HermesWidgetBundle.swift HeraldWidgets/HeraldWidgetBundle.swift
```

- [ ] **Step 2: Commit**

```bash
cd ~/Hermes-iOS
git add -A
git commit -m "chore(rebrand): rename Hermes* Swift files to Herald*"
```

---

## Task 5: Replace Swift Type Names and String Literals

**Files:**
- Modify: all `.swift` files in `Herald/` and `HeraldWidgets/` and `HeraldTests/`

**Interfaces:**
- Depends on: Task 4 (files at new paths)
- Produces: compilable Swift with Herald naming throughout

- [ ] **Step 1: Rename types (Hermes* class/struct/protocol/enum identifiers)**

```bash
cd ~/Hermes-iOS
# Replace type names — structural identifiers, not user strings
find Herald HeraldWidgets HeraldTests HeraldUITests -name "*.swift" | xargs perl -i -pe '
  s/\bHermesAvatar\b/HeraldAvatar/g;
  s/\bHermesHostStore\b/HeraldHostStore/g;
  s/\bHermesHostStatus\b/HeraldHostStatus/g;
  s/\bHermesActivityAttributes\b/HeraldActivityAttributes/g;
  s/\bHermesWidgetData\b/HeraldWidgetData/g;
  s/\bHermesClientProtocol\b/HeraldClientProtocol/g;
  s/\bHermesHostServiceProtocol\b/HeraldHostServiceProtocol/g;
  s/\bLiveHermesClient\b/LiveHeraldClient/g;
  s/\bLiveHermesHostService\b/LiveHeraldHostService/g;
  s/\bResilientHermesClient\b/ResilientHeraldClient/g;
  s/\bMockHermesClient\b/MockHeraldClient/g;
  s/\bMockHermesHostService\b/MockHeraldHostService/g;
'
```

- [ ] **Step 2: Replace Logger subsystem strings**

```bash
find Herald HeraldWidgets -name "*.swift" | xargs perl -i -pe '
  s|io\.hermesmobile\.HermesMobile|net.fihonline.herald|g;
  s|io\.hermesmobile|net.fihonline.herald|g;
'
```

- [ ] **Step 3: Replace user-visible "Hermes" string literals**

These are display names shown to users — not internal variable names:

```bash
find Herald HeraldWidgets -name "*.swift" | xargs perl -i -pe '
  # String literals that users see
  s/"Hermes"/"Herald"/g;
  # Conversation default title (in ChatStore, MockHermesClient, LiveHeraldClient)
  # already covered by the line above
  # VoiceState display name
  # already covered
'
```

- [ ] **Step 4: Update CarPlay module reference**

The `project.yml` `UISceneDelegateClassName` uses `$(PRODUCT_MODULE_NAME).CarPlaySceneDelegate` (a build variable) so it stays correct automatically. Verify no hardcoded module name remains:

```bash
grep -rn "HermesMobile\.CarPlaySceneDelegate\|HermesMobile\.AppEntry" Herald/ HeraldWidgets/
```

Expected: zero results. If any exist, replace `HermesMobile.` with `Herald.`.

- [ ] **Step 5: Check for any remaining Hermes identifiers**

```bash
grep -rn "\bHermes[A-Z]" Herald/ HeraldWidgets/ HeraldTests/ --include="*.swift" | grep -v "// "
```

Expected: zero results. Fix any that appear.

- [ ] **Step 6: Commit**

```bash
cd ~/Hermes-iOS
git add -A
git commit -m "chore(rebrand): replace Hermes type names and string literals with Herald"
```

---

## Task 6: Rename Theme Preset

**Files:**
- Modify: `Herald/Core/Theme.swift`
- Modify: `Herald/Core/ThemeManager.swift`

**Interfaces:**
- `ThemePreset.nous` → `ThemePreset.herald`
- `ThemeManager.defaultPreset` returns `.herald`

- [ ] **Step 1: Rename .nous case and its label in Theme.swift**

Open `Herald/Core/Theme.swift`. Find the `ThemePreset` enum. Change:

```swift
// Before:
enum ThemePreset: String, Codable, CaseIterable, Identifiable {
    case nous, midnight, ember, mono, cyberpunk, slate
    ...
    case .nous: "Nous"
    ...
    case .nous: Color(hex: 0x4A9EFF)
    ...
    case .nous:
        // color block
```

To:

```swift
enum ThemePreset: String, Codable, CaseIterable, Identifiable {
    case herald, midnight, ember, mono, cyberpunk, slate
    ...
    case .herald: "Herald"
    ...
    case .herald: Color(hex: 0xFF6B00)  // molten orange brand color
    ...
    case .herald:
        // same color block as .nous had
```

Use the in-editor rename — or run:

```bash
perl -i -pe '
  s/\bcase nous\b/case herald/g;
  s/case \.nous:/case .herald:/g;
  s/"Nous"/"Herald"/g;
' Herald/Core/Theme.swift
```

Then manually update the `.herald` accent color from `0x4A9EFF` to `0xFF6B00` (molten orange) in the `accentColor` switch.

- [ ] **Step 2: Update ThemeManager default**

```bash
perl -i -pe 's/var preset: ThemePreset = \.nous/var preset: ThemePreset = .herald/' Herald/Core/ThemeManager.swift
```

- [ ] **Step 3: Handle Codable migration for existing .nous UserDefaults values**

In `ThemeManager.swift`, add a migration guard so devices that had `.nous` stored in UserDefaults decode it as `.herald`. Find the `@AppStorage` or `UserDefaults` decode site and add:

```swift
// Migration: .nous was renamed to .herald in HERALD 1.0.0
if storedRawValue == "nous" {
    storedRawValue = "herald"
}
```

Place this immediately before the `ThemePreset(rawValue: storedRawValue)` call. The exact location depends on how `ThemeManager` reads its stored preset — find the decode/init site.

- [ ] **Step 4: Verify no .nous references remain**

```bash
grep -rn "\.nous\b\|\"nous\"" Herald/ HeraldWidgets/ --include="*.swift"
```

Expected: zero results.

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add Herald/Core/Theme.swift Herald/Core/ThemeManager.swift
git commit -m "chore(rebrand): rename ThemePreset.nous → .herald, set brand orange accent"
```

---

## Task 7: Update Relay

**Files:**
- Modify: `relay/docker-compose.yml`
- Modify: `relay/app/hermes_adapter.py`
- Modify: `relay/.env.example` (if it exists)

**Interfaces:**
- Container name changes: `hermes-relay` → `herald-relay`
- Bundle ID in env: `net.fihonline.herald`

- [ ] **Step 1: Update docker-compose.yml container/service names**

```bash
perl -i -pe '
  s/hermes-relay/herald-relay/g;
  s/hermes_mobile/herald/g;
' relay/docker-compose.yml
```

Verify the result:
```bash
cat relay/docker-compose.yml
```

- [ ] **Step 2: Update .env.example if it exists**

```bash
if [ -f relay/.env.example ]; then
  perl -i -pe '
    s/com\.herald-dev\.HermesMobileApp/net.fihonline.herald/g;
    s/com\.herald-dev\.Hermes[A-Za-z]*/net.fihonline.herald/g;
    s/APNS_BUNDLE_ID=.*/APNS_BUNDLE_ID=net.fihonline.herald/;
  ' relay/.env.example
fi
```

- [ ] **Step 3: Update user-facing strings in hermes_adapter.py**

```bash
# Update display strings only — keep function/class names functional
perl -i -pe '
  s/"Hermes Mobile"/"Herald"/g;
  s/"Hermes"/"Herald"/g;
' relay/app/hermes_adapter.py
```

- [ ] **Step 4: Verify relay tests still pass**

```bash
cd ~/Hermes-iOS/relay && .venv/bin/python -m pytest tests/ -q
```

Expected: all tests pass (same count as before this task).

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add relay/
git commit -m "chore(rebrand): rename relay container and update bundle ID to Herald"
```

---

## Task 8: Update Connector

**Files:**
- Modify: `connector/pyproject.toml`
- Rename: `connector/src/hermes_mobile_connector/` → `connector/src/herald_connector/`
- Modify: all `.py` files in `connector/src/herald_connector/`

**Interfaces:**
- Package name: `hermes-mobile-connector` → `herald-connector`
- Module: `hermes_mobile_connector` → `herald_connector`
- CLI entry point: `hermes-mobile` → `herald`, `hermes-mobile-mcp` → `herald-mcp`

- [ ] **Step 1: Rename the source package directory**

```bash
cd ~/Hermes-iOS/connector/src
mv hermes_mobile_connector herald_connector
```

- [ ] **Step 2: Update pyproject.toml**

```bash
perl -i -pe '
  s/name = "hermes-mobile-connector"/name = "herald-connector"/;
  s/hermes_mobile_connector\.cli:main/herald_connector.cli:main/g;
  s/hermes_mobile_connector\.mcp_server:main/herald_connector.mcp_server:main/g;
  s/\[tool\.setuptools\.packages\.find\]/[tool.setuptools.packages.find]/;
  s/include = \["hermes_mobile_connector\*"\]/include = ["herald_connector*"]/;
  s/"hermes-mobile"/"herald"/;
  s/"hermes-mobile-mcp"/"herald-mcp"/;
' connector/pyproject.toml
```

- [ ] **Step 3: Update import statements in all connector Python files**

```bash
find connector/src/herald_connector -name "*.py" | xargs perl -i -pe '
  s/from hermes_mobile_connector/from herald_connector/g;
  s/import hermes_mobile_connector/import herald_connector/g;
'
# Also update connector tests if any
find connector/tests -name "*.py" 2>/dev/null | xargs perl -i -pe '
  s/from hermes_mobile_connector/from herald_connector/g;
  s/import hermes_mobile_connector/import herald_connector/g;
'
```

- [ ] **Step 4: Update user-facing display strings in connector**

```bash
find connector/src/herald_connector -name "*.py" | xargs perl -i -pe '
  s/"Hermes Mobile"/"Herald"/g;
  s/"Hermes"/"Herald"/g;
' 
```

- [ ] **Step 5: Update __init__.py package name if present**

```bash
grep -rn "hermes_mobile_connector\|hermes-mobile" connector/src/herald_connector/ | head -20
```

Fix any remaining references manually.

- [ ] **Step 6: Reinstall the connector in development mode to verify**

```bash
cd ~/Hermes-iOS/connector
.venv/bin/pip install -e . -q
.venv/bin/herald --help 2>/dev/null || .venv/bin/python -c "from herald_connector import cli; print('OK')"
```

Expected: help text or "OK" with no ImportError.

- [ ] **Step 7: Commit**

```bash
cd ~/Hermes-iOS
git add connector/
git commit -m "chore(rebrand): rename connector package hermes_mobile_connector→herald_connector"
```

---

## Task 9: Place Brand Assets

**Files:**
- Create: `Herald/Resources/AppIcon.png` (1024×1024)
- Create: `docs/assets/brand-mark.svg`
- Create: `docs/assets/brand-mark-vertical.svg`
- Create: `docs/assets/app-icon.svg`
- Create: `docs/assets/architecture.svg`
- Create: `docs/social-preview.png` (placeholder — see note)
- Create: `docs/screenshots/` directory (empty — populated from device screenshots separately)

**Interfaces:**
- Produces: brand assets usable in README and Xcode asset catalog

- [ ] **Step 1: Resize app-icon.png to 1024×1024 and place it**

```bash
mkdir -p ~/Hermes-iOS/Herald/Resources
sips -z 1024 1024 ~/Desktop/herald-rebrand/app-icon.png --out ~/Hermes-iOS/Herald/Resources/AppIcon.png
```

Verify:
```bash
sips -g pixelWidth -g pixelHeight ~/Hermes-iOS/Herald/Resources/AppIcon.png
```

Expected: `pixelWidth: 1024`, `pixelHeight: 1024`.

- [ ] **Step 2: Copy brand-mark.png for reference**

```bash
mkdir -p ~/Hermes-iOS/docs/assets
cp ~/Desktop/herald-rebrand/brand-mark.png ~/Hermes-iOS/docs/assets/brand-mark-source.png
```

- [ ] **Step 3: Create brand-mark.svg (landscape)**

Create `docs/assets/brand-mark.svg` with this content:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 600 120" width="600" height="120">
  <!-- Flame mark -->
  <g transform="translate(20, 8)">
    <!-- Outer flame body -->
    <path d="M32 100 C10 80 0 55 12 35 C18 25 24 38 26 45 C28 30 35 10 50 0 C45 20 48 30 52 38 C56 20 60 28 62 40 C68 28 72 40 70 58 C68 75 60 88 50 100 Z"
          fill="#FF6B00"/>
    <!-- Inner hot tip -->
    <path d="M50 100 C38 85 32 68 38 52 C42 42 46 50 48 58 C50 48 54 38 58 48 C62 60 60 78 50 100 Z"
          fill="#FFF5E0"/>
    <!-- Ember shadow accent -->
    <path d="M32 100 C14 82 8 60 16 40 C20 30 24 40 26 48 L28 42 C30 28 36 14 50 0 C42 22 44 34 46 44 L44 36 C40 18 50 4 50 4 C46 24 50 36 52 42 L50 34 C54 22 60 30 62 42 C64 30 68 38 70 58 C68 76 60 90 50 100 Z"
          fill="#FF3D00" opacity="0.4"/>
  </g>
  <!-- HERALD text -->
  <text x="120" y="78"
        font-family="'SF Pro Display', 'Helvetica Neue', Arial, sans-serif"
        font-size="56"
        font-weight="700"
        letter-spacing="8"
        fill="#F5F0E8">HERALD</text>
</svg>
```

- [ ] **Step 4: Create brand-mark-vertical.svg (stacked)**

Create `docs/assets/brand-mark-vertical.svg`:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 240" width="200" height="240">
  <!-- Flame centered -->
  <g transform="translate(75, 10) scale(1.0)">
    <path d="M32 100 C10 80 0 55 12 35 C18 25 24 38 26 45 C28 30 35 10 50 0 C45 20 48 30 52 38 C56 20 60 28 62 40 C68 28 72 40 70 58 C68 75 60 88 50 100 Z"
          fill="#FF6B00"/>
    <path d="M50 100 C38 85 32 68 38 52 C42 42 46 50 48 58 C50 48 54 38 58 48 C62 60 60 78 50 100 Z"
          fill="#FFF5E0"/>
  </g>
  <!-- HERALD text centered -->
  <text x="100" y="200"
        font-family="'SF Pro Display', 'Helvetica Neue', Arial, sans-serif"
        font-size="28"
        font-weight="700"
        letter-spacing="6"
        text-anchor="middle"
        fill="#F5F0E8">HERALD</text>
</svg>
```

- [ ] **Step 5: Create app-icon.svg (flame only)**

Create `docs/assets/app-icon.svg`:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <rect width="512" height="512" fill="#0A0A0A"/>
  <g transform="translate(152, 60) scale(4.1)">
    <path d="M32 100 C10 80 0 55 12 35 C18 25 24 38 26 45 C28 30 35 10 50 0 C45 20 48 30 52 38 C56 20 60 28 62 40 C68 28 72 40 70 58 C68 75 60 88 50 100 Z"
          fill="#FF6B00"/>
    <path d="M50 100 C38 85 32 68 38 52 C42 42 46 50 48 58 C50 48 54 38 58 48 C62 60 60 78 50 100 Z"
          fill="#FFF5E0"/>
  </g>
</svg>
```

- [ ] **Step 6: Create architecture.svg**

Create `docs/assets/architecture.svg`:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 200" width="800" height="200">
  <rect width="800" height="200" fill="#0A0A0A"/>
  <!-- Nodes -->
  <!-- iPhone/iPad -->
  <rect x="20" y="70" width="140" height="60" rx="8" fill="#1A1D23" stroke="#FF6B00" stroke-width="1.5"/>
  <text x="90" y="96" font-family="'SF Mono', 'Courier New', monospace" font-size="13" fill="#F5F0E8" text-anchor="middle">iPhone / iPad</text>
  <text x="90" y="116" font-family="'SF Mono', 'Courier New', monospace" font-size="11" fill="#FF6B00" text-anchor="middle">HERALD app</text>
  <!-- Relay -->
  <rect x="230" y="70" width="140" height="60" rx="8" fill="#1A1D23" stroke="#FF6B00" stroke-width="1.5"/>
  <text x="300" y="96" font-family="'SF Mono', 'Courier New', monospace" font-size="13" fill="#F5F0E8" text-anchor="middle">Relay</text>
  <text x="300" y="116" font-family="'SF Mono', 'Courier New', monospace" font-size="11" fill="#FF6B00" text-anchor="middle">herald-relay</text>
  <!-- Connector -->
  <rect x="440" y="70" width="140" height="60" rx="8" fill="#1A1D23" stroke="#FF6B00" stroke-width="1.5"/>
  <text x="510" y="96" font-family="'SF Mono', 'Courier New', monospace" font-size="13" fill="#F5F0E8" text-anchor="middle">Connector</text>
  <text x="510" y="116" font-family="'SF Mono', 'Courier New', monospace" font-size="11" fill="#FF6B00" text-anchor="middle">herald-connector</text>
  <!-- Runtime -->
  <rect x="650" y="70" width="130" height="60" rx="8" fill="#1A1D23" stroke="#FF6B00" stroke-width="1.5"/>
  <text x="715" y="96" font-family="'SF Mono', 'Courier New', monospace" font-size="13" fill="#F5F0E8" text-anchor="middle">AI Runtime</text>
  <text x="715" y="116" font-family="'SF Mono', 'Courier New', monospace" font-size="11" fill="#FF6B00" text-anchor="middle">Hermes / Ollama</text>
  <!-- Arrows -->
  <line x1="160" y1="100" x2="230" y2="100" stroke="#FF6B00" stroke-width="2" marker-end="url(#arrow)"/>
  <line x1="370" y1="100" x2="440" y2="100" stroke="#FF6B00" stroke-width="2" marker-end="url(#arrow)"/>
  <line x1="580" y1="100" x2="650" y2="100" stroke="#FF6B00" stroke-width="2" marker-end="url(#arrow)"/>
  <!-- Labels -->
  <text x="195" y="90" font-family="'SF Mono', 'Courier New', monospace" font-size="9" fill="#F5F0E8" text-anchor="middle">HTTPS/SSE</text>
  <text x="405" y="90" font-family="'SF Mono', 'Courier New', monospace" font-size="9" fill="#F5F0E8" text-anchor="middle">WebSocket</text>
  <text x="615" y="90" font-family="'SF Mono', 'Courier New', monospace" font-size="9" fill="#F5F0E8" text-anchor="middle">MCP / stdio</text>
  <!-- Arrow marker -->
  <defs>
    <marker id="arrow" markerWidth="8" markerHeight="8" refX="6" refY="3" orient="auto">
      <path d="M0,0 L0,6 L8,3 z" fill="#FF6B00"/>
    </marker>
  </defs>
</svg>
```

- [ ] **Step 7: Create docs/social-preview.png (GitHub social card)**

GitHub uses 1280×640 for social cards. Create it by centering the brand mark on a black background:

```bash
# Create a 1280x640 black canvas with brand-mark centered
# Requires ImageMagick (brew install imagemagick) — fall back to manual if unavailable
if command -v magick &>/dev/null; then
  magick -size 1280x640 xc:'#0A0A0A' \
    ~/Hermes-iOS/docs/assets/brand-mark-source.png \
    -gravity Center -geometry +0-40 -composite \
    ~/Hermes-iOS/docs/social-preview.png
else
  echo "ImageMagick not found — create docs/social-preview.png manually (1280x640, #0A0A0A bg, brand mark centered)"
fi
```

If ImageMagick is unavailable, create `docs/social-preview.png` manually using any image editor: 1280×640px, `#0A0A0A` background, brand-mark.png centered.

- [ ] **Step 8: Create docs/screenshots placeholder directory**

```bash
mkdir -p ~/Hermes-iOS/docs/screenshots
touch ~/Hermes-iOS/docs/screenshots/.gitkeep
```

Note: Actual screenshots (`iphone-chat.png`, `ipad-sidebar.png`, `voice-mode.png`, `settings.png`, `pairing.png`) are populated from device separately. The README references them but they can be added post-launch.

- [ ] **Step 9: Update Xcode asset catalog to reference AppIcon.png**

Open `Herald/Resources/Assets.xcassets/AppIcon.appiconset/Contents.json` and verify the 1024×1024 entry points to `AppIcon.png`:

```bash
cat Herald/Resources/Assets.xcassets/AppIcon.appiconset/Contents.json | grep -A2 "1024"
```

If the filename field is different from `AppIcon.png`, update the JSON to match the file you placed. The simplest fix is to name the placed file to match whatever name is already in Contents.json.

- [ ] **Step 10: Commit**

```bash
cd ~/Hermes-iOS
git add docs/assets/ docs/screenshots/ docs/social-preview.png Herald/Resources/AppIcon.png
git commit -m "chore(rebrand): add Herald brand assets (SVGs, app icon, architecture diagram, social preview)"
```

---

## Task 10: Rewrite README.md

**Files:**
- Replace: `README.md`

**Interfaces:**
- Produces: professional README matching the spec in `rebrand-instructions.md` §6.1
- References brand assets at `docs/assets/` and screenshots at `docs/screenshots/`

- [ ] **Step 1: Write the new README.md**

Replace the entire file with:

```markdown
<!-- HERALD -->
<p align="center">
  <img src="docs/assets/brand-mark.svg" alt="HERALD" height="80"/>
  <br/>
  <sub>Self-hosted AI companion for iPhone and iPad</sub>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-1.0.0-FF6B00" alt="version"/>
  <img src="https://img.shields.io/badge/license-MIT-F5F0E8" alt="license"/>
  <img src="https://img.shields.io/badge/platform-iOS%2026+-0A0A0A?labelColor=0A0A0A&color=FF6B00" alt="platform"/>
  <img src="https://img.shields.io/badge/self--hosted-yes-FF3D00" alt="self-hosted"/>
  <img src="https://img.shields.io/badge/relay-active-FF6B00" alt="relay"/>
</p>

<p align="center">
  <img src="docs/assets/app-icon.svg" alt="HERALD icon" width="96" style="border-radius:20px"/>
  &nbsp;&nbsp;&nbsp;
  <span>HERALD is a native iOS companion for self-hosted AI runtimes. It adds voice mode, sensors, CarPlay, session management, and a relay so your AI moves between your phone, tablet, and desktop without becoming a hosted service.</span>
</p>

---

<p align="center">
  <img src="docs/screenshots/iphone-chat.png" alt="iPhone chat" width="30%" style="border-radius:12px;border:1px solid #1A1D23"/>
  &nbsp;
  <img src="docs/screenshots/ipad-sidebar.png" alt="iPad sidebar" width="30%" style="border-radius:12px;border:1px solid #1A1D23"/>
  &nbsp;
  <img src="docs/screenshots/voice-mode.png" alt="Voice mode" width="30%" style="border-radius:12px;border:1px solid #1A1D23"/>
</p>

---

## Features

| Feature | Description |
|---------|-------------|
| **Streaming Chat** | Real-time streaming with markdown, code blocks, inline diffs, and attachments |
| **Voice Mode** | OpenAI Realtime voice with live camera context and tool delegation |
| **iPad Native** | Full NavigationSplitView layout with session browser sidebar |
| **Session Management** | Pin, archive, rename, search. Device-scoped sessions. |
| **Model Switching** | Switch models on the fly via direct RPC |
| **Sensors** | Health, location, motion data piped to your AI in real-time |
| **CarPlay** | Hands-free AI from your dashboard |
| **Themes** | 6 built-in presets with custom wallpaper support |
| **Cron Jobs** | Schedule recurring AI tasks from your phone |
| **Skills Browser** | Browse and manage installed agent skills |

---

## Architecture

<p align="center">
  <img src="docs/assets/architecture.svg" alt="HERALD architecture" width="100%"/>
</p>

---

## Quick Start

1. **Deploy the relay**
   ```bash
   cd relay
   docker compose up -d
   ```

2. **Install the connector**
   ```bash
   pip install herald-connector
   herald start
   ```

3. **Install HERALD on your iPhone**
   - Build from source (see [Building from Source](#building-from-source)) or download the latest release
   - Open the app, scan the pairing QR code
   - Start chatting with your AI

---

## Building from Source

**Prerequisites:** Xcode 26+, macOS 26+, Apple Developer account

```bash
git clone https://github.com/fireishott/Herald.git
cd Herald
xcodegen generate
open Herald.xcodeproj
```

See [docs/BUILDING.md](docs/BUILDING.md) for signing, entitlements, and device install instructions.

---

## Relay Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `RELAY_ENVIRONMENT` | `development` | `production` or `development` |
| `PUBLIC_BASE_URL` | `http://localhost:8000/v1` | Public relay URL |
| `APNS_KEY_ID` | — | APNs key ID for push notifications |
| `APNS_TEAM_ID` | — | Apple Developer team ID |
| `APNS_BUNDLE_ID` | `net.fihonline.herald` | App bundle ID |

See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for the full reference.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

## Acknowledgements

Built on the foundation of [Hermes-iOS](https://github.com/dylan-buck/Hermes-iOS) by [Dylan Buck](https://github.com/dylan-buck) and the [Nous Research](https://nousresearch.com/) community. Original work licensed under MIT.

---

## License

[MIT](LICENSE)
```

- [ ] **Step 2: Commit**

```bash
cd ~/Hermes-iOS
git add README.md
git commit -m "docs(rebrand): rewrite README for HERALD brand"
```

---

## Task 11: Update Supporting Docs and License

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CONTRIBUTING.md`
- Modify: `SECURITY.md`
- Modify: `MAINTAINER_NOTES.md`
- Modify: `LICENSE`
- Create: `docs/BUILDING.md`
- Create: `docs/CONFIGURATION.md`

**Interfaces:**
- Produces: complete documentation set with Herald naming

- [ ] **Step 1: Add CHANGELOG entry at top**

Open `CHANGELOG.md`. Add before the first existing entry:

```markdown
## [1.0.0] - 2026-07-19

### Changed
- Rebrand from Hermes-iOS to HERALD
- New bundle ID: `net.fihonline.herald`
- New relay container: `herald-relay`
- New connector package: `herald-connector`
- Theme preset renamed: `.nous` → `.herald` with brand orange (#FF6B00) accent
- Version reset to 1.0.0 to mark the new identity

```

- [ ] **Step 2: Update name references in CONTRIBUTING.md, SECURITY.md, MAINTAINER_NOTES.md**

```bash
for f in CONTRIBUTING.md SECURITY.md MAINTAINER_NOTES.md; do
  perl -i -pe '
    s/Hermes-iOS/HERALD/g;
    s/HermesMobile/Herald/g;
    s/hermes-ios/herald-ios/g;
    s/fireishott\/Hermes-iOS/fireishott\/Herald/g;
  ' ~/Hermes-iOS/$f
done
```

- [ ] **Step 3: Update LICENSE**

Open `LICENSE`. The current copyright line reads something like:

```
MIT License

Copyright (c) 2026 Hermes iOS Contributors
```

Change to:

```
MIT License

Copyright (c) 2026 Herald Contributors
Copyright (c) 2026 Original Hermes iOS Contributors
```

- [ ] **Step 4: Create docs/BUILDING.md**

```bash
cat > ~/Hermes-iOS/docs/BUILDING.md << 'EOF'
# Building HERALD from Source

## Prerequisites

- Xcode 26 or later
- macOS 26 or later
- Apple Developer account (free account works for device-limited builds)
- [XcodeGen](https://github.com/yonaskolb/XcodeGen): `brew install xcodegen`

## Steps

1. Clone the repo:
   ```bash
   git clone https://github.com/fireishott/Herald.git
   cd Herald
   ```

2. Generate the Xcode project:
   ```bash
   xcodegen generate
   ```

3. Open in Xcode:
   ```bash
   open Herald.xcodeproj
   ```

4. Select the `Herald` scheme and your target device.

5. In Xcode → Signing & Capabilities, set your development team.

6. Build and run (⌘R).

## Device Install via CLI

```bash
# Unlock keychain first
security unlock-keychain -p '<password>' ~/Library/Keychains/login.keychain-db

# Build
xcodebuild \
  -project Herald.xcodeproj \
  -scheme Herald \
  -configuration Debug \
  -destination 'generic/platform=iOS' \
  build \
  -allowProvisioningUpdates \
  DEVELOPMENT_TEAM=58U7UPFS53

# Install
xcrun devicectl device install app \
  --device <device-udid> \
  <path-to-Herald.app>
```

## Entitlements Note

The repo ships with full entitlements (HealthKit, push, app groups). If you're building with a free Apple ID (no paid team), strip the entitlements file before building:

```bash
echo '<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict></dict></plist>' > Herald/Herald.entitlements
```

Restore it after building:
```bash
git checkout Herald/Herald.entitlements
```
EOF
```

- [ ] **Step 5: Create docs/CONFIGURATION.md**

```bash
cat > ~/Hermes-iOS/docs/CONFIGURATION.md << 'EOF'
# HERALD Configuration Reference

## Relay Environment Variables

Set these in `relay/.env` (copy from `relay/.env.example`):

| Variable | Default | Description |
|----------|---------|-------------|
| `RELAY_ENVIRONMENT` | `development` | `production` or `development` |
| `DATABASE_URL` | `sqlite:////data/relay.db` | Database connection string |
| `PUBLIC_BASE_URL` | `http://localhost:8000/v1` | Public relay URL (used in push payloads) |
| `SECRET_KEY` | — | Random secret for session tokens |
| `APNS_KEY_ID` | — | APNs key ID (10-char string from Apple Developer) |
| `APNS_TEAM_ID` | — | Apple Developer Team ID |
| `APNS_BUNDLE_ID` | `net.fihonline.herald` | App bundle identifier |
| `APNS_KEY_PATH` | — | Path to APNs `.p8` private key file |
| `PUSH_BROKER_URL` | — | Optional: managed push broker URL |

## Connector Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `HERMES_HOME` | `~/.hermes` | Path to Hermes home directory |
| `RELAY_URL` | `http://localhost:8000/v1` | Relay base URL |
| `CONNECTOR_CREDENTIAL` | — | Bearer token from relay pairing |

## Docker Compose (Quick Deploy)

```bash
cd relay
cp .env.example .env
# Edit .env with your values
docker compose up -d
```

The relay binds to port 8000 by default.

## Reverse Proxy (Production)

Place a reverse proxy (nginx, Caddy, Traefik) in front of the relay for TLS termination. Set `PUBLIC_BASE_URL` to your public HTTPS URL.
EOF
```

- [ ] **Step 6: Commit all**

```bash
cd ~/Hermes-iOS
git add CHANGELOG.md CONTRIBUTING.md SECURITY.md MAINTAINER_NOTES.md LICENSE docs/BUILDING.md docs/CONFIGURATION.md
git commit -m "docs(rebrand): update changelog, license, contributing, and add BUILDING/CONFIGURATION docs"
```

---

## Task 12: Build Verification

**Files:**
- No changes — verification only

- [ ] **Step 1: Unlock keychain and run xcodebuild**

```bash
security unlock-keychain -p '<password>' ~/Library/Keychains/login.keychain-db

# Strip entitlements for the build check
cp Herald/Herald.entitlements Herald/Herald.entitlements.bak
echo '<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict></dict></plist>' > Herald/Herald.entitlements

xcodebuild \
  -project Herald.xcodeproj \
  -scheme Herald \
  -configuration Debug \
  -destination 'generic/platform=iOS' \
  build \
  -allowProvisioningUpdates \
  DEVELOPMENT_TEAM=58U7UPFS53 \
  2>&1 | tail -20

# Restore entitlements
cp Herald/Herald.entitlements.bak Herald/Herald.entitlements
rm Herald/Herald.entitlements.bak
```

Expected: `** BUILD SUCCEEDED **`

- [ ] **Step 2: Verify relay tests pass**

```bash
cd ~/Hermes-iOS/relay && .venv/bin/python -m pytest tests/ -q
```

Expected: all tests pass.

- [ ] **Step 3: Verify connector imports**

```bash
cd ~/Hermes-iOS/connector && .venv/bin/python -c "from herald_connector import cli; print('OK')"
```

Expected: `OK`

- [ ] **Step 4: Verify no remaining HermesMobile references in Swift source**

```bash
grep -rn "HermesMobile\|io\.hermesmobile" ~/Hermes-iOS/Herald/ ~/Hermes-iOS/HeraldWidgets/ --include="*.swift" | grep -v "//"
```

Expected: zero results.

- [ ] **Step 5: Final commit**

```bash
cd ~/Hermes-iOS
git commit --allow-empty -m "chore(rebrand): verify HERALD build and test suite"
```

---

## Task 13: Push and GitHub Setup (Manual)

These steps require a browser and GitHub account action — not automatable via CLI alone.

- [ ] **Step 1: Add upstream remote if not present**

```bash
cd ~/Hermes-iOS
git remote get-url upstream 2>/dev/null || git remote add upstream https://github.com/dylan-buck/Hermes-iOS.git
git remote -v
```

Expected: both `origin` (fireishott/Herald) and `upstream` (dylan-buck/Hermes-iOS) listed.

- [ ] **Step 2: Push to origin**

```bash
cd ~/Hermes-iOS
git push origin master
```

- [ ] **Step 3: GitHub repo rename (manual)**

In the browser, go to `https://github.com/fireishott/Hermes-iOS` → Settings → Repository name → change to `Herald` → Rename.

- [ ] **Step 4: Update local remote URL**

```bash
git remote set-url origin https://github.com/fireishott/Herald.git
git push origin master
```

- [ ] **Step 5: Update GitHub repo description and topics (manual)**

In browser → About section (top right of repo page):
- Description: `Self-hosted AI companion for iPhone and iPad. Native iOS client with relay, voice mode, sensors, and CarPlay.`
- Topics: `herald ai-companion self-hosted ios ipad carplay voice-mode sensors swift python mcp`

- [ ] **Step 6: Final verification checklist**

```
- [ ] App builds as Herald.xcodeproj with Herald scheme
- [ ] Bundle ID is net.fihonline.herald
- [ ] No "Hermes" text visible in app UI (settings, about, errors)
- [ ] Relay container named herald-relay
- [ ] Connector imports as herald_connector
- [ ] README loads on GitHub with brand mark
- [ ] LICENSE shows both copyright lines
- [ ] Fork relationship to upstream preserved
```
