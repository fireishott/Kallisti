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
