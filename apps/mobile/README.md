# j3 iOS app

The iPhone and iPad client for j3 servers. It is a trimmed fork of t3code's Expo app: it keeps
projects, threads (streaming output, tools, thinking, questions, todos, goals, subagents), the
composer (images, model and thinking level, slash commands and skills) and stop/steer/queue. It
talks to `j3-server` through `packages/client-runtime` with no sign-in: you add a host by URL and
the app connects to it directly, normally over Tailscale.

iOS only. Everything below runs from this directory on a Mac with a current Xcode and Node 26.

## Install

```sh
pnpm install        # from the repository root
```

## Run on the simulator

```sh
pnpm ios            # expo prebuild --clean --platform ios, then expo run:ios (Debug, Metro)
pnpm start          # Metro only, for a build that is already installed
```

`pnpm ios:release` builds the Release configuration with the JS bundle embedded, so it runs
without Metro. The same build with `xcodebuild` directly (a cold build takes about 20 minutes):

```sh
EXPO_NO_GIT_STATUS=1 pnpm exec expo prebuild --clean --platform ios
cd ios
xcodebuild -workspace j3.xcworkspace -scheme j3 -configuration Release \
  -sdk iphonesimulator -destination "generic/platform=iOS Simulator" \
  -derivedDataPath /tmp/j3-dd build
xcrun simctl install booted /tmp/j3-dd/Build/Products/Release-iphonesimulator/j3.app
xcrun simctl launch booted dev.jacobyuill.j3code
```

Expo modules ship precompiled iOS frameworks tied to one `expo-modules-core` build, so the
root `pnpm-workspace.yaml` pins the `expo-*` patch versions; a newer one aborts at launch with a
dyld "Symbol not found" in an `Expo*.framework`.

## Run on your iPhone with a personal team

`app.config.ts` sets the bundle id `dev.jacobyuill.j3code` and `ios.appleTeamId`. To use your
own free Apple ID team, change `appleTeamId` (and the bundle id if it is taken), then:

1. In Xcode, sign in with the Apple ID under Settings > Accounts.
2. Plug in the iPhone, trust the Mac, and turn on Developer Mode (Settings > Privacy & Security).
3. Build and install:

   ```sh
   EXPO_NO_GIT_STATUS=1 pnpm exec expo prebuild --clean --platform ios
   pnpm exec expo run:ios --device --configuration Release
   ```

   Or without the Expo CLI, from `ios/`:

   ```sh
   xcodebuild -workspace j3.xcworkspace -scheme j3 -configuration Release -sdk iphoneos \
     -destination "generic/platform=iOS" -derivedDataPath /tmp/j3-dd-device \
     -allowProvisioningUpdates DEVELOPMENT_TEAM=8NY34B669X CODE_SIGN_STYLE=Automatic build
   xcrun devicectl list devices
   xcrun devicectl device install app --device <udid> \
     /tmp/j3-dd-device/Build/Products/Release-iphoneos/j3.app
   xcrun devicectl device process launch --device <udid> dev.jacobyuill.j3code
   ```

4. On first launch, trust the developer profile under Settings > General > VPN & Device
   Management.

A free team's provisioning profile expires after 7 days; run step 3 again to renew it.

## Point it at a server

Start `j3-server` on the host (it listens on port 3780 by default) and make sure the phone can
reach it, for example on the same tailnet. In the app, open Connections, add a host and enter its
URL, such as `http://shieldy:3780` (`http://` is assumed when the scheme is left out). The app
reads `/.well-known/t3/environment`, saves the host and connects. Saved hosts can be renamed,
moved to another URL, switched off or removed from Settings.

In a development build, a deep link fills in and submits the form:

```sh
xcrun simctl openurl booted "j3code://connections/new?host=http://shieldy:3780&autoConnect=1"
```

## Checks

```sh
pnpm typecheck
pnpm test
```
