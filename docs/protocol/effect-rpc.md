# Effect RPC over WebSocket (t3code wire format)

This is what the j3 server must speak so t3code's clients (`packages/client-runtime`) connect unchanged. It covers effect@4.0.5 `effect/rpc` with `RpcSerialization.layerJson`. Rust types live in `crates/j3-protocol/src/effect_rpc.rs`. The captured frames in `crates/j3-protocol/tests/fixtures/effect_rpc/frames.json` are the ground truth.

Sources: t3code @ 8760d063 and effect 4.0.5 (`npm pack effect@4.0.5`, files under `src/rpc/`).

## Connection handshake (t3code)

1. **Descriptor preflight**: `GET /.well-known/t3/environment` (`packages/contracts/src/environmentHttp.ts`). The client compares `orchestrationProtocolVersion` (missing means 1) with its own `ORCHESTRATION_PROTOCOL_VERSION = 2` (`packages/contracts/src/environment.ts`, `packages/client-runtime/src/connection/compatibility.ts`). If they differ, it refuses to connect.
2. **Ticket preflight** (remote/bearer connections only): `POST /api/auth/websocket-ticket/rpc`, falling back to `POST /api/auth/websocket-ticket` on older servers. Auth is `authorization: Bearer <token>`, or `authorization: DPoP <token>` plus a `dpop` header (`packages/client-runtime/src/authorization/remote.ts`). The response contains `ticket`.
3. **Upgrade**: `GET /ws` (when the base URL has no path) with these query params:
   - `orchestrationProtocol=2` (required). On mismatch the server replies `426` with JSON `{code:"orchestration_protocol_incompatible", message, orchestrationProtocolVersion}` (`apps/server/src/ws.ts` `hasCompatibleOrchestrationProtocol`, route at `HttpRouter.add("GET","/ws",…)`).
   - `wsTicket=<ticket>` when a ticket was issued. If the ticket is DPoP-bound, the proof comes in the `dpop` header or the `dpop` query param, since browsers can't set WS headers (`apps/server/src/auth/EnvironmentAuth.ts` `authenticateWebSocketUpgrade`). Without a ticket the server falls back to normal request auth (session cookie / `authorization`).
   - Optional analytics params: `clientSurface`, `clientAppVersion`, `clientDeviceType`, `clientOs`, `clientWebDeployment`, `clientBrowser`, `clientOsMajorVersion`, `clientDeviceModel`, `connectionMethod`, `clientOwner`, `clientDevice` (`appendClientConnectionParams`).
   - No subprotocol and no custom headers (`Socket.layerWebSocket(url, {openTimeout: 15s})`, `packages/client-runtime/src/rpc/session.ts`).
4. **Server options**: `RpcServer.make(group, {disableTracing: true, disableFatalDefects: true})` with `RpcServer.makeProtocolWithHttpEffectWebsocket` and `RpcSerialization.layerJson`. With `disableFatalDefects`, a handler defect becomes that request's `Exit` Die instead of a connection-level `Defect`.

## Framing

- Every WebSocket text frame holds one JSON message. Inbound, a frame may also hold a JSON array of messages (`RpcSerialization.json.decode`). The server handles them in order (fixture `raw-batched-frame`). Effect never sends arrays.
- The server sends one message per frame (`JSON.stringify(response)`).
- Optional fields with `undefined` values are left out of the JSON.

## Client → server

| `_tag` | Fields | Notes |
|---|---|---|
| `Request` | `id`, `tag`, `payload`, `headers`, `traceId?`, `spanId?`, `sampled?`, `isNotification?` | `headers` is `[[name, value], …]`. `payload` is encoded by the payload schema. The client sends trace fields even though the t3code server disables tracing. |
| `Ack` | `requestId` | Sent after the consumer pulls each `Chunk`. |
| `Interrupt` | `requestId` | Cancels the request. The internal `interruptors` field is not on the wire. |
| `Ping` | — | Sent every 5 s (`makePinger`; t3code uses `pingTimeout: 15s`). |
| `Eof` | — | Not sent over WebSocket (HTTP protocol only). |

Request ids: the default client uses one **module-global JS number counter** (`requestIdCounter++`, `RpcClient.ts`). Ids start at 0, are not per-connection, and are never bigint strings. The server accepts `string | number` and echoes the id back in the same JSON type. Any other id type gets a `Defect` (`Invalid request id`).

Socket upgrade headers are prepended to each request's `headers` on the server side (`makeSocketProtocol` `headers.concat(message.headers)`). That merge happens server-side only and is not on the wire.

## Server → client

| `_tag` | Fields | Notes |
|---|---|---|
| `Chunk` | `requestId`, `values` (non-empty array) | One batch per pull from the handler stream. Stream chunking decides the batch size (fixture `stream-batched` sends `[1,2,3]` in one frame). |
| `Exit` | `requestId`, `exit` | Terminal message for that request. |
| `Defect` | `defect` | Connection-level error, e.g. undecodable JSON. The client fails **every** pending request. |
| `Pong` | — | Reply to `Ping`. |
| `ClientProtocolError` | `error` | Created inside the client only (socket failures). It never crosses the wire. |
| `Request` | as above | Bidirectional protocols only. t3code doesn't use it. |

### Exit / Cause

```
Exit  = {"_tag":"Success","value":<success-encoded>}      // streams and void: null
      | {"_tag":"Failure","cause":[Reason, …]}
Reason = {"_tag":"Fail","error":<error-schema-encoded>}   // tagged errors: {"_tag":"NotFound",…}
       | {"_tag":"Die","defect":<Schema.Defect>}          // Error → {"name","message"}; strings stay strings
       | {"_tag":"Interrupt","fiberId"?:number}
```

In effect 4 the Cause is a **flat array**. It has no Sequential/Parallel nodes (`RpcMessage.ExitEncoded`, `Schema.Cause`). The server reports schema and routing errors as a Die Exit on that request:
- unknown `tag` gives `"Unknown request tag: X"`
- a payload that fails to decode gives the formatted schema issue (fixtures `raw-unknown-tag`, `raw-bad-payload`).

## Streams, acks, interrupts

- A `stream: true` RPC answers with zero or more `Chunk`s, then an `Exit`. A normal end is `Success` with `value: null`. A failure mid-stream is a `Failure`/`Fail` Exit sent right after the chunks already delivered (fixture `stream-fail`).
- **Ack gating (required)**: the WebSocket client advertises `supportsAck: true`. After each `Chunk`, the server closes a per-request latch and sends nothing more on that stream until the matching `Ack` arrives (`RpcServer.ts` `streamEffect`). The client acks each chunk after the consumer pulls it. This allows exactly one un-acked chunk per stream. Elements produced in the meantime can be merged into the next `Chunk` batch.
- t3code exception (`apps/server/src/terminal/OutputProtocol.ts`): for `terminalAttach`/`terminalObserve`, the server acks its own chunks until 8 chunks or 64 KiB are un-acked. In effect this is a credit window. The client still sends one `Ack` per chunk.
- **Interrupt**: the client sends `Interrupt{requestId}`. The server interrupts the handler fiber and replies with `Exit` `Failure [{"_tag":"Interrupt","fiberId":N}]` (fixtures `stream-interrupt`, `unary-interrupt`). The client sends `Interrupt` when a consumer abandons a stream (e.g. `Stream.take`) or a unary call is interrupted.
- Responses whose `requestId` the client doesn't know are broadcast to all of its pending requests (`makeProtocolSocket` `processData`). Don't send them.

## Keepalive

The client pings every 5 s. Any inbound frame resets its timer, not just `Pong`. If nothing arrives within `pingTimeout` (15 s in t3code), the client closes the socket and fails in-flight calls. The server must answer every `Ping` with `Pong`.

## Capture

`frames.json` was captured on 2026-11-01 from a Node 26 script in `/tmp` using `effect@4.0.5`, `@effect/platform-node@4.0.5` and `@effect/platform-node-shared@4.0.5`. The setup:
- Server: `RpcServer.layer(group, {disableTracing: true, disableFatalDefects: true})` + `RpcServer.layerProtocolWebsocket({path: "/ws"})` + `RpcSerialization.layerJson` on a real `NodeHttpServer`.
- Client: `RpcClient.layerProtocolSocket({pingInterval: "150 millis"})` + `Socket.layerWebSocket("ws://127.0.0.1:47231/ws?orchestrationProtocol=2")`, with a `WebSocketConstructor` wrapper that logs every sent and received text frame.
- Scenarios:
  - unary success, tagged failure and defect
  - per-element stream, batched stream and failing stream
  - a stream abandoned after 2 elements (`Stream.take`)
  - an interrupted unary call and pings
- The `raw-*` scenarios were sent by hand over a plain `WebSocket`: unknown tag, bad payload, batched array frame and malformed JSON.
