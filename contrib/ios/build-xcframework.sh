#!/usr/bin/env bash
# Builds crates/j3-mobile for iOS: the static library for the device
# (aarch64-apple-ios) and the simulator (aarch64-apple-ios-sim), the Swift
# bindings, and target/ios/J3MobileFFI.xcframework that apps/ios links.
#
# Skipped when nothing the library is built from changed since the last run
# (the Rust sources of j3-mobile and the crates it uses, Cargo.toml,
# Cargo.lock). Pass --force to rebuild anyway.
#
# Needs macOS with Xcode and rustup (the iOS targets are added on first use).
#
#   contrib/ios/build-xcframework.sh [--force]
#
# Environment:
#   J3_PROFILE   cargo profile of the iOS slices: release (default) or dev.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/target/ios"
cargo_target="${CARGO_TARGET_DIR:-$root/target}"
profile="${J3_PROFILE:-release}"
force=0
[[ "${1:-}" == "--force" ]] && force=1

cd "$root"

# rustup's toolchain: a Homebrew `rust` earlier on PATH has no iOS std.
toolchain_bin="$(dirname "$(rustup which cargo)")"
export PATH="$toolchain_bin:$PATH"
export IPHONEOS_DEPLOYMENT_TARGET=18.0

inputs() {
  {
    find crates/j3-mobile crates/j3-client crates/j3-protocol crates/j3-text \
      -type f \( -name '*.rs' -o -name 'Cargo.toml' \) -not -path '*/tests/*'
    echo Cargo.toml
    echo Cargo.lock
  } | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256 | cut -d' ' -f1
}

stamp="$(inputs)-$profile"
if [[ $force == 0 && -f "$out/stamp" && "$(cat "$out/stamp")" == "$stamp" \
  && -d "$out/J3MobileFFI.xcframework" && -f "$out/swift/j3_mobile.swift" ]]; then
  echo "build-xcframework: up to date ($out)"
  exit 0
fi

start=$SECONDS
targets=(aarch64-apple-ios aarch64-apple-ios-sim)
installed="$(rustup target list --installed)"
for target in "${targets[@]}"; do
  grep -qx "$target" <<<"$installed" || rustup target add "$target"
done

# The bindings come from the library's metadata, read by the bindgen built
# for this Mac (a debug build is enough).
cargo build -p j3-mobile --lib --bin uniffi-bindgen

dir="$profile"
[[ "$profile" == dev ]] && dir=debug
for target in "${targets[@]}"; do
  # --no-default-features leaves the bindgen CLI out; only the staticlib is
  # needed, not the crate's cdylib.
  cargo rustc -p j3-mobile --lib --crate-type staticlib --profile "$profile" \
    --no-default-features --target "$target"
done

rm -rf "$out/swift" "$out/headers" "$out/J3MobileFFI.xcframework"
mkdir -p "$out/swift" "$out/headers"
"$cargo_target/debug/uniffi-bindgen" generate --library "$cargo_target/debug/libj3_mobile.dylib" \
  --language swift --out-dir "$out/swift"
cp "$out/swift/j3_mobileFFI.h" "$out/headers/"
cp "$out/swift/j3_mobileFFI.modulemap" "$out/headers/module.modulemap"
xcodebuild -create-xcframework \
  -library "$cargo_target/aarch64-apple-ios/$dir/libj3_mobile.a" -headers "$out/headers" \
  -library "$cargo_target/aarch64-apple-ios-sim/$dir/libj3_mobile.a" -headers "$out/headers" \
  -output "$out/J3MobileFFI.xcframework" >/dev/null

echo "$stamp" >"$out/stamp"
echo "build-xcframework: built in $((SECONDS - start)) s ($out)"
