# De-T3

Plan to remove every piece of t3code-derived code from j3code and stop describing the project as a fork. This is a plan only; nothing here has been implemented.

Upstream reference: t3code @ `8760d063e903406c8e8f695b0852c0a15fc1e00f` (the snapshot recorded in `NOTICE`), checked out at `/tmp/t3up` for the provenance numbers below.

## Done when

1. No tracked file is a copy, trim, or transcription of t3code. A check run at cutover finds zero tracked blobs whose hash matches a blob in the upstream snapshot.
2. No comment cites a t3code source path (`OV2:`, `C/…`, `CR/…`, `M/…`, `Sidebar.logic.ts`, `threadSort.ts`, …).
3. No t3 identity is left on the wire, on disk, or in the UI: `/.well-known/t3/environment`, `x-t3-orchestration-protocol`, `t3-context://`, `app.t3.context-fragment`, `t3.json`, `t3code.*` storage keys, `@t3tools/*`, `T3*` native modules, `t3-chat-*` themes, `Workflow::T3`, `workflow-t3`, `sidebar/t3*.rs`. CI enforces this with a deny-list grep (see [Gates](#gates)).
4. `NOTICE` no longer contains t3code's MIT notice. `README.md`, `PLAN.md`, `CONTRIBUTING.md`, `docs/protocol/`, and the iOS About screen stop saying "fork of t3code".
5. The iOS app on the iPhone has feature parity with today's app ([parity matrix](#ios-parity-matrix)) and talks to `j3-server` over the j3-owned protocol.

GitHub already reports `jtyuill/j3code` as `isFork: false, parent: null` (checked with `gh repo view`), so "fork status" means the code and the project's self-description, not a GitHub setting.

MIT does not require any of this. Once no substantial portion of t3code remains, the notice obligation lapses, which is what makes step 4 legitimate. The rewrite is done from j3's own behaviour specs rather than from t3 sources, but it is not a legal clean room; the authors have read t3code.

## Findings

### TypeScript side is almost entirely upstream

Tracked files under `apps/mobile`, `packages/`, `tests/conformance`, `patches/`, `docs/`, and the root JS configs, compared by blob hash and line diff with the upstream snapshot:

| Class | Files | Lines |
|---|---:|---:|
| Byte-identical to upstream | 904 | 258,788 |
| Same content at a different path | 11 | 539 |
| Modified, ≥80% of lines unchanged | 252 | 76,388 |
| Modified, 50–80% unchanged | 22 | 2,373 |
| Modified, <50% unchanged | 17 | 1,996 |
| No upstream counterpart | 19 | 3,684 |

Per area:

| Area | Identical | ≥80% same | New | Notes |
|---|---:|---:|---:|---|
| `apps/mobile/src` | 424 | 176 | 4 (160 lines) | ≈90k LOC. Effect 4 + Effect Atom state, Uniwind styling, custom native-stack navigator. Largest: `ThreadFeed.tsx` (123.7 KB), `ThreadDetailScreen.tsx`, `use-composer-drafts.ts`, `thread-work-log.tsx`, `threadOutboxDelivery.ts`. |
| `apps/mobile/modules` | 27 | 6 | 0 | `T3ComposerEditor`, `T3NativeControls`/`T3KeyboardCommands`, `T3Highlights` (wasm2c, 3.1 MB vendored C). |
| `apps/mobile/deps` | 3 | 0 | 0 | t3-built tarballs `react-native-enriched-markdown-…-t3.16`, `react-native-screens-…-t3.48`. |
| `packages/client-runtime` | 159 | 62 | 0 | ≈50k LOC; 54 of 71 subpaths imported by mobile. |
| `packages/contracts` | 75 | 1 | 0 | Verbatim; only `package.json`/`tsconfig.json` changed. |
| `packages/shared` | 110 | 1 | 1 | ≈23k LOC. |
| `patches/` | 18 | 0 | 0 | All 18 inherited from upstream `patchedDependencies`. |
| `tests/conformance` | 0 | 0 | 9 | j3-authored, but it decodes with `@t3tools/contracts`. |
| `docs/protocol` | 0 | 0 | 3 | j3-authored, but written as a spec of t3's wire with t3 source citations. |

The TS side has had no functional change since `650e5ca` ("M5 (part 2)"). Only four commits ever touched it (`35e2db3`, `b400fad`, `650e5ca`, `11cb65b`). The desktop has since gained features the phone lacks (`@path` completion, ghost prediction, terminal, files, git, dictation, usage).

There is nothing worth salvaging: 19 new files, mostly conformance and docs. The plan deletes the TypeScript tree outright.

### Rust side speaks t3 on the wire and stores t3 on disk

| Crate | Coupling |
|---|---|
| `j3-protocol` | `src/t3/` is 7,712 of 8,522 lines and is a transcription of `packages/contracts` with per-type upstream line citations. `effect_rpc.rs` (401) transcribes effect@4.0.5 RPC JSON framing. `j3.rs` (401), `t3/macros.rs`, and the time helpers are j3-original. |
| `j3-core` | 11,303 of 14,770 lines are in files importing `j3_protocol::t3`. `engine/model/triage.rs` is a port of `Orchestrator.ts`/`ThreadSettlementService.ts`. The SQLite store keeps t3 JSON (`threads.shell` = `OrchestrationV2ThreadShell`, `entities.json` with t3 entity kinds), with no `user_version` and no migrations (`store.rs:23-52,96`). The HTTP routes exist for the t3 client: `/.well-known/t3/environment`, `/api/auth/session`, and three 501 stubs. |
| `j3-client` | 7,846 of 8,282 lines import t3 types, and `lib.rs:24` re-exports `pub use j3_protocol::t3`. `projection.rs`, `shell.rs`, `thread.rs`, and `workflow/{status,sections,snooze,order,view,git}.rs` are ports of client-runtime and t3 web reducers, with line citations. |
| `j3-desktop` | 46 of ~123 files use t3 types. `sidebar/t3.rs`, `sidebar/t3_view.rs`, and `sidebar/menu.rs` port t3 web sidebar code. `Workflow::T3` is the default setting. Hand-written t3 JSON fixtures include `app/fixtures.rs` (1,522 lines). |

Wire surface today: 34 WS method tags (5 streams), 32 `dispatchCommand` variants (28 t3 + 4 `j3.goal.*`), 32 domain event types, and 16 TurnItem kinds. The full list is in `crates/j3-core/src/app.rs:592-760` and `crates/j3-protocol/src/t3/commands.rs:371-408`.

Coverage that must be replaced, not lost: protocol tests (55), core tests (56, including `engine_mapping.rs`, `engine_app.rs`, `transport_ws.rs`, `transport_http.rs`), and `j3-client/tests/client.rs` (10).

### History

t3 code entered history in `35e2db3` (contracts and conformance) and `b400fad` (mobile, client-runtime, shared, patches). The repo has 104 commits on one branch, `main`. revived's `/etc/nixos` flake pins `j3code` by revision (PLAN.md, M6).

## Decisions

### D1. iOS app: SwiftUI shell, Rust core over UniFFI

| Option | For | Against |
|---|---|---|
| A. Clean Expo/RN rewrite | Familiar stack, hot reload | Keeps Node/pnpm/Expo plus patch-pinning (dyld breakage on `expo-*` drift, `apps/mobile/README.md:37-39`). A third protocol client in TS. Rewriting 90k LOC of RN without re-deriving it from t3 is the hardest variant. |
| B. SwiftUI + hand-written Swift client | Fully native | A second protocol client, reducer set, and reconnect supervisor to keep in sync with `j3-client`. |
| **C. SwiftUI + `j3-client` via UniFFI** | One protocol client and state model for desktop and phone. Protocol changes break both clients at compile time. Removes Node, pnpm, Effect, the 18 patches, and the TS CI job entirely. | FFI layer to design. iOS cross-compiles add build time. |
| D. Rune for the whole iOS app | Shares desktop views | Rune's iOS support is embed-only (`rune::embed::LayerHost`, changelog 2026-10-30/31). No `UITextInput` yet (Rune PLAN.md M6 is pending), so it has no usable text input or IME. Accessibility on iOS is immature. |

**Choose C.** Swift owns navigation, lists, sheets, the composer (UIKit text view), and the system integration. Rust owns connection, protocol, state, markdown/math preparation, and highlighting.

### D2. Thread feed rendering is decided by a spike

The desktop feed already renders markdown, RaTeX math, code highlighting, and streaming fade-in through rune-kit (`j3-desktop/src/math/`, `thread/feed/`). The spike (P0.3) compares:

- **D2a.** A Rune `LayerHost` embedded as a `UIViewRepresentable` for the feed only, reusing desktop feed code.
- **D2b.** SwiftUI rendering from a Rust-side markdown AST exposed over FFI, with math as RaTeX SVG/PNG images.

Criteria: text selection and copy, scroll physics and 120 Hz smoothness, VoiceOver, streaming cost, and app size. Choose D2b unless D2a passes all five; text selection and accessibility are where D2a is most likely to fail. Either way, the markdown/math preparation code moves from `j3-desktop` into a UI-agnostic crate so both apps share it.

### D3. A j3-owned protocol (v2)

Design it from omp's model, not by renaming t3 types. Principles:

- **Transport.** JSON over one WebSocket. The envelope is j3's own and covers request/response, stream items with credit-based acks, cancel, ping/pong, and an error object `{code, message, data}`. No Effect `Exit`/`Cause`/`_tag` encoding.
- **Discovery.** `GET /api/v1/info` returns server id, name, protocol version, and capabilities. The protocol version is one constant in one place; today it lives in three (`t3/environment.rs:58`, `http.rs:25-27`, `app.rs:204`).
- **Entities.** `Project`; `Thread` (index row with status, title, archived, pin and triage fields); `Turn` (one prompt until `session_settled`); `Item` (user message, assistant text, thinking, tool call with output and images, todo list, goal, subagent, error); `Request` (pending `ask`/`select`/`confirm`/`input`/`editor`); `QueueEntry`. These replace t3's `Run`/`RunAttempt`/`ExecutionNode`/`ProviderSession`/`ProviderThread`/`ProviderTurn` layering.
- **Streaming.** Text deltas (`item.delta` appends) instead of re-sending whole items, plus `item.upsert` for structural changes. A monotonic server `sequence` stays for reconnect: snapshot first, then deltas after a given sequence.
- **Methods.** Namespaced by domain: `server.*`, `projects.*`, `threads.*`, `turns.*`, `requests.*`, `queue.*`, `files.*`, `git.*`, `terminal.*`, `attachments.*`, `usage.*`, `subagents.*`. The current j3 extensions (`j3.goal.*`, `j3.getUsage`, `j3.getSubagentTranscript`) become first-class methods.
- **Dropped.** `/api/auth/session`, the three 501 HTTP stubs, `server.reportClientActivity`, `server.updateSettings` (it always fails), the `filesystem.getMetadata` stub, the 7 ignored `accept*` flags on `subscribeThread`, `t3/background.rs` and `t3/environment_http.rs` (used only by tests), the `t3.json` project-defaults file, and every opaque-`Value` field kept for cut t3 features (pull requests, checkpoints, handoffs, guest share).
- **Spec.** `docs/protocol/wire.md` replaces `effect-rpc.md`, `ios-surface.md`, and `j3-extensions.md`. It has a method/event table and an old→new mapping for each of the 34 current tags and 32 commands.

### D4. Dual-stack transition, then a hard cutover

j3-core serves both stacks during the transition: v1 (t3) on `/ws` and the t3 HTTP routes, v2 on `/api/v1/*`. Both are fed from the same engine model. The current iPhone app keeps working until the new one reaches parity, and the desktop moves to v2 first, which proves the protocol before any Swift exists. The v1 code is deleted in P5 in one commit, with no compatibility shims kept after cutover.

The engine model already exists as an internal `ThreadModel` (`engine/model.rs`) that is then mapped to t3 entities, so v2 is a second mapper from the same model. [INFERENCE: P0.2 confirms the split before P2 starts.]

### D5. Persistence: a versioned schema and a one-shot migration

- Add `PRAGMA user_version` and an ordered migration list to `store.rs`.
- The v1→v2 migration copies projects and the thread index (id, project, session file, title, archived, deleted, plus pin/visit/settle/snooze fields lifted out of the `shell` JSON), then drops `entities`.
- Conversation items are rebuilt from the omp session `.jsonl` when a thread is first opened, through a new replay path. `transcript.rs` already parses omp session files for subagents. omp's session file stays the source of truth, as PLAN.md already states.
- Accepted loss: anything that lived only in the t3 projections and not in omp's session file, such as transient queue state of finished threads. P0.2 measures this by replaying real threads on shieldy and diffing against the stored projections.

### D6. Desktop thread triage is kept but re-specified

Settle, snooze, pin and reorder, unread, and auto-settle are j3 features users rely on, but today's code is a line-port of t3's `Sidebar.logic.ts`, `threadSort.ts`, `threadSettled.ts`, and `Orchestrator.ts`. P0.1 writes the behaviour as j3's own spec in `docs/triage.md`, from the desktop's observed behaviour and tests, not from t3 sources. P3 then reimplements it against v2 types. Names change: `Workflow::T3` becomes `Workflow::Inbox`, and `sidebar/t3.rs` and `t3_view.rs` become `sidebar/inbox.rs` and `inbox_view.rs`. The persisted setting value migrates the same way (`T3` → `Inbox`).

### D7. Keep git history

Old commits carry the old `NOTICE`, so distributing them stays compliant. Rewriting all 104 SHAs with `git filter-repo` or a fresh repo is destructive, breaks revived's pinned flake input, and removes nothing a reader needs. If a history-free repo is wanted later, it is a separate, explicit decision: a new repo seeded from the post-cutover tree.

### D8. Toolchain after cutover

No Node, pnpm, or TypeScript in the repo. The flake dev shell gains the iOS Rust targets (`aarch64-apple-ios`, `aarch64-apple-ios-sim`) and `xcodegen` on macOS. The Xcode project is generated from `apps/ios/project.yml`, and the generated `.xcodeproj` is gitignored, the same model as today's ignored `ios/` prebuild output.

## Phases

Each phase ends green: `cargo test --workspace` passes, and the current iPhone app still connects until P5.

### P0. Specs and spikes (no production code)

1. **Behaviour specs from j3, not from t3.** Write `docs/triage.md` (D6) and an iOS feature list (the [parity matrix](#ios-parity-matrix)), each from the running desktop and phone apps and from j3's own tests.
2. **Engine split check.** Confirm that `engine/model.rs` → `ThreadModel` is the single internal model. Replay 10 real shieldy threads from their omp `.jsonl` files and diff the result against the stored projections to size the loss in D5.
3. **Feed spike (D2).** A throwaway iOS app that embeds a Rune `LayerHost` showing one recorded thread, measured against the D2 criteria. Record the result in this doc.
4. **FFI spike.** A UniFFI wrapper over `j3-client` connecting to shieldy from the simulator. It answers: tokio runtime ownership, async callbacks to Swift, XCFramework build time, and binary size.

Exit: specs committed, D2 decided, the FFI approach proven on a real device.

### P1. Protocol v2 spec

Write `docs/protocol/wire.md` per D3: envelope, discovery, every method with params, result and errors, stream item kinds, sequence and reconnect rules, attachment upload and asset URLs, and the old→new mapping table. Review it against the parity matrix and the desktop feature set.

Exit: every current desktop and phone interaction maps to a v2 method or is listed as dropped.

### P2. Server on v2 (dual-stack)

1. **`j3-protocol`.** New modules for v2 types by domain, plus the v2 envelope codec. `t3/` and `effect_rpc.rs` stay untouched until P5.
2. **`j3-core`.** A v2 RPC driver and router; a v2 mapper from `ThreadModel`; `/api/v1/info`; attachments and assets under `/api/v1/`; store migrations (D5) and the replay path; triage reimplemented from `docs/triage.md`, sitting beside the ported `triage.rs` until P5.
3. **Tests.** v2 equivalents of `transport_ws.rs`, `transport_http.rs`, `engine_app.rs`, and `engine_mapping.rs`, with fixtures recorded from j3's own server instead of `fixtures/t3/real/*`.

Exit: shieldy runs dual-stack, and the old iPhone app still works against it.

### P3. Client and desktop on v2

1. **`j3-client`.** Moves to v2: connection supervisor, typed calls, and new reducers (shell, thread, inbox/triage) written from the v2 spec and `docs/triage.md`. Remove the `pub use j3_protocol::t3` re-export.
2. **`j3-desktop`.** Migrates all 46 t3-using files, renames per D6, and regenerates the hand-written fixtures (`app/fixtures.rs`, `git/fixtures.rs`, `files/fixtures.rs`, feed/terminal/models/completion/usage test JSON) in v2 shapes. The in-process local server is reached over v2.
3. **Shared crate.** Move markdown/math preparation (`j3-desktop/src/math/`, `message_text`) and highlighting into a UI-agnostic crate (working name `j3-text`) that the desktop and `j3-mobile` both use.

Exit: the desktop runs on v2 only, all 22 UI tests pass against a real server, and macOS plus revived get an interactive smoke run.

### P4. New iOS app (the bulk of the work)

Layout:

```
crates/j3-mobile/   UniFFI bindings over j3-client + j3-text; staticlib → XCFramework
apps/ios/           SwiftUI app; project.yml (XcodeGen); Swift tests
contrib/ios/        build-xcframework.sh (cargo for ios + ios-sim, uniffi-bindgen swift, xcodebuild -create-xcframework)
```

Steps:

1. **`j3-mobile` API.** The surface is observable state (hosts, shell, thread view models) plus commands. Rust keeps the reducers; Swift receives immutable snapshots or diffs through callback interfaces and publishes them through `@Observable` models.
2. **App shell.** iOS 18 minimum (matches today's `ios.deploymentTarget`), `NavigationSplitView` for iPad, hardware-keyboard shortcuts, bundle id `dev.jacobyuill.j3code`, URL scheme `j3code`, team `8NY34B669X`. Same bundle id means the new build replaces the old app in place. Saved hosts do not migrate because the storage keys change, so hosts are re-added once; the `j3code://connections/new?host=…&autoConnect=1` deep link is kept for that.
3. **Screens, in order.** Connections → thread list (grouped by project, archive) → thread feed (D2 result) → composer → requests (`ask` and the other dialogs) → queue/steer/stop → new thread flow → settings.
4. **Composer.** A `UITextView` subclass with atomic chips for `/skill`, `^model`, and `@path` (desktop semantics; completion from `projects.searchEntries`), images from PhotosPicker and paste, plus model and thinking pickers. It is written new, not adapted from `T3ComposerEditor`.
5. **Tests.** Rust tests for `j3-mobile` (state over an in-process server, as `j3-client/tests/client.rs` does today), XCTest for view-model logic, and one simulator smoke script (build, install, launch, connect to a local `j3-server` with a scripted omp, send a prompt, answer an `ask`). Every documented build command goes in `apps/ios/README.md`.

Exit: parity matrix complete on a physical iPhone against shieldy, including a reconnect after backgrounding and a network change.

### P5. Cutover and deletion

Delete everything in [Deletion manifest](#deletion-manifest) in one change. Move the old live-omp protocol check (`tests/conformance`, never run in CI) to an `#[ignore]` Rust test that drives a real server through `j3-client`.

Exit: `cargo test --workspace` passes, the CI `rust` job passes, the `js` job is gone, and the deny-list gate is added and green.

### P6. Identity and docs

- `NOTICE`: remove the t3code paragraph and its MIT text. Keep omp, LobeHub/OpenCode logos, and Lucide.
- `README.md`: drop "Forked from t3code"; replace the layout rows for `apps/mobile` and `packages/` with `apps/ios`, `crates/j3-mobile`, and `contrib/ios`; remove the `pnpm` test lines.
- `PLAN.md`: rewrite the intro, the decision rows (iOS, Wire protocol, Persistence, Auth, Repo, License, Build), the Layout block, and the conformance paragraph. Milestones M2/M3/M5 keep their history and gain a pointer to this plan.
- `CONTRIBUTING.md`: replace the TS instructions with iOS build steps.
- `crates/j3-core/src/serve.rs:20-21`: drop the "t3code keeps 3773" comment. The port stays 3780.
- `.github/workflows/ci.yml`: remove the `js` job; optionally add a macOS job that builds `j3-mobile` for `aarch64-apple-ios-sim`.

### P7. Final provenance check

Run the hash comparison against `/tmp/t3up` over the post-cutover tree (expected: 0 matches), the deny-list grep, and a grep for upstream citation patterns. Record the results here and mark the milestone done in `PLAN.md`.

## iOS parity matrix

"Today" is the current Expo app.

| Feature | Today | New app v1 |
|---|---|---|
| Hosts: add by URL, rename, change URL, disable, remove; dev deep link | yes | yes |
| Multiple hosts at once | yes | yes |
| Thread list grouped by project; archive/unarchive/delete/rename | yes | yes |
| Add project on a remote host (directory browse) | yes | yes |
| New thread: host, project, model, thinking level | yes | yes |
| Feed: markdown, code, math, tool calls, thinking, images, todos, goals, subagents | yes | yes (math via RaTeX, as on desktop; D2) |
| Subagent transcript | no | yes (`subagents.transcript`) |
| Requests: `ask`/`select`/`confirm`/`input`/`editor` | yes | yes |
| Composer: text, images, slash commands/skills, `^model` | yes | yes |
| Composer: `@path` file references | no | yes |
| Stop, steer, queue (edit, cancel, promote) | yes | yes |
| iPad split view, hardware keyboard, command palette | yes | split view and shortcuts; palette later |
| Image viewer, file preview (QuickLook) | yes | yes |
| Video playback, HTML render webview | yes | dropped unless the parity review keeps them |
| Ten color themes | yes | system light/dark only |
| Triage (pin, settle, snooze, unread) | partial | later, after desktop P3 |
| Terminal, files pane, git, usage, dictation | no | later; dictation via the iOS keyboard |

## Deletion manifest

Removed in P5:

- **TypeScript tree.** `apps/mobile/` (including `modules/t3-*`, `deps/*.tgz`, `plugins/`, `scripts/`, generated Uniwind themes), `packages/contracts`, `packages/client-runtime`, `packages/shared`, `tests/conformance`, `patches/`, `package.json`, `pnpm-workspace.yaml`, `pnpm-lock.yaml`, `.pnpmfile.cjs`, and `tsconfig.base.json`. Also the local `node_modules/`, which is untracked.
- **`j3-protocol`.** `src/t3/` (19 files), `src/effect_rpc.rs`, `tests/t3_server.rs`, `tests/t3_orchestration.rs`, `tests/effect_rpc.rs`, and `tests/fixtures/{t3,effect_rpc}/`.
- **`j3-core`.** The v1 RPC driver (`rpc.rs`), the t3 HTTP routes and protocol query/header checks (`http.rs`), the t3 mapper half of `engine/projection.rs`, the ported `engine/model/triage.rs`, `ServerProvider` catalog shaping in `catalog.rs`, and the v1 tests.
- **`j3-client`.** The t3 re-export and the ported `projection.rs`, `shell.rs`, `thread.rs`, and `workflow/*` reducers.
- **`j3-desktop`.** `sidebar/t3.rs`, `sidebar/t3_view.rs`, the ported `sidebar/menu.rs` logic, and t3 fixture JSON.
- **Docs.** `docs/protocol/effect-rpc.md`, `ios-surface.md`, and `j3-extensions.md`, all superseded by `wire.md`.

## Gates

Two CI checks are added in P5. Both run in the existing `rust` job as plain shell steps.

1. **Deny-list grep over tracked files.** Pattern, case-insensitive: `t3code|t3tools|pingdotgg|\bt3\b|x-t3-|t3-context|well-known/t3|T3[A-Z][a-z]`. It excludes this document and `PLAN.md` history rows. Expected matches: 0.
2. **Citation grep.** Pattern: `OV2:|C/[a-zA-Z]+\.ts|CR/|Sidebar\.logic|threadSort\.ts|threadSettled\.ts|Orchestrator\.ts`. Expected matches: 0.

The blob-hash provenance check (P7) is run once by hand at cutover, because it needs the upstream checkout.

## Risks

| Risk | Mitigation |
|---|---|
| The phone has no working app between deleting v1 and shipping the new one | Dual-stack (D4). v1 is deleted only after the parity matrix passes on the device. |
| DB migration loses state | Back up `j3.sqlite` on each host before the migration. P0.2 sizes the loss. The migration is tested on a copy of shieldy's DB. |
| Rune iOS embed is weeks old | It is used for the feed only, and only if the spike passes (D2). D2b is the fallback. |
| UniFFI async, tokio, and the iOS app lifecycle (background suspension kills sockets) | The FFI spike covers backgrounding. The supervisor reconnects on `scenePhase` active. |
| Rust iOS builds slow the Swift edit loop | Prebuilt XCFramework is cached by `contrib/ios/build-xcframework.sh` and rebuilt only when `crates/` change. |
| Desktop fixture churn hides regressions | Fixtures are regenerated from a recorded v2 server session, not edited by hand. The 22 UI tests stay the gate. |
| Free-team profile expires every 7 days | Unchanged from today. Documented in `apps/ios/README.md`. |
| Re-deriving triage from memory of t3 | Spec it from observed j3 behaviour first (P0.1). Implement only from the spec. |

## Open questions

1. Triage on the phone: is "later" right, or is it part of v1?
2. Video playback and the HTML render webview: keep for v1 or drop?
3. History (D7): confirm keeping it rather than starting a fresh repo.
