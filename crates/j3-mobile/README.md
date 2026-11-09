# j3-mobile

UniFFI bindings over `j3_client` and `j3_text` for the SwiftUI iOS app
(`docs/m8.md` D1, P4). The crate builds a `staticlib` (the iOS app links
it through an XCFramework), a `cdylib` (only to generate bindings from), and
the `uniffi-bindgen` binary.

## Surface

- `Host(baseUrl:)`: one server. `watch(listener:)` and
  `watchThread(threadId:listener:)` push `HostSnapshot` / `ThreadSnapshot`
  to Swift objects implementing `HostListener` / `ThreadListener`, and
  return a `Subscription` that stops on `cancel()` or release. Every other
  method is an `async throws` command (`ClientError`).
- `prepareMath`, `splitInlineMath`, `typesetFormula`, `rasterizeFormula`,
  `richUserText`: feed text preparation from `j3-text`. Snapshots carry raw
  text.
- `parseMarkdown`: an assistant message as flat blocks (list depth, quote
  depth and item marker on each block) with inline runs and formulas, for
  SwiftUI (`docs/m8.md` D2b). Math follows the desktop: display formulas
  become blocks, inline formulas in top-level paragraphs become runs.

Listeners are called on a thread of the library's tokio runtime (named
`j3-mobile`), never the main thread. Hop to the main actor and return at
once; a slow listener holds up one of the runtime's two workers:

```swift
@MainActor @Observable final class HostModel {
    var snapshot: HostSnapshot?
}

final class HostWatcher: HostListener {
    let model: HostModel
    init(model: HostModel) { self.model = model }
    func hostChanged(snapshot: HostSnapshot) {
        Task { @MainActor [model] in model.snapshot = snapshot }
    }
}
```

Each listener call carries the whole state (not a diff). Changes that land
while a call runs coalesce into the next call.

## Tests and bindings (any OS)

```sh
nix develop -c cargo test -p j3-mobile

# The cdylib for the build machine, then Swift bindings from it.
nix develop -c cargo build -p j3-mobile --lib --release
nix develop -c cargo run -p j3-mobile --bin uniffi-bindgen -- \
  generate --library target/release/libj3_mobile.so \
  --language swift --out-dir target/j3-mobile-swift
```

On macOS the library is `target/release/libj3_mobile.dylib`. The output is
`j3_mobile.swift`, `j3_mobileFFI.h` and `j3_mobileFFI.modulemap`. They are
not committed; regenerate them whenever the Rust surface changes.

## iOS build (macOS with Xcode)

`contrib/ios/build-xcframework.sh` builds the static library for
`aarch64-apple-ios` and `aarch64-apple-ios-sim` with rustup's toolchain
(the flake's has no iOS standard library; `--no-default-features` keeps the
bindgen CLI out), generates the Swift bindings and writes
`target/ios/J3MobileFFI.xcframework` and `target/ios/swift/j3_mobile.swift`,
which `apps/ios` links and compiles. It skips the build when its inputs did
not change. See `apps/ios/README.md`.

## Test server

`j3-test-server` (feature `test-server`) serves a fresh data directory whose
omp is a script, for the iOS smoke test (`contrib/ios/smoke.sh`):

```sh
cargo run -p j3-mobile --features test-server --bin j3-test-server -- --port 3799
```
