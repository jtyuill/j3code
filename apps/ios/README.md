# j3 for iOS

A SwiftUI app over the Rust core in `crates/j3-mobile` (UniFFI bindings over
`j3-client` and `j3-text`; `docs/m8.md` D1, D2b, P4). Swift owns
navigation, lists, sheets, the composer and system integration; Rust owns the
connection, protocol v2, state, and markdown/math preparation.

- Bundle id `dev.jacobyuill.j3code`, team `8NY34B669X`, iOS 18 or later,
  iPhone and iPad (`NavigationSplitView`).
- URL scheme `j3code`. `j3code://connections/new?host=<address>&autoConnect=1`
  adds a server and connects at once; without `autoConnect` it opens the add
  form prefilled. `name=<name>` sets its display name.
- Hardware keyboard: ⌘N new thread, ⌘, settings, ⇧⌘K connections, ⌘. stop,
  Return (or ⌘Return, see Settings) sends, ↑ ↓ Tab Esc in completions.

## Layout

```
apps/ios/project.yml       XcodeGen spec (the .xcodeproj is generated, not committed)
apps/ios/J3Code/           the app: App, Hosts, Threads, Thread, Feed, Composer, Settings, Support
apps/ios/J3CodeTests/      XCTest for view-model logic
apps/ios/J3CodeUITests/    the simulator smoke test (and a read-only browse test)
contrib/ios/               build-xcframework.sh, smoke.sh
target/ios/                build output: J3MobileFFI.xcframework, swift/j3_mobile.swift
```

## Requirements

macOS with Xcode (27 used), [XcodeGen](https://github.com/yonaskolb/XcodeGen)
and rustup. The scripts use rustup's toolchain even when a Homebrew `rust` is
earlier on `PATH`, and add the `aarch64-apple-ios` and
`aarch64-apple-ios-sim` targets on first use.

## Build

```sh
# The Rust library for device and simulator, the Swift bindings, the
# XCFramework. Skipped when nothing it is built from changed; --force rebuilds.
contrib/ios/build-xcframework.sh

cd apps/ios
xcodegen generate

# Simulator build.
xcodebuild -project J3Code.xcodeproj -scheme J3Code \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath build/dd build

# Open in Xcode instead.
open J3Code.xcodeproj
```

Run `build-xcframework.sh` again after changing anything under `crates/` that
`j3-mobile` uses; the Xcode project does not run cargo itself.

## Tests

```sh
# View-model logic (host store, deep links, thread grouping, request answers,
# composer tokens).
cd apps/ios
xcodebuild -project J3Code.xcodeproj -scheme J3Code \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath build/dd \
  test -only-testing:J3CodeTests

# Rust side (any OS).
cargo test -p j3-mobile

# Smoke test: builds everything, starts j3-test-server (a j3 server whose omp
# is a script), boots a simulator and runs the UI test that connects through
# the deep link, opens the thread, sends a prompt, answers omp's question and
# asks a side question.
contrib/ios/smoke.sh                  # J3_SIM=<simulator name>, J3_PORT=<port>
```

The test server alone, e.g. to try the app by hand in a simulator:

```sh
cargo run -p j3-mobile --features test-server --bin j3-test-server -- --port 3799
xcrun simctl openurl booted 'j3code://connections/new?host=127.0.0.1:3799&autoConnect=1'
```

Its omp answers `Echo: <prompt>`; a prompt containing "ask" first opens a
`select` request ("Pick a color"), and a `/btw` side question is answered
`Aside: <question>`.

A read-only tour of a real server, writing screenshots on the Mac:

```sh
TEST_RUNNER_J3_BROWSE_URL=shieldy:3780 TEST_RUNNER_J3_SHOTS=/tmp/j3shots \
  xcodebuild -project J3Code.xcodeproj -scheme J3Code \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' -derivedDataPath build/dd \
  test -only-testing:J3CodeUITests/SmokeUITests/testBrowse
```

## Device

The iPhone needs Developer Mode on and to be paired with the Mac (Xcode ›
Devices). Signing is automatic with the team above.

```sh
contrib/ios/build-xcframework.sh
cd apps/ios && xcodegen generate
xcodebuild -project J3Code.xcodeproj -scheme J3Code -configuration Release \
  -destination 'generic/platform=iOS' -derivedDataPath build/dd \
  -allowProvisioningUpdates build

xcrun devicectl list devices
xcrun devicectl device install app --device <udid> \
  build/dd/Build/Products/Release-iphoneos/J3Code.app
xcrun devicectl device process launch --device <udid> \
  --payload-url 'j3code://connections/new?host=shieldy:3780&autoConnect=1' \
  dev.jacobyuill.j3code
```

The same bundle id as the previous app means the install replaces it; saved
servers are not carried over (re-add them with the deep link).

With a free (personal team) profile the app stops launching after 7 days;
build and install again. A first install from a new certificate needs the
profile trusted on the phone (Settings › General › VPN & Device Management).
