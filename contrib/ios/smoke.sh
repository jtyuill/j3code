#!/usr/bin/env bash
# Simulator smoke test of the iOS app: builds the XCFramework and the app,
# starts a j3 server with a scripted omp (crates/j3-mobile, bin
# j3-test-server), boots a simulator, and runs the UI test that installs and
# launches the app, connects through the j3code:// deep link, opens the
# server's thread, sends a prompt and answers omp's question.
#
#   contrib/ios/smoke.sh
#
# Environment:
#   J3_SIM    simulator name (default "iPhone 17 Pro")
#   J3_PORT   test server port (default 3799)
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
sim="${J3_SIM:-iPhone 17 Pro}"
port="${J3_PORT:-3799}"
log="$(mktemp -t j3-test-server)"

cd "$root"
contrib/ios/build-xcframework.sh

export PATH="$(dirname "$(rustup which cargo)"):$PATH"
cargo build -p j3-mobile --features test-server --bin j3-test-server
server_bin="${CARGO_TARGET_DIR:-$root/target}/debug/j3-test-server"
"$server_bin" --port "$port" >"$log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
for _ in $(seq 1 120); do
  grep -q "listening on" "$log" && break
  kill -0 $server 2>/dev/null || { cat "$log"; echo "smoke: the test server exited" >&2; exit 1; }
  sleep 0.5
done
grep -q "listening on" "$log" || { cat "$log"; echo "smoke: the test server did not start" >&2; exit 1; }
echo "smoke: $(grep 'listening on' "$log")"

udid="$(xcrun simctl list devices available -j | /usr/bin/python3 -c '
import json, sys
name = sys.argv[1]
for runtime, devices in json.load(sys.stdin)["devices"].items():
    for device in devices:
        if device["name"] == name and "iOS" in runtime:
            print(device["udid"]); sys.exit(0)
sys.exit(1)' "$sim")" || { echo "smoke: no simulator named $sim" >&2; exit 1; }
xcrun simctl boot "$udid" 2>/dev/null || true
xcrun simctl bootstatus "$udid" -b >/dev/null
# A stale install can fail the launch preflight; start clean.
xcrun simctl uninstall "$udid" dev.jacobyuill.j3code 2>/dev/null || true

cd apps/ios
xcodegen generate --quiet
set +e
TEST_RUNNER_J3_SMOKE_URL="http://127.0.0.1:$port" xcodebuild \
  -project J3Code.xcodeproj -scheme J3Code \
  -destination "platform=iOS Simulator,id=$udid" \
  -derivedDataPath build/dd \
  test -only-testing:J3CodeUITests/SmokeUITests/testPromptAndAsk \
  | grep -E "error:|Test Case|TEST (SUCCEEDED|FAILED)|XCTAssert|failed"
result=${PIPESTATUS[0]}
set -e
if [[ $result == 0 ]]; then echo "smoke: passed"; else echo "smoke: FAILED (server log: $log)"; fi
exit "$result"
