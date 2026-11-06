#!/bin/bash
# Builds j3 (release) and wraps it in a macOS app bundle, ad-hoc signed.
#   contrib/macos/bundle.sh [dest-dir]   (default: ~/Applications)
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
dest=${1:-$HOME/Applications}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)

cargo build --release -p j3-desktop --manifest-path "$root/Cargo.toml"

app="$dest/j3.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$root/target/release/j3" "$app/Contents/MacOS/j3"

# Icon: the iOS app icon, scaled into an .icns.
iconset=$(mktemp -d)/j3.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size "$root/apps/mobile/assets/brand/ios-icon-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) "$root/apps/mobile/assets/brand/ios-icon-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/j3.icns"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>j3</string>
  <key>CFBundleDisplayName</key><string>j3</string>
  <key>CFBundleIdentifier</key><string>dev.jacobyuill.j3code.desktop</string>
  <key>CFBundleExecutable</key><string>j3</string>
  <key>CFBundleIconFile</key><string>j3</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key><string>j3 connects to j3 servers on your tailnet and local network.</string>
  <key>NSMicrophoneUsageDescription</key><string>j3 transcribes your dictation on this Mac.</string>
</dict>
</plist>
EOF

codesign --force --deep --sign - "$app"
echo "$app"
