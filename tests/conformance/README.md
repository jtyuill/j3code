# j3 conformance client

A scripted client that drives a j3 server the way t3code's `client-runtime` and mobile app do, and lets t3's own schemas judge every byte. The schemas are a copy of t3code's `packages/contracts` in `../../packages/contracts`.

- WebSocket RPC: effect@4.0.5 `RpcClient.make(WsRpcGroup)` over `Socket.layerWebSocket` with `RpcSerialization.layerJson`, built as `CR/rpc/session.ts` builds it (no retries, 15 s open and ping timeouts, mobile query params, `orchestrationProtocol=2` last). Every reply and stream item is decoded by the contracts schemas inside the RpcClient. A value that does not decode fails the step and prints the `SchemaError` path.
- HTTP: `HttpApiClient.group(EnvironmentHttpApi, …)` for the descriptor and `/api/auth/session`. The attachment upload POST and asset GET have no contract; they are checked by status and bytes.

## Run

Node 26 runs the TypeScript sources directly through native type stripping, so there is no build step.

```sh
pnpm install    # from the repository root; installs the whole workspace
cd tests/conformance
pnpm conformance --url http://127.0.0.1:3780 [--model <provider/id>] [--project <dir>]
pnpm typecheck
```

| Option | Default | Meaning |
|---|---|---|
| `--url` | `http://127.0.0.1:3780` | server HTTP base URL |
| `--model` | first model of provider `omp` | omp model slug used in every `modelSelection`, e.g. `anthropic/claude-sonnet-4-5` |
| `--project` | this repository's root | project directory **on the server host**. It must exist; it is browsed and registered as a project, and the agent runs in it |
| `--timeout` | `300` | seconds each agent-driven wait may take |
| `--frames <file>` | | append every WebSocket text frame (both directions) to `<file>` as JSONL |

Each step prints `PASS`, `FAIL` (with the decode error, typed failure or timeout plus the observed state) or `SKIP` (a prerequisite did not pass). The exit code is 0 only when every step passes. Against a dead port, `descriptor`, `ws.connect` and `auth.session` fail with the connection error and everything else is skipped.

The agent steps send real prompts to omp: the agent is asked to call the `ask` tool, run `sleep 15` and `sleep 30` with bash, and reply with single words. A full run takes a few minutes and costs a few model turns.

## Steps

References are to `docs/protocol/ios-surface.md`.

| Step | Checks |
|---|---|
| `descriptor` | `GET /.well-known/t3/environment` decodes as `ExecutionEnvironmentDescriptor`, `orchestrationProtocolVersion` is 2 (H1) |
| `ws.connect` | `/ws?clientSurface=mobile&…&orchestrationProtocol=2` opens (H2) |
| `config.handshake` | the first `subscribeServerConfig` item is a `snapshot`; `environmentId` matches the descriptor; provider `omp` is enabled, installed, not unauthenticated, and has models including `--model` (M1) |
| `auth.session` | `/api/auth/session` decodes as `AuthSessionState` and `sessionGrantsScope` grants `orchestration:read`, `orchestration:operate`, `filesystem:read` (H3) |
| `server.probe` | answers within 3 s (M2) |
| `filesystem.browse` | browsing the project's parent (`<parent>/`) lists the project directory (M13) |
| `shell.subscribe` | `subscribeShell{requestCompletionMarker}`: `snapshot` first, then `synchronized` (§4.2) |
| `project.create` | `projects.mutate project.create` returns the project with the client's `projectId` (M12) |
| `shell.project.updated` | the shell stream delivers `project.updated` for it |
| `attachment.upload` | `attachments.createUploadUrl` → `POST` of a 1×1 PNG answers 2xx (M10, H4) |
| `attachment.asset` | `assets.createUrl` → `GET` returns the same bytes with `content-type: image/png` (M11, H5) |
| `thread.launch` | `orchestration.launchThread` with an `initialMessage` (+ the image) asking the agent to call `ask` with options red/blue; `threadId` and `projection.thread.id` match, `resumed:false` (§3.3) |
| `thread.subscribe` | `subscribeThread{requestCompletionMarker, acceptDedupedBodies}`: `snapshot` first, then `synchronized` |
| `thread.user-message` | the `user_message` item, the conversation message and `run.userMessageId` all carry the client's `messageId`; `inputIntent:"turn_start"` (§3.4) |
| `ask.waiting` | the run reaches `waiting` with a pending `user_input` runtime request **and** a `user_input_request` item for the same `requestId` offering "blue" (§4.3) |
| `shell.waiting` | the shell row shows `waiting` with `pendingRuntimeRequest.kind:"user_input"` for that request |
| `ask.respond` | `runtime-request.respond` with `answers:{<questionId>: <option value or label>}` |
| `ask.completed` | the request is `resolved`, the run `completed`, and the assistant text mentions blue |
| `shell.completed` | the shell row has no active run, no pending request, and `latestRunId` is that run |
| `queue.start` | `message.dispatch` with `start_immediately` (bash `sleep 15`) → its run is `running` |
| `queue.enqueue` | a second `message.dispatch` with `queue_after_active` → its run is first seen `queued`, and its `user_message` item has `inputIntent:"queued_turn"` (§4.5) |
| `queue.drain` | the first run completes, then the queued run starts and completes; its user item becomes `completed` |
| `interrupt.running` | `message.dispatch` of a bash `sleep 30` prompt → run `running` (waits up to 30 s for the command item) |
| `run.interrupt` | `run.interrupt{holdQueue:true}` → run `interrupted`, with `run_interrupt_request` and `run_interrupt_result` items (§4.5) |
| `dispatch.idempotent` | the same `message.dispatch` (same `commandId`) twice returns the same `sequence`, and only one run and one user item exist (§3.1) |
| `thread.rename` | `thread.metadata.update{title}` → `thread.metadata-updated` on the thread stream and the new title on the shell row |
| `thread.projection` | `getThreadProjection` agrees with the streamed runs and statuses; `visibleTurnItems` is non-empty, positioned 0..n-1, and includes the first user message (§4.3) |
| `reconnect.session` | close the socket, open a new one, and repeat the config handshake |
| `reconnect.thread` | `subscribeThread{afterSequence:<last seen>}`: no snapshot behind the cursor, no decreasing sequence, `synchronized` arrives (§4.1) |
| `reconnect.shell` | a fresh `subscribeShell` on the new socket: snapshot with the project and thread, then `synchronized` |
| `thread.archive` | `thread.archive` → `thread.archived` with `archivedAt` set; the thread leaves the active shell list |
| `archived.snapshot` | `getArchivedShellSnapshot` contains the thread (M9) |
| `thread.unarchive` | `thread.unarchive` → `thread.unarchived` with `archivedAt:null`; the thread is back in the active list |
| `thread.delete` | `thread.delete` → `thread.deleted` on the thread stream, `thread.removed` on the shell |
| `project.delete` | cleanup: `projects.mutate project.delete` → `project.removed` on the shell |
| `config.open` | `subscribeServerConfig` stayed open on both sockets (§2.2 M1) |
| `sequences` | across both sessions, thread and shell stream sequences only increased (overlap at or below the cursor is counted, not failed) |

The queue, interrupt and idempotency steps each wait for the thread to be idle first, so they still run when the `ask` turn misbehaves.

## Layout

- `src/main.ts`: CLI and the scenario
- `src/session.ts`: one WebSocket RPC session (socket, protocol, client, config subscription)
- `src/http.ts`: contracts HTTP client, upload and asset fetches
- `src/views.ts`: the thread and shell state a client derives from the streams, plus sequence checks
- `src/watcher.ts`: waiting on stream-derived state with timeouts
- `src/report.ts`: step runner and error formatting

`packages/contracts` is a pnpm workspace package; the workspace catalog pins one `effect` version, so contracts and harness share one copy.
