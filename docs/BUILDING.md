# Building Kallisti for iOS from source

The app lives on the `restore/kallisti-identity-on-eris` branch of this
repository, beside the Rust daemon. The daemon is not needed to compile the app,
but the app is not useful without one to pair with.

## Prerequisites

- Xcode 26 or newer, on macOS 26 or newer.
- An Apple ID for Simulator builds; an Apple Developer team for a device build.
- No XcodeGen, no CocoaPods, no package manager. `Herald.xcodeproj` is committed
  and builds as it sits.

## Build

```bash
git clone --branch restore/kallisti-identity-on-eris \
  https://github.com/fireishott/Kallisti.git
cd Kallisti
open Herald.xcodeproj
```

Then pick the **Kallisti** scheme, pick your team under Signing & Capabilities,
and run to a Simulator or a registered device (⌘R).

From the command line, with no signing, to check that it compiles:

```bash
xcodebuild -project Herald.xcodeproj -scheme Kallisti \
  -destination 'generic/platform=iOS' -configuration Debug \
  build CODE_SIGNING_ALLOWED=NO
```

## Signing

`Herald.xcodeproj` ships with the maintainer's team (`58U7UPFS53`) in its build
settings and with automatic signing. Xcode replaces it with yours the moment you
select a team in Signing & Capabilities, and a Simulator build does not need one
at all. App Store uploads additionally need your own provisioning profiles for
the app and its four extensions; a local device build does not.

## Configuration

`Config/DSH.example.xcconfig` documents the optional build settings (the DSH
transport's base URL and token). Copy it to `Config/DSH.local.xcconfig` only if
you build with `-xcconfig`; Xcode builds without it.

## What the app needs to run

A reachable Eris daemon and a pairing code. Run the daemon (see the repository
README), open its dashboard, mint a pairing code under Devices, and redeem it in
the app. Nothing in the app works before pairing, by design: there is no relay
and no cloud account.

## If it does not build

- Wrong branch: `main` is the daemon only and has no Swift sources.
- Xcode too old: the project uses Swift 6 language mode.
- A device build failing on signing is a team or profile problem, not a source
  problem; a Simulator build proves the source compiles.
