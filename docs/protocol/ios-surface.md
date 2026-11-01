# iOS thread surface: minimal server spec

This is the smallest server surface the trimmed iOS app needs. The app is a fork of `t3code/apps/mobile` and talks to the server through `packages/client-runtime`. The fork keeps:

- connecting by host URL with no auth
- projects: list, and add by path with a remote directory listing
- thread list: create, archive, delete, rename
- thread view: streamed assistant text, tool calls and thinking
- composer: text, images, model and thinking picker, slash commands and skills
- stop, steer and queue
- answering agent questions
- todos

The Effect RPC envelope (`Request`/`Chunk`/`Ack`/`Exit`/`Ping`/`Pong`, Cause encoding, ack gating) is specified in [`effect-rpc.md`](effect-rpc.md). This document covers what goes inside the envelopes.

Sources are t3code @ `8760d063` and omp RPC (`/tmp/omp-src`). Citation prefixes:

| Prefix | Path |
|---|---|
| `C/` | `packages/contracts/src/` |
| `OV2:` | `packages/contracts/src/orchestrationV2.ts` |
| `CR/` | `packages/client-runtime/src/` |
| `M/` | `apps/mobile/src/` |
| `S/` | `apps/server/src/` |
| `PI/` | `packages/provider-pi/src/server/`. This is t3code's adapter for Pi, the agent omp forked from. Its RPC is close to omp's, so it is the best reference for the mapping in §5. |
| `R:` | `/tmp/omp-src/docs/rpc.md` |
| `W:` | `/tmp/omp-src/sdk/rust/omp-rpc/src/wire.rs` |

`[INFERENCE]` marks claims that were not checked against source or a running system.

## 0. Summary

| Surface | Must be real | Constant stub is enough | Not needed after the client edits in §1.5 / §6 |
|---|---|---|---|
| HTTP endpoints (§1) | 5: descriptor, `/ws` upgrade, `/api/auth/session`, attachment upload POST, asset GET | 3: shell snapshot, bounded thread snapshot, thread history (return 501) | 2: `/oauth/token`, `/api/auth/websocket-ticket[/rpc]` |
| WS RPC methods (§2) | 13 | 9 | 10 more are reachable only through server flags j3 does not set, or through UI the trim removes; answer them with a per-request failure |
| `dispatchCommand` variants (§3) | 17, plus `orchestration.launchThread` for creating threads | 9 triage/list commands that only need to succeed (or be trimmed out of the UI) | 29 others (55 in the union: 17 + 9 + 29) |
| Stream item kinds (§4) | `subscribeServerConfig`: 1 (`snapshot`), optionally 1 more (`providerStatuses`). `subscribeShell`: 6. `subscribeThread`: 3 frame kinds carrying 20 domain-event types and 15 TurnItem kinds | | |

### Ground rules for the wire

These apply to every shape below.

- **Ids.** Every branded id (`ThreadId`, `ProjectId`, `CommandId`, `MessageId`, `RunId`, `RunAttemptId`, `NodeId`, `TurnItemId`, `RuntimeRequestId`, `PlanId`, `EventId`, `EnvironmentId`, `ProviderSessionId`, `ProviderThreadId`, `ProviderTurnId`, …) is `TrimmedNonEmptyString.pipe(Schema.brand(..))` (`C/baseSchemas.ts:246-335`; `TrimmedNonEmptyString` at `:14-29`). On the wire it is a plain string that must be non-blank after trimming. There is no format check. The client generates UUIDv4s for `CommandId`, `MessageId`, `ThreadId` and `ProjectId` (`M/lib/commandMetadata.ts:10-25`). It also derives command ids such as `"<uuid>:runtime-mode"` (`M/state/threadOutboxDelivery.ts:187-189`), so the server must accept any non-blank string.
- **Constrained ids.** `ProviderInstanceId` and `ProviderDriverKind` match `^[a-zA-Z][a-zA-Z0-9_-]*$` and are at most 64 characters (`C/providerInstance.ts:7-22`). `ChatAttachmentId` matches `/^[a-z0-9_-]+$/i` (`C/chatAttachment.ts:34-37`).
- **Integers.** `NonNegativeInt` is an int ≥ 0 and `PositiveInt` an int ≥ 1 (`C/baseSchemas.ts:31-32`).
- **Timestamps.** `IsoDateTime` is a plain string with no validation (`C/baseSchemas.ts:45`). `Schema.DateTimeUtc` fields are ISO-8601 UTC strings on the JSON wire, e.g. `"2026-11-01T12:00:00.000Z"`. [INFERENCE: this follows from the RPC using `Schema.toCodecJson`, `S/orchestration-v2/ThreadTransportPerformance.test.ts:24-26`.]
- **Optionality.** `Schema.optional(X)` means the key may be absent. `Schema.optionalKey(X)` also means the key may be absent; neither allows `undefined` on the wire, so omit the key instead of sending null. `Schema.NullOr(X)` means the key is required and the value may be `null`. `withDecodingDefault` means the key may be absent.
- **Tagged errors.** They encode as `{"_tag":"ErrorName", ...fields}`. A `cause: Schema.optional(Schema.Defect())` field can always be omitted.
- **Strict decoding.** The client decodes every response and stream item with the contracts schema. One missing required field or one wrong literal fails that item. The subscription then errors and is retried after 250 ms (`CR/state/threads.ts:1030-1035`, `CR/state/shell.ts:437-441`), so a server that keeps sending a bad shape spins forever. **Every non-optional field below is hard-required, even if no screen reads it.** The only exceptions are the forward-compatible arrays and unions noted in §4.4.

## 1. Connection bootstrap (HTTP + WS handshake)

### 1.1 Order of requests on every connect and reconnect

1. `GET /.well-known/t3/environment`. The resolver checks that `environmentId` matches the saved environment and that `orchestrationProtocolVersion == 2` (`CR/connection/resolver.ts:296-325`).
2. WebSocket `GET /ws?…&orchestrationProtocol=2` (`CR/connection/resolver.ts:83-89,314-325`; `CR/connection/compatibility.ts:37-41`).
3. Over the WebSocket: `subscribeServerConfig` with payload `{"usageLimitSources":true,"usageLimitsCommand":true,"workspaceSnapshotsOnDemand":true}` (`CR/rpc/session.ts:170-179,269`; `M/connection/runtime.ts:39`). **The first `{"type":"snapshot"}` item is the handshake.** `session.ready` resolves only after it arrives (`CR/rpc/session.ts:305-307,419-423`), and `config.environment.environmentId` must equal the descriptor's id (`:327-343`).
4. `GET /api/auth/session`, fetched again for every new transport session (`CR/state/session.ts:156-210`).
5. `GET /api/orchestration/shell`. On any failure the client subscribes `orchestration.subscribeShell` without `afterSequence` and takes the snapshot from the socket (`CR/state/shell.ts:377-435`).
6. For each open thread: `GET /api/orchestration/threads/:id/bounded`, then `orchestration.subscribeThread` (`CR/state/threads.ts:937-1029`).
7. Later, on app foreground, retry or network change: `server.probe` (`CR/connection/supervisor.ts:628-661,731-808`). Separately, 500 ms after a probe starts, the supervisor polls the descriptor; if HTTP answers but the socket has been silent, it declares the socket dead (`:663-695`).

Reconnect backoff runs from 1 s up to 300 s and resets after 30 s connected (`CR/connection/supervisor.ts:42-43,60`). Supervisor timeouts are 15 s to establish and 75 s to synchronize (`:46-48`).

### 1.2 HTTP plumbing

- The base URL is cut down to the origin root: `remoteApiBaseUrl` sets `pathname="/"` (`CR/rpc/http.ts:89-95`). The HTTP client is `HttpApiClient.make(EnvironmentHttpApi)` (`CR/rpc/http.ts:97-119`). The contract is `C/environmentHttp.ts:838-846`.
- On a credential-less connection (`httpAuthorization === null`) **no auth headers are sent** (`CR/state/environmentHttpAuth.ts:43-60`).
- Orchestration endpoints add the header `x-t3-orchestration-protocol: 2` (`CR/state/environmentHttpAuth.ts:28-37`; schema `C/environmentHttp.ts:80-84`).
- Error bodies are tagged JSON (`C/environmentHttp.ts:131-251`):
  - `EnvironmentRequestInvalidError` (400)
  - `EnvironmentAuthInvalidError` (401)
  - `EnvironmentScopeRequiredError` (403)
  - `EnvironmentOperationForbiddenError` (403)
  - `EnvironmentResourceNotFoundError` (404), `{"_tag":"EnvironmentResourceNotFoundError","code":"not_found","reason":"thread_not_found","traceId":"…"}`
  - `EnvironmentInternalError` (500)

  Each has a required `traceId` (TrimmedNonEmptyString). A body that does not decode is reported as `RemoteEnvironmentAuthUndeclaredStatusError(status)` (`CR/rpc/http.ts:121-175`).

### 1.3 Endpoints

#### H1. `GET /.well-known/t3/environment`: MUST be real

- Contract: `C/environmentHttp.ts:528-532`. Schema: `ExecutionEnvironmentDescriptor` (`C/environment.ts:169-178`).
- Client call sites: `CR/environment/descriptor.ts:8-17` (10 s timeout), `CR/connection/resolver.ts:296-310`, and the liveness check `CR/connection/driver.ts:150-179` (2.5 s timeout, `cache:"no-store"`, called repeatedly while the app is in the foreground). **The endpoint must be cheap.**
- Reference server handler: `S/http.ts:314-327` → `S/environment/ServerEnvironment.ts:208-294`.
- Fields:

| field | type | req | notes |
|---|---|---|---|
| `environmentId` | EnvironmentId | yes | **Must stay the same across restarts.** A mismatch triggers `environmentMismatchError` (`CR/connection/resolver.ts:305-310`). Persist it in SQLite. |
| `label` | TrimmedNonEmptyString | yes | hostname |
| `platform` | `{os:"darwin"\|"linux"\|"windows"\|"unknown", arch:"arm64"\|"x64"\|"other", machine?}` | yes | `C/environment.ts:18-46`. Unknown `machine` values decode as absent. |
| `serverVersion` | TrimmedNonEmptyString | yes | |
| `orchestrationProtocolVersion` | Int | optionalKey | **Send `2`.** If it is absent the client assumes 1 and blocks with `ConnectionBlockedError{reason:"unsupported"}` (`CR/connection/compatibility.ts:9-27`, `C/environment.ts:13`). |
| `databaseMigration` | Int | optionalKey | omit |
| `capabilities` | `ExecutionEnvironmentCapabilities` | yes | `C/environment.ts:72-166`. `{}` decodes, because `repositoryIdentity` defaults to false and every other key is optionalKey. Recommended values for j3 are below. |

- Recommended j3 capabilities:
  - `threadStop:true`. Stop then sends `thread.stop` without fetching the projection when no runId is known (`CR/operations/commands.ts:818-828`).
  - `attachmentUploads:true`.
  - `providerWorkspaceSnapshotsOnDemand:true`. Merges per-cwd slash commands and skills from `server.refreshProviders` (`CR/state/server.ts:685-709`).
  - Optionally `fileAttachments:{maxUploadBytes:N}`. Leave it out to hide non-image file attachments (`M/features/threads/ThreadComposer.tsx:977-983`).
  - **Do not set** `commandOutputStreaming`, `threadForceStop` or `threadSettlement`.

```json
{"environmentId":"env-shieldy-3f9a","label":"shieldy","platform":{"os":"linux","arch":"x64"},
 "serverVersion":"0.1.0","orchestrationProtocolVersion":2,
 "capabilities":{"repositoryIdentity":false,"attachmentUploads":true,"threadStop":true,
   "providerWorkspaceSnapshotsOnDemand":true}}
```

#### H2. `GET /ws`: MUST be real

- URL: the base path becomes `/ws` when the path is empty or `/` (`CR/connection/resolver.ts:83-86`). Query parameters:
  - `orchestrationProtocol=2`, always appended last.
  - Analytics parameters to ignore: `clientSurface=mobile`, `clientAppVersion`, `clientDeviceType`, `clientOs=iOS`, `clientOsMajorVersion`, `clientDeviceModel`, `connectionMethod=direct`, `clientOwner`, `clientDevice` (`CR/authorization/remote.ts:63-112`; `M/lib/authClientMetadata.ts:5-23`).
  - `wsTicket` is present only on bearer connections, so it is absent here.
- No subprotocol and no custom headers (`CR/rpc/session.ts:46,226-230`).
- Reference server behaviour: if `orchestrationProtocol != "2"`, reply HTTP 426 with `{"code":"orchestration_protocol_incompatible","message":…,"orchestrationProtocolVersion":2}` (`S/ws.ts:3348-3357`). Serialization is `RpcSerialization.layerJson` on both sides (`S/ws.ts:3405`; `CR/rpc/session.ts:231-247`).
- Keepalive: the client pings every 5 s and drops the socket after 15 s with no inbound frame (`CR/rpc/protocol.ts:10-12`). Any inbound frame resets the timer. See `effect-rpc.md` §Keepalive.

Example: `ws://100.64.0.7:3773/ws?clientSurface=mobile&clientOs=iOS&connectionMethod=direct&orchestrationProtocol=2`

#### H3. `GET /api/auth/session`: MUST answer, and a constant is fine

- Contract: `C/environmentHttp.ts:535-541`. Schema: `AuthSessionState` (`C/auth.ts:392-399`).
- Client: `CR/state/session.ts:46-78` (6 s timeout, retried 2 s→60 s).
- **Why it is required:** the client checks every guarded write against this response *locally, before sending anything*:
  - `CR/state/commandPermissions.ts:40-68` for every `createEnvironmentRpcCommand` (`CR/state/runtime.ts:983-1004`) whose method is listed in `C/clientRpcPermissions.ts:17-71`.
  - Uploads require `orchestration:operate` (`M/lib/attachmentUpload.ts:209-211,287-292`).
  - `filesystem.browse`, `projects.searchEntries`, `projects.readFile` and file-backed `assets.createUrl` require `filesystem:read` (`CR/state/filesystem.ts:27-55`; `CR/state/assets.ts:89-104`).

  Without these grants, adding a project and attaching images fail inside the app without ever reaching the server.
- Grant logic: `sessionGrantsScope` (`C/auth.ts:126-136`). `authenticated:false` grants nothing. When `permissions` is present it is authoritative.

```json
{"authenticated":true,
 "auth":{"policy":"unsafe-no-auth","bootstrapMethods":[],"sessionMethods":[],"sessionCookieName":"j3_session"},
 "scopes":["orchestration:read","orchestration:operate"],
 "permissions":["orchestration:read","orchestration:operate","filesystem:read","filesystem:write","settings:write"]}
```

- `auth` (`ServerAuthDescriptor`, `C/auth.ts:174-181`):
  - `policy` ∈ `desktop-managed-local|loopback-browser|remote-reachable|unsafe-no-auth` (`:12-17`)
  - `bootstrapMethods`, `sessionMethods`: arrays, may be empty
  - `sessionCookieName`: TrimmedNonEmptyString
- Scope literals are listed at `C/auth.ts:30-66`.

#### H4. `POST /api/attachments/upload/<token>`: MUST be real (images)

- The client gets the URL from WS `attachments.createUploadUrl` (§2, M10). It resolves `relativeUrl` against the HTTP base (`CR/state/assets.ts:59-65`; `M/lib/attachmentUpload.ts:344-352`).
- The upload is a **`POST`** with the raw file bytes as the body, `Content-Type: <mimeType>`, and no auth (`M/lib/attachmentUpload.ts:200-247`, expo `File.upload(..., {httpMethod:"POST", uploadType: BINARY_CONTENT})`). The client treats any 2xx as success.
- Reference server (`S/http.ts:463-500`; prefix `S/assets/AttachmentUpload.ts:41`):
  - checks `Content-Length == sizeBytes`, else 400
  - stores the bytes and replies **204**
  - replies 404 for an unknown token
- j3 may use any opaque token, because the client only resolves `relativeUrl`.
- The mobile app always takes this upload path. `supportsImageUploads: true` is hard-coded at `M/state/threadOutboxDelivery.ts:215` and `M/state/composer-attachment-uploads.ts:139`.

#### H5. `GET /api/assets/<token>/<fileName>`: MUST be real (image display)

- The client gets the URL from WS `assets.createUrl` (§2, M11). Images load by URL with no headers (`M/features/threads/ThreadMarkdownImage.tsx:49`, `M/lib/attachmentDocument.ts:50`).
- Reference server (`S/http.ts:405-461`, `S/assets/AssetAccess.ts:71,877`) replies with the bytes and these headers: `content-type`, `cache-control: private, max-age=3600`, `x-content-type-options: nosniff`. It supports `Range` and `If-Range`, and returns 404 for unknown tokens.

#### H6. `GET /api/orchestration/shell`: stub (501)

- Contract: `C/environmentHttp.ts:682-689`, success `OrchestrationV2ShellSnapshot` (`OV2:2207`). Client: `CR/state/shellSnapshotHttp.ts:27-60`.
- **On any failure the client returns `Option.none` and falls back to the socket** (`CR/state/shellSnapshotHttp.ts:108-126`). The cost is one extra round trip per connect.

#### H7. `GET /api/orchestration/threads/:threadId/bounded`: stub (501), with one trap

- Contract: `C/environmentHttp.ts:706-714`, success `OrchestrationV2ThreadBoundedSnapshot` (`OV2:3777-3793`). Client: `CR/state/boundedThreadSnapshotHttp.ts:20-125`.
- **Trap:** a decoded `EnvironmentResourceNotFoundError` body means "missing". The client then marks the thread deleted and never opens the socket (`CR/state/boundedThreadSnapshotHttp.ts:100-110`, `CR/state/threads.ts:999-1004`). Every other failure means "unavailable" and the socket snapshot is used (`:111-120`).
- Rule: reply `501` with a non-JSON body. Send the tagged NotFound only for a thread that really does not exist.
- If implemented, reply with `{snapshotSequence, projection, historyCursor:null, hasMoreHistory:false, latestLocalTurnOrdinal:null}`.

#### H8. `GET /api/orchestration/threads/:threadId/history`: stub (501)

- Contract: `C/environmentHttp.ts:715-722`. Client: `CR/state/threadHistoryHttp.ts:15-46`.
- The client calls it only when a snapshot advertised `historyCursor`/`hasMoreHistory`. j3's snapshots never do (§4.2), so the endpoint is never called.

#### Not needed once the client is edited

- `POST /oauth/token` (`C/environmentHttp.ts:549-556`)
- `POST /api/auth/websocket-ticket[/rpc]` (`:557-571`)

If the stock pairing flow is kept instead, stub them:
- `/oauth/token` → `{"access_token":"x","issued_token_type":"urn:ietf:params:oauth:token-type:access_token","token_type":"Bearer","expires_in":31536000,"scope":"orchestration:read orchestration:operate"}`
- the ticket endpoint → `{"ticket":"x","expiresAt":"2099-01-01T00:00:00.000Z"}`

The user then has to type a dummy pairing code (`packages/shared/src/remote.ts:234-237` rejects URLs with no token).

Everything else in `EnvironmentHttpApi` is unused by the trimmed app: transcript, `/api/projects*`, pull-request diff, connect, mcpOAuth, webhooks.

### 1.4 Classification

| # | Endpoint | Class |
|---|---|---|
| H1 | `GET /.well-known/t3/environment` | real |
| H2 | `GET /ws` | real |
| H3 | `GET /api/auth/session` | constant |
| H4 | `POST /api/attachments/upload/<token>` | real |
| H5 | `GET /api/assets/<token>/<name>` | real |
| H6 | `GET /api/orchestration/shell` | 501 |
| H7 | `GET /api/orchestration/threads/:id/bounded` | 501, never a tagged 404 |
| H8 | `GET /api/orchestration/threads/:id/history` | 501, never called |

### 1.5 Client-runtime edits: connect by host with no auth or pairing

The mobile **primary** connection path already runs without credentials. It provides `bearerToken = Option.none()` (`M/connection/platform.ts:186-192`). The broker then returns `{socketUrl, httpAuthorization: null}` without any auth calls (`CR/connection/resolver.ts:78-109`), and later HTTP goes out bare. What is missing is a way to *register* such a host. Recommended edits:

1. Widen `BearerConnectionProfile.authorization` to `Schema.optionalKey(Schema.Literals(["t3-connect","none"]))` (`CR/connection/catalog.ts:26`). Make `credential` optional in `BearerConnectionRegistration` (`:84-91`).
2. In the bearer broker (`CR/connection/resolver.ts:148-176`): when `profile.authorization === "none"`, return `{httpBaseUrl, socketUrl: primarySocketUrl(profile.wsBaseUrl, …), httpAuthorization: null}` before the credential lookup at `:155`. Reuse `primarySocketUrl` (`:78-89`). The descriptor and protocol checks at `:296-325` still run.
3. Add `registerDirect({host})` to `CR/connection/onboarding.ts:101-153`, and expose it at `:56-75,301-333`. It normalizes the host (`packages/shared/src/advertisedEndpoint.ts:23-45`), fetches the descriptor (`:106`), runs the compatibility check (`:136-140`), **skips** `bootstrapRemoteBearerSession` (`:141-145`), and saves a profile with `authorization:"none"`.
4. Mobile:
   - add a `connectDirectHost` command (`M/connection/onboarding.ts:13-29`) and call it from `M/state/use-remote-environment-registry.ts:53-62`
   - `M/features/connection/ConnectionsNewRouteScreen.tsx:154-160` takes a host only; drop the code field (`:73,236`) and the QR scan
   - point `M/features/connection/devAutoConnect.ts:46-71` at the new command
   - drop relay discovery from `M/features/connection/useConnectionController.ts:20,34,46,71-84`
5. Keep the H3 stub rather than synthesizing a session on the client. If you do want to synthesize it: `CR/state/session.ts:156-204` would return an all-scopes `AuthSessionState` when `httpAuthorization === null`.

An alternative with no schema change is to emit `PrimaryConnectionRegistration`s from a host list stored on the phone (`M/connection/platform.ts:242-247`). The registry does not persist these and cannot remove them (`CR/connection/registry.ts:616-623`; `CR/connection/model.ts:50-55`), which is why the edits above are recommended instead.

## 2. WS RPC methods

### 2.0 Envelope and failure rules

Request: `{"_tag":"Request","id":<number>,"tag":"<method>","payload":{…},"headers":[]}`.
Unary reply: `{"_tag":"Exit","requestId":<same id>,"exit":{"_tag":"Success","value":…}}`.
A stream sends `Chunk{requestId, values:[…]}` frames. **Each `Chunk` must be acked** (`Ack{requestId}`) before the next one is sent. Values produced while waiting are merged into the next batch.
See `effect-rpc.md` §Client→server, §Server→client, §Streams.

- **Unknown or unimplemented tags.** With effect 4.0.5, the server answers with a Die `Exit` on that request only (`effect-rpc.md` §Exit/Cause, fixture `raw-unknown-tag`). j3 must do the same. **It must never send a connection-level `Defect`.** A `Defect` fails every pending request and stream on the socket (`effect-rpc.md` §Server→client), including `subscribeServerConfig`. Losing that stream makes the client treat the session as dead and reconnect (`CR/rpc/session.ts:257-268,310-316`).
- **How the client treats failures:**
  - Unary failures go through `runAtomCommand` → `AsyncResult.failure`. A typed failure is logged as a warning; a defect is logged as an error (`CR/state/runtime.ts:450-466`). Screens show their own error state; nothing throws.
  - Durable subscriptions (`CR/rpc/client.ts:296-356`):
    - a typed `Fail` is retried with backoff (250 ms ×2ⁿ, at most 30 s)
    - a `Die` fails the stream until the next session or foreground wake. The thread view shows "Could not synchronize the thread." (`CR/state/threads.ts:1021`)
    - a transport error waits for the next session
  - So subscription errors should be **typed** (`OrchestrationV2GetThreadProjectionError` and similar), not defects.
- **`EnvironmentAuthorizationError`** (`C/auth.ts:340-347`) is in almost every method's error union. j3 never emits it.

### 2.1 Method table

Tier **R** = required. Tier **S** = cheap stub that must succeed or fail cleanly. Tier **G** = gated: the client never calls it while j3 leaves the gating flag unset or the trim removes the UI. Answer a G method with a per-request failure if it ever arrives.

| # | Tag | Kind | Tier | Rpc def | Screen / caller |
|---|---|---|---|---|---|
| M1 | `subscribeServerConfig` | stream | R | `C/rpc.ts:1862-1875` | handshake; composer models, thinking, slash commands, skills (`CR/rpc/session.ts:269`; `CR/state/server.ts:485,782`) |
| M2 | `server.probe` | unary | R | `C/rpc.ts:627-631` | liveness (`CR/rpc/session.ts:407-413`) |
| M3 | `server.refreshProviders` | unary | R | `C/rpc.ts:639-659` | composer slash menu and skills per cwd (`M/features/threads/use-composer-command-menu.ts:66-69`; `CR/providerSkills.ts:270-335`); model refresh (`M/features/threads/ThreadSettingsSheet.tsx:931`) |
| M4 | `orchestration.subscribeShell` | stream | R | `C/rpc.ts:1831-1836` | home, project list, thread list (`CR/state/shell.ts:362-440`) |
| M5 | `orchestration.subscribeThread` | stream | R | `C/rpc.ts:1838-1843` | thread view (`CR/state/threads.ts:937-1027`) |
| M6 | `orchestration.dispatchCommand` | unary | R | `C/rpc.ts:1713-1717` | every thread mutation (§3) (`CR/operations/commands.ts:277-278`) |
| M7 | `orchestration.launchThread` | unary | R | `C/rpc.ts:1825-1829` | new thread plus its first message (`CR/operations/commands.ts:685-731`; `M/lib/projectThreadStartTurn.ts:40-86`) |
| M8 | `orchestration.getThreadProjection` | unary | R | `C/rpc.ts:1789-1796` | Stop fallback when no runId is known; subagent stop (`CR/operations/commands.ts:280-281,788-863`) |
| M9 | `orchestration.getArchivedShellSnapshot` | unary | R | `C/rpc.ts:1756-1763` | archive screen; refreshed after every archive (`M/features/archive/useArchivedThreadSnapshots.ts:14`; `M/features/home/useThreadListActions.ts:160-162`) |
| M10 | `attachments.createUploadUrl` | unary | R | `C/rpc.ts:1396-1400` | composer images (`CR/state/attachments.ts:24-27`; `M/lib/attachmentUpload.ts:339`) |
| M11 | `assets.createUrl` | unary | R | `C/rpc.ts:1384-1388` | image display, upload verification, favicons (`CR/state/assets.ts:132-265`; `M/lib/attachmentUpload.ts:316-334`) |
| M12 | `projects.mutate` | unary | R | `C/rpc.ts:1326-1330` | add, rename, delete project (`CR/operations/commands.ts:311-394`; `M/features/projects/AddProjectScreen.tsx:715,765-773`; `M/features/settings/SettingsProjectOverviewRouteScreen.tsx:155-160,252-255,295-298`) |
| M13 | `filesystem.browse` | unary | R | `C/rpc.ts:1361-1365` | add project: remote directory listing (`CR/state/filesystem.ts:150-156`; `M/features/projects/AddProjectScreen.tsx:317,911-930`) |
| M14 | `server.reportClientActivity` | unary, void | S | `C/rpc.ts:990-993` | presence; failures are ignored (`M/connection/background-activity.ts:53-85`) |
| M15 | `attachments.delete` | unary, void | S | `C/rpc.ts:1402-1405` | cleanup of abandoned uploads (`M/lib/attachmentUpload.ts:76,340`) |
| M16 | `orchestration.getTurnItem` | unary | S | `C/rpc.ts:1819-1823` | expanded tool row; post-Stop reconcile, which tolerates failure (`CR/state/orchestration.ts:114-119`; `CR/state/threads.ts:833-839`) |
| M17 | `orchestration.searchThreads` | unary | S | `C/rpc.ts:1750-1754` | thread search (`M/state/queries.ts:76-80`) |
| M18 | `orchestration.forceStopThread` | unary | S | `C/rpc.ts:1804-1808` | "Force stop" button (`M/features/threads/ThreadDetailScreen.tsx:562-566`) |
| M19 | `projects.readFile` | unary | S | `C/rpc.ts:1314-1318` | reads `t3.json` on every new task (`M/features/threads/use-new-task-project-defaults.ts:38-48`) |
| M20 | `projects.searchEntries` | unary | S | `C/rpc.ts:1290-1294` | composer `@path` search (`M/state/queries.ts:426`) |
| M21 | `filesystem.getMetadata` | unary | S | `C/rpc.ts:1355-1359` | file-link chips in messages (`M/features/threads/ThreadFeed.tsx:2831,2898`) |
| M22 | `server.updateSettings` | unary | S | `C/rpc.ts:821+` | one-time model-favorites migration; failure is only logged (`M/features/threads/ThreadSettingsSheet.tsx:370`) |
| G1 | `orchestration.subscribeCommandOutput` | stream | G | `C/rpc.ts:1845-1852` | only with `capabilities.commandOutputStreaming` (`M/features/threads/thread-command-output.tsx:15-31`) |
| G2 | `orchestration.getOlderSettledThreads` | unary | G | `C/rpc.ts:1780-1787` | only with `ServerConfig.shellOlderSettled && shellThreadFamilies` (`CR/state/shell.ts:366-371`) |
| G3 | `orchestration.getThreadFamily` | unary | G | `C/rpc.ts:1765-1769` | only with `shellThreadFamilies` (`CR/state/threadFamilies.ts:107-110`) |
| G4 | `orchestration.stopWorkflow` | unary | G | `C/rpc.ts` (OV2 schema `:4167`) | subagent stop button (`M/features/threads/SubagentStopButton.tsx:23-25`); j3 never emits subagents |
| G5 | `projects.ensureScratch` | unary | G | `C/rpc.ts:1332-1336` | "scratch" new task; shown only when the server sets `scratchWorkspaceRoot` [INFERENCE] |
| G6 | `projects.createNew` | unary | G | `C/rpc.ts:1338-1342` | "new empty project"; needs `newProjectsRoot` (`M/features/projects/AddProjectScreen.tsx:1032-1035`) |
| G7 | `projects.writeFile` | unary | G | `C/rpc.ts` (`WsProjectsWriteFileRpc`) | "save plan" on `ProposedPlanCard`; j3 never emits `proposed_plan` |
| G8 | `assets.persistChatAttachments` | unary | G | `C/rpc.ts:1390-1394` | inline `dataUrl` images; only when `supportsImageUploads` is false (`CR/operations/commands.ts:283-309`) |
| G9 | `secrets.answerRequest` | unary | G | `C/rpc.ts` (`WsSecretsAnswerRequestRpc`) | `SecretRequestCard`; j3 never emits secret requests |
| G10 | `server.getConfig` | unary | G | `C/rpc.ts:633-637` | never called (`CR/rpc/session.ts:77` uses only its type). It is trivial to answer with the snapshot config. |

### 2.2 Per-method shapes

#### M1 `subscribeServerConfig` (stream; error `KeybindingsConfigError | ServerSettingsError | EnvironmentAuthorizationError`)

- Payload: all keys `Schema.optional(Boolean)`: `environmentThemes`, `usageLimitSources`, `usageLimitsCommand`, `workspaceSnapshotsOnDemand`.
- Item `ServerConfigStreamEvent` (`C/server.ts:723-790`). Every item carries `version: 1` and a `type`:
  - `snapshot{config}`: **required, first item**
  - `providerStatuses{payload:{providers}}`: optional; use it to push a new model list
  - `keybindingsUpdated`, `settingsUpdated`, `environmentThemesUpdated`, `usageLimitSourcesUpdated`: j3 never sends these
- **Keep the stream open for the whole life of the socket.** If it ends, the client reports "config subscription ended." and drops the session (`CR/rpc/session.ts:257-268,310-316`).
- `ServerConfig` (`C/server.ts:609-662`):
  - Required:
    - `environment`: the same object as H1
    - `auth`: as in H3
    - `cwd`: TrimmedNonEmptyString
    - `keybindingsConfigPath`: TrimmedNonEmptyString
    - `keybindings`: `[]`
    - `issues`: `[]`
    - `providers`: `ServerProvider[]`
    - `availableEditors`: `[]`
    - `observability`: `{logsDirectoryPath, localTracingEnabled, otlpTracesEnabled, otlpMetricsEnabled}` (`C/server.ts:283-298`)
    - `settings`: `{}` decodes, because every field has a decoding default (`C/settings.ts:1181-1364`)
  - optionalKey flags: **leave all of them unset**. That covers `shellThreadFamilies`, `shellOlderSettled`, `threadFind`, `threadFindProgressive`, `threadSnapshotPagination`, `scratchWorkspaceRoot` and `newProjectsRoot`.
- `ServerProvider` (`C/server.ts:206-268`):
  - Required:
    - `instanceId`: ProviderInstanceId
    - `driver`: ProviderDriverKind (known values `C/model.ts:121-129`)
    - `enabled`, `installed`: boolean
    - `version`: string|null
    - `status`: `ready|warning|error|disabled`
    - `auth`: `{status:"authenticated"|"unauthenticated"|"unknown"}`
    - `checkedAt`: IsoDateTime
    - `models`: ServerProviderModel[]
  - Defaulted:
    - `slashCommands` (`[]`): `{name, description?, input?{hint}}` (`:102-107`)
    - `skills` (`[]`): `{name, path, enabled, description?, scope?, displayName?, shortDescription?, userInvocationOnly?, userInvocable?}` (`:109-121`)
  - Optional: `workspaceSnapshots: [{cwd, checkedAt, slashCommands, slashCommandsPending?, skills, optionDescriptors?}]` (`:124-131`), plus many display fields.
- `ServerProviderModel` (`C/server.ts:75-86`):
  - Required: `slug`, `name`, `isCustom`, `capabilities: ModelCapabilities|null` (**the key is required**)
  - Optional: `shortName`, `subProvider`, `isDefault`, …
  - `ModelCapabilities = {optionDescriptors?: (select|boolean)[]}` (`C/model.ts:7-46,102-104`)
- **Model picker:** `buildModelOptions` (`M/lib/modelOptions.ts:238-417`). It skips providers that are disabled, not installed, or have `auth.status:"unauthenticated"` (`:298-307`). A selection is `{instanceId, model: slug, options?}` (`:344-350`). The outbox refuses to send a selection that is not in the config (`M/state/threadOutboxDelivery.ts:777-783,854-860`).
- **Thinking picker:** a `select` descriptor in `capabilities.optionDescriptors` (`M/components/ComposerModelControl.tsx:24-44`; `packages/shared/src/model.ts:199-211`). The chosen value travels as `modelSelection.options: [{id, value}]`.
- **Slash menu:**
  - Built-ins `/model`, `/plan` and `/default` are client-side (`M/features/threads/use-composer-command-menu.ts:107-146`).
  - Provider commands come from `provider.slashCommands` or `workspaceSnapshots[cwd].slashCommands`. They are inserted into the text as `/<name> ` (`:198-200`).
  - Skills come from `skills` and are inserted as `$<name> ` (`:196`).

j3 recommendation: one provider, `instanceId:"omp"`, `driver:"pi"`. Pi is omp's upstream and already has a thinking descriptor of the right shape (`PI/thinkingCapabilities.ts:24-44`). [INFERENCE: the client has no omp driver; `pi` is the closest match.]

```json
{"version":1,"type":"snapshot","config":{
 "environment":{"environmentId":"env-shieldy-3f9a","label":"shieldy","platform":{"os":"linux","arch":"x64"},"serverVersion":"0.1.0","orchestrationProtocolVersion":2,"capabilities":{"repositoryIdentity":false,"attachmentUploads":true,"threadStop":true,"providerWorkspaceSnapshotsOnDemand":true}},
 "auth":{"policy":"unsafe-no-auth","bootstrapMethods":[],"sessionMethods":[],"sessionCookieName":"j3_session"},
 "cwd":"/home/jacob","keybindingsConfigPath":"/home/jacob/.j3/keybindings.json","keybindings":[],"issues":[],
 "providers":[{"instanceId":"omp","driver":"pi","displayName":"omp","enabled":true,"installed":true,"version":"14.2.0",
   "status":"ready","auth":{"status":"authenticated"},"checkedAt":"2026-11-01T12:00:00.000Z",
   "models":[{"slug":"anthropic/claude-sonnet-4-5","name":"Claude Sonnet 4.5","isCustom":false,"isDefault":true,
     "capabilities":{"optionDescriptors":[{"id":"thinking","label":"Thinking","type":"select",
       "options":[{"id":"off","label":"Off"},{"id":"low","label":"Low"},{"id":"medium","label":"Medium","isDefault":true},{"id":"high","label":"High"}]}]}}],
   "slashCommands":[],"skills":[]}],
 "availableEditors":[],
 "observability":{"logsDirectoryPath":"/home/jacob/.j3/logs","localTracingEnabled":false,"otlpTracesEnabled":false,"otlpMetricsEnabled":false},
 "settings":{}}}
```

#### M2 `server.probe`

Payload `{}`, success `{}`. Answer quickly; the probe has a 3 s or 15 s timeout.

#### M3 `server.refreshProviders` (error `EnvironmentAuthorizationError | ProviderSetupError`)

- Payload: all `Schema.optional`. `instanceId`, `cwd`, `fresh`, `refreshModels`, `reloadSecrets`, `workspaceSnapshotsOnDemand`, `usageLimitsCommand`.
- Success: `ServerProviderUpdatedPayload = {providers: ServerProvider[]}` (`C/server.ts:859-861`).
- Client behaviour: the composer calls it with `{instanceId, cwd}` whenever the selected provider has no complete `workspaceSnapshots` entry for the thread's cwd (`CR/providerSkills.ts:167-197,325-329`). It passes `reportFailure:false` and retries with backoff. The snapshots are merged only when `capabilities.providerWorkspaceSnapshotsOnDemand` is set (`CR/state/server.ts:685-709`).

```json
{"instanceId":"omp","cwd":"/home/jacob/Developer/j3code"}
→ {"providers":[{ "...":"as M1", "workspaceSnapshots":[{"cwd":"/home/jacob/Developer/j3code","checkedAt":"2026-11-01T12:00:00.000Z",
   "slashCommands":[{"name":"compact","description":"Compact the context","input":{"hint":"[focus]"}}],
   "skills":[{"name":"jev","path":"/home/jacob/.omp/agent/skills/jev/SKILL.md","enabled":true,"description":"Calibrated judgments"}]}]}]}
```

#### M4, M5: see §4.

#### M6, M7: see §3.

#### M8 `orchestration.getThreadProjection` (error `OrchestrationV2GetThreadProjectionError {threadId, message, cause?}`, `OV2:3895-3902`)

- Payload `{threadId}` (`OV2:3643-3645`). Success: `OrchestrationV2ThreadProjection` (§4.3).
- The client reads `runs[].{id,status}`, `subagents`, `providerThreads`, `turnItems` and `thread.activeProviderThreadId` (`CR/operations/commands.ts:788-863`).

#### M9 `orchestration.getArchivedShellSnapshot` (error `OrchestrationV2GetShellSnapshotError {message, cause?}`, `OV2:3904-3910`)

- Payload `{}`.
- Success `{schemaVersion: PositiveInt, snapshotSequence, projects: OrchestrationProjectShell[], threads: OrchestrationV2ThreadShell[]}` (`OV2:3598-3603`). `threads` holds the archived threads.

```json
{} → {"schemaVersion":1,"snapshotSequence":1842,"projects":[…],"threads":[{"…ThreadShell…","archivedAt":"2026-10-30T09:00:00.000Z"}]}
```

#### M10 `attachments.createUploadUrl` (error `AttachmentUploadSigningKeyError{cause}`, `C/assets.ts:128-137`)

- Payload union (`C/assets.ts:90-113`):
  - image: `{type?:"image" (optionalKey), name ≤255, mimeType ∈ PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES, sizeBytes 1..MAX_IMAGE}`
  - file: `{type:"file", name, mimeType ≤100, sizeBytes 1..MAX_FILE}`
- Success `{attachmentId ≤256, relativeUrl ≤4096, expiresAt: number (epoch ms)}` (`:116-120`).
- The client then POSTs the bytes (H4) and refers to the upload in commands as `{type:"image", id: attachmentId, name, mimeType, sizeBytes}` (`M/lib/attachmentUpload.ts:138-155`).

```json
{"type":"image","name":"IMG_0001.jpg","mimeType":"image/jpeg","sizeBytes":183022}
→ {"attachmentId":"att_9k2m","relativeUrl":"/api/attachments/upload/att_9k2m.sig","expiresAt":1793534400000}
```

#### M11 `assets.createUrl` (error `AssetAccessError` union, `C/assets.ts:310-325`)

- Payload `{resource: AssetResource}`, tagged by `_tag` (`C/assets.ts:20-67`). j3 needs these variants:
  - `{_tag:"attachment", attachmentId, fileName?, mimeType?, disposition?:"inline"|"attachment"}`
  - `{_tag:"project-favicon", cwd, path?}`
- Success `{relativeUrl, expiresAt: number, sourcePath?, imageDimensions?:{width,height}, fileRevision?}` (`:76-85`).
- An unknown attachment must fail with `AssetAttachmentNotFoundError`. The client then re-uploads (`CR/state/attachments.ts:40-47`).
- Favicon: return a `relativeUrl` whose last segment is `project-favicon-missing` (`packages/shared/src/projectFavicon.ts:3,35-44`), or fail. Either way the UI shows no icon.

```json
{"resource":{"_tag":"attachment","attachmentId":"att_9k2m","mimeType":"image/jpeg","disposition":"inline"}}
→ {"relativeUrl":"/api/assets/tok_ab12/IMG_0001.jpg","expiresAt":1793534400000}
```

#### M12 `projects.mutate` (error `ProjectMutationError{commandId, message, cause?}`, `C/project.ts:227-234`)

- Payload `ProjectMutation` (`C/project.ts:179-225`), tagged by `type`. The client generates `projectId`.
  - `{type:"project.create", commandId, projectId, title, workspaceRoot, createWorkspaceRootIfMissing?, defaultModelSelection?: ModelSelection|null, scripts?}`
  - `{type:"project.update", commandId, projectId, title?, workspaceRoot?, defaultModelSelection?, autoPull?, projectIcon?, faviconPath?, defaultThreadEnvMode?, scripts?, pinned?, archived?}`
  - `{type:"project.delete", commandId, projectId, force?}`
- Success `Project` (`C/project.ts:152-168`):
  - Required: `id`, `title`, `workspaceRoot`, `defaultModelSelection`(NullOr), `scripts`, `createdAt`, `updatedAt`, `deletedAt`(NullOr)
  - Optional: `repositoryIdentity`, `faviconPath`, `projectIcon`, `defaultThreadEnvMode`, `autoPull`, `pinnedAt`, `archivedAt`
- After success, the server must also emit `project.updated` (or `project.removed`) on the shell stream (§4.1). That is how the project list updates.

```json
{"type":"project.create","commandId":"0b1e…","projectId":"8c7d…","title":"j3code","workspaceRoot":"/home/jacob/Developer/j3code"}
→ {"id":"8c7d…","title":"j3code","workspaceRoot":"/home/jacob/Developer/j3code","defaultModelSelection":null,"scripts":[],"createdAt":"2026-11-01T12:00:00.000Z","updatedAt":"2026-11-01T12:00:00.000Z","deletedAt":null}
```

#### M13 `filesystem.browse` (error `FilesystemBrowseError{message, partialPath?, cwd?, failure?, parentPath?, platform?, cause?}`, `C/filesystem.ts:58-90`)

- Payload `{partialPath ≤512, cwd? ≤512}` (`C/filesystem.ts:29-32`).
- Success `{parentPath, entries:[{name, fullPath}]}` (`:41-44`).
- The client sends the directory part of the typed path and filters by the leaf segment itself (`M/features/projects/AddProjectScreen.tsx:911-930`). The query has a 5 s idle TTL. Expand `~`, and list directories only.

```json
{"partialPath":"~/Developer/"} → {"parentPath":"/home/jacob/Developer","entries":[{"name":"j3code","fullPath":"/home/jacob/Developer/j3code"}]}
```

#### Stubs M14–M22

| # | Payload | Answer |
|---|---|---|
| M14 | `ClientActivityReportInput` (`C/background.ts:67-80`): `clientId`, `clientKind`, `visible`, `focused`, `recentlyInteracted`, `scopes[]`, `observedAt` (ISO), optional `environmentId`, `appState`, `lowPowerMode`, `ttlMs` | void success (`Exit` `Success` with value `null`) |
| M15 | `{attachmentId}` (`C/assets.ts:122-125`) | void success; an unknown id also counts as success |
| M16 | `{threadId, itemId, revision?}` (`OV2:4034-4039`) | `{"item": <TurnItem>}` or `{"item":null}` (`OV2:4041-4043`) |
| M17 | `{query (2..200), limit?, olderSettledBefore?, projectIds?, environmentLabel?, titlesOnly?}` (`OV2:3693-3702`; `C/threadSearch.ts:20-26`) | `{"matches":[]}`, or title matches `{threadId, projectId, source:"user", snippet, messageCreatedAt}` (`C/threadSearch.ts:28-47`) |
| M18 | `{threadId, runId?}` (`OV2:3951-3955`) | `{"outcome":"killed","sharedWithThreadIds":[]}` (`OV2:3958-3967`); kill the omp process |
| M19 | `{cwd, relativePath, allowMissing?}` (`C/project.ts:433-439`) | `{"relativePath":"t3.json","contents":"","byteLength":0,"truncated":false,"exists":false}` (`:442-449`) |
| M20 | `{cwd, query, limit, kind?, imageOnly?}` | `{"entries":[{"path":"src/main.rs","kind":"file"}],"truncated":false}` (`C/project.ts:239-262`); `{"entries":[],"truncated":false}` is fine |
| M21 | `{paths: string[1..64]}` (`C/filesystem.ts:8-27`) | `{"entries":[null,…]}` (one `null` per path) or `{kind, byteLength?, mimeType?}` |
| M22 | settings patch | typed failure `ServerSettingsError`, or echo the settings |

## 3. Commands: `orchestration.dispatchCommand` and `orchestration.launchThread`

### 3.1 Envelope

- `dispatchCommand` takes **the command object itself** as the payload, with no wrapper. The payload schema is `OrchestrationV2Command` (`OV2:2877-3347`), discriminated by `type`.
- Success `{sequence: NonNegativeInt}` (`OV2:3638-3640`).
- Error `OrchestrationV2DispatchCommandError {commandId, commandType: string, message: string, detail?: string, cause?}` (`OV2:3884-3893`). The reference wrapper is `S/ws.ts:1910-1959`.
- **Idempotency.** The outbox retries transport failures with the *same* `commandId` (`M/state/threadOutboxDelivery.ts:666-…`). The server must dedupe by `commandId` and replay the original `{sequence}` (reference: `S/orchestration-v2/Orchestrator.ts:13263-13318`; receipt schema `S/orchestration-v2/CommandReceiptStore.ts:40-63`).
- **What `sequence` means.** It must be in the same number space as the stream sequences (§4.1). It must also be ≤ the sequence of the event that makes the write visible:
  - The client keeps an optimistic user bubble until `thread.sequence >= result.sequence`, or until a projection message has `id == messageId` (`CR/state/threadState.ts:34-43`; `M/state/acknowledged-thread-messages.ts:46-52`).
  - Shell overlays work the same way, using `shell.snapshotSequence >= result.sequence` (`CR/state/threadLifecycle.ts:33,80-90`).
  - `{sequence:0}` means "nothing happened" and is always safe (`CR/operations/commands.ts:644,805,869`).
- **Error text.** The mobile outbox restores a draft only for the legacy tag `OrchestrationDispatchCommandError`, plus messages that look like transport errors (`M/state/thread-outbox-model.ts:255-289`). A V2 rejection whose `message` *looks like* a network error is retried forever. Keep rejection messages human-readable and avoid words like "connection" or "timeout".

### 3.2 Nested schemas

- **`ModelSelection`** (`C/modelSelection.ts:9-51`): `{instanceId: ProviderInstanceId, model: TrimmedNonEmptyString, options?: ProviderOptionSelections}`.
  - The decoder also accepts the legacy key `provider` in place of `instanceId`.
  - `options` is either `[{id, value: string|boolean}]` or a legacy object `{id: value}` (`C/model.ts:48-100`). **Accept both.** [INFERENCE: the client emits the array, because the array member comes first in the union.]
  - The thinking level is the option whose id matches the descriptor j3 advertises, e.g. `{"id":"thinking","value":"high"}`.
- **`ChatAttachment`** (`C/chatAttachment.ts:148-205`), a union:
  - image `{type:"image", id: ChatAttachmentId, name ≤255, mimeType /^image\//, sizeBytes ≤10 MiB, source?}`
  - file `{type:"file", id, name, mimeType, sizeBytes 1..50 MiB, source?}`
  - unknown `{type:<other>, id, name, mimeType, sizeBytes}`

  Commands carry only references. The bytes arrive earlier through H4.
- **`OrchestrationMessageContext`** (`C/composerContext.ts:284-304`): `{version:1, guest?, records: unknown[]}` (forward-compatible). Store it as-is and echo it on the user message.
- **Enums:**
  - `RuntimeMode` = `approval-required|auto-accept-edits|auto|full-access`
  - `ProviderInteractionMode` = `default|plan` (`C/providerPolicy.ts:25-36`)
  - `OrchestrationV2CreationSource` = `web|mobile|mcp|provider|server` (`OV2:91-97`)
  - `OrchestrationV2Actor` = `user|agent|system` (`OV2:88`)

### 3.3 `orchestration.launchThread`: create a thread with its first message

The mobile app never dispatches `thread.create`. Every new thread goes through `launchThread`. The chain is `M/lib/projectThreadStartTurn.ts:40-86` → `startThreadTurn` (`CR/operations/commands.ts:670-731`), called from the outbox (`M/state/threadOutboxDelivery.ts:1030-1058`).

- Input `OrchestrationV2ThreadLaunchInput` (`OV2:3606-3626`):

| field | type | opt |
|---|---|---|
| `commandId` | CommandId | req |
| `creationSource` | CreationSource | optional; mobile sends `"mobile"` |
| `threadId` | ThreadId | optional; mobile always sends a client UUID |
| `reuseExistingThread` | boolean | optional |
| `projectId` | ProjectId | req |
| `title` | TrimmedNonEmptyString | req; derived from the prompt, at most 72 chars (`M/lib/projectThreadStartTurn.ts:88-91`) |
| `generateTitle` | boolean | optional |
| `modelSelection` | ModelSelection | req |
| `runtimeMode` | RuntimeMode | req |
| `interactionMode` | ProviderInteractionMode | req |
| `workspaceStrategy` | `{type:"root", branch?}` \| `{type:"existing_worktree", …}` \| `{type:"worktree", …}` (`OV2:649-665`) | req; the trimmed app sends `{"type":"root"}`, and j3 rejects the others |
| `initialMessage` | `{messageId?, text, context?, attachments: ChatAttachment[]}` | optional; mobile always sends it |

- Success `OrchestrationV2ThreadLaunchResult` (`OV2:3629-3635`):
  - `threadId`
  - **`projection`: the full `OrchestrationV2ThreadProjection` (§4.3), required**
  - `resumed: boolean` (true when this is a replay of an already-launched `commandId`)
  - `sequence?` (optionalKey; recommended)
- Error `OrchestrationV2ThreadLaunchError {commandId, projectId, message, cause?}` (`OV2:3912-3920`).

```json
{"commandId":"6f1c2a4e-…","creationSource":"mobile","threadId":"1d3e5c77-…","projectId":"8c7d…",
 "title":"Fix the flaky login test","generateTitle":true,
 "modelSelection":{"instanceId":"omp","model":"anthropic/claude-sonnet-4-5","options":[{"id":"thinking","value":"high"}]},
 "runtimeMode":"full-access","interactionMode":"default","workspaceStrategy":{"type":"root"},
 "initialMessage":{"messageId":"a9e0b0d2-…","text":"Fix the flaky login test",
   "attachments":[{"type":"image","id":"att_9k2m","name":"shot.png","mimeType":"image/png","sizeBytes":183422}]}}
→ {"threadId":"1d3e5c77-…","projection":{…},"resumed":false,"sequence":1843}
```

### 3.4 Required `dispatchCommand` variants (17)

Every variant carries `type` and `commandId`. The user message the server projects **must use the client's `messageId` as its `id`**, and the run created for it must have `userMessageId = messageId`. Otherwise the optimistic bubble never reconciles (`M/features/threads/pending-thread-feed.ts:121-159`).

| # | `type` | Def | Fields (beyond `type`, `commandId`) | Sent by |
|---|---|---|---|---|
| D1 | `message.dispatch` | `OV2:3121-3161` | `createdBy`(req), `creationSource`(req), `threadId`, `messageId`, `text`, `attachments`(req, may be `[]`), `context?`, `titleSeed?`, `modelSelection?`, `deliveryIntent?: "auto"\|"steer"\|"restart"`, `dispatchMode`(req): `{type:"start_immediately"}` \| `{type:"queue_after_active"}` \| `{type:"steer_active",targetRunId}` \| `{type:"restart_active",targetRunId}` \| `{type:"defer_start",workspaceStrategy?}`. Cut-feature optionals: `sourcePlanRef`, `notification`, `scheduledTaskId`, `idleCompaction`, `*ContinuationOfRunId`, … | composer/outbox (`CR/operations/commands.ts:733-781`; `M/state/use-thread-composer-state.ts:642-681`) |
| D2 | `run.interrupt` | `OV2:3201-3208` | `threadId`, `runId`, `reason?`, `holdQueue?` (the client always sends `true`) | Stop (`M/features/threads/ThreadRouteScreen.tsx:688-703`; `CR/operations/commands.ts:870-876`) |
| D3 | `thread.stop` | `OV2:2878-2888` | `threadId`, `reason?`, `stopDelegatedTasks?` | Stop with no runId known, when `capabilities.threadStop` is set (`CR/operations/commands.ts:818-828`) |
| D4 | `thread.archive` | `OV2:2916-2920` | `threadId` | list swipe (`M/features/home/useThreadListActions.ts:86-171`); the client refuses while the thread is running (`:120-126`) |
| D5 | `thread.unarchive` | `OV2:2932-2936` | `threadId` | archive screen |
| D6 | `thread.delete` | `OV2:2937-2943` | `threadId`, `onlyIfUnused?` | list (`M/features/home/useThreadListActions.ts:150-153,174-190`) |
| D7 | `thread.metadata.update` | `OV2:3020-3035` | `threadId`, `title?`, `regenerateTitle?`; git/PR/limit fields are cut | rename (`M/features/home/useThreadListActions.ts:541-545`; `CR/operations/commands.ts:609-646`) |
| D8 | `thread.runtime-mode.set` | `OV2:3096-3101` | `threadId`, `runtimeMode` | outbox, before D1, when the mode differs; the commandId is `<msg>:runtime-mode` (`M/state/threadOutboxDelivery.ts:786-818`) |
| D9 | `thread.interaction-mode.set` | `OV2:3102-3107` | `threadId`, `interactionMode` | same |
| D10 | `runtime-request.respond` | `OV2:3250-3259` | `threadId`, `requestId`, then either `decision?: "accept"\|"acceptForSession"\|"acceptAlways"\|"decline"\|"cancel"` + `feedback?`, or `answers?: Record<questionId, string\|string[]>` + `attachmentsByQuestionId?` | `M/features/threads/use-selected-thread-requests.ts:167-174`; `CR/operations/commands.ts:879-905`; answer shape `CR/state/pendingUserInput.ts:17,161-176` |
| D11 | `thread.user-input.dismiss` | `OV2:3260-3265` | `threadId`, `requestId` | `M/features/threads/use-selected-thread-requests.ts:175-178` |
| D12 | `queued-run.cancel` | `OV2:3234-3239` | `threadId`, `runId` | queue sheet (`M/features/threads/ThreadQueueControl.tsx:136`) |
| D13 | `queued-run.edit` | `OV2:3240-3249` | `threadId`, `runId`, `text`, `attachments?` (absent keeps the existing ones; present replaces all), `context?` | `M/state/use-thread-composer-state.ts:206-209,465-470` |
| D14 | `queued-message.promote-to-steer` | `OV2:3209-3215` | `threadId`, `queuedRunId`, `targetRunId` | `M/features/threads/ThreadQueueControl.tsx:135` |
| D15 | `queue.resume` | `OV2:3216-3220` | `threadId` | "Resume" after a held queue (`CR/operations/commands.ts:1025-1035`) |
| D16 | `queued-run.start` | `OV2:3221-3226` | `threadId`, `runId` | "send now" on one queued run |
| D17 | `thread.visit` | `OV2:3005-3014` | `threadId`, `visitedAt: IsoDateTime`, `opened?`, `searchQuery? ≤200` | fires automatically when a thread opens (`M/features/threads/ThreadDetailScreen.tsx:342-345`). **It must succeed**; a no-op that returns the current sequence is enough. |

How the composer's mode maps to D1 (`CR/operations/commands.ts:733-781`):

| Composer mode | `dispatchMode` | `deliveryIntent` | Other |
|---|---|---|---|
| `"start"` (outbox default) | `{type:"start_immediately"}` | none | `titleSeed` is dropped |
| `"queue"` | `{type:"queue_after_active"}` | none | |
| steer while running | `{type:"start_immediately"}` | `"auto"` | (`M/state/use-thread-composer-state.ts:643`) |

The server resolves the effective mode (reference: `S/orchestration-v2/CommandPolicy.ts:92-182`):
- No `deliveryIntent`: use `dispatchMode` as sent.
- No active run (status `preparing|starting|running|waiting`): `start_immediately`.
- `"steer"` → `steer_active` on the latest active run. `"restart"` → `restart_active`.
- `"auto"`:
  - If the run is preparing or starting, or has no running provider turn for its active attempt: `queue_after_active`.
  - Otherwise use the first one the session supports: `supportsActiveSteering` → steer; `supportsQueuedMessages` → queue; `supportsSteeringByInterruptRestart` → restart. If none apply: queue.

For j3, whose session reports `supportsActiveSteering:true`: `auto` with a running turn → steer, and with nothing running → start.

```json
{"type":"message.dispatch","commandId":"0c6b…","createdBy":"user","creationSource":"mobile","threadId":"1d3e…","messageId":"e2f4…",
 "text":"Now add a regression test","attachments":[],"modelSelection":{"instanceId":"omp","model":"anthropic/claude-sonnet-4-5","options":[{"id":"thinking","value":"high"}]},
 "dispatchMode":{"type":"start_immediately"}}
{"type":"message.dispatch","commandId":"…","createdBy":"user","creationSource":"mobile","threadId":"…","messageId":"…","text":"use vitest","attachments":[],
 "titleSeed":"use vitest","deliveryIntent":"auto","dispatchMode":{"type":"start_immediately"}}
{"type":"run.interrupt","commandId":"…","threadId":"…","runId":"run_17","holdQueue":true}
{"type":"thread.metadata.update","commandId":"…","threadId":"…","title":"Login test fix"}
{"type":"runtime-request.respond","commandId":"…","threadId":"…","requestId":"rq_10","answers":{"db":"SQLite","features":["Auth","Search"]}}
{"type":"queued-message.promote-to-steer","commandId":"…","threadId":"…","queuedRunId":"run_18","targetRunId":"run_17"}
```

### 3.5 No-op accepts (9)

The trim should remove the list UI for these (§6). Until then, return the current sequence (or `{sequence:0}`) without changing anything:

| `type` | Def | Fields |
|---|---|---|
| `thread.settle` | `OV2:2945` | `threadId` |
| `thread.unsettle` | `OV2:2959` | `threadId`, `reason:"user"` |
| `thread.snooze` | `OV2:2965` | `threadId`, `snoozedUntil` |
| `thread.unsnooze` | `OV2:2971` | `threadId`, `reason` |
| `thread.auto-settle.set` | `OV2:2977` | `enabled` |
| `thread.pin` | `OV2:2983` | `threadId`, `orderKey?` |
| `thread.unpin` | `OV2:2989` | `threadId` |
| `thread.pin.reorder` | `OV2:2994` | `orderKey` |
| `thread.active.reorder` | `OV2:3000` | `orderKey` |

Callers: `M/features/home/useThreadListActions.ts:89-93,213-220,566-569`; `M/features/threads/ThreadDetailScreen.tsx:548-549`.

### 3.6 Cut variants (answer with `OrchestrationV2DispatchCommandError`)

| Variants | Reason |
|---|---|
| `thread.create` | No mobile caller; mobile uses `launchThread`. |
| `thread.guest-share.close`, `thread.guest-message.dismiss` | Guest share. |
| `thread.auto-settle` | Server-internal. |
| `thread.mark-unread` | No mobile caller. |
| `thread.pull-request.link`, `.unlink`, `-link.sync`, `.watch`, `.sync` | PRs. |
| `thread.title.regeneration.complete` | Server-internal. |
| `thread.model-selection.set` | Mobile sends the model per turn on D1 (`CR/operations/commands.ts:636-643`). Optional. |
| `provider-session.detach` | No mobile caller. |
| `prepared-run.release`, `.progress`, `.fail`, `.retry` | Worktree preparation. |
| `notification.delivery.accept` | Notifications. |
| `subagent.stop` | j3 emits no subagents. |
| `queued-run.reorder` | No client caller. |
| `checkpoint.rollback` | Checkpoints. |
| `thread.fork`, `thread.merge_back` | Fork and side chats. |
| `delegated_task.*` (4 variants) | Delegated tasks. |
| `thread.created.record` | Server-internal. |
| `provider.switch` | Provider handoff. |

All of these are defined in `OV2:2877-3347`. The server-only `OrchestrationV2InternalCommand` (`OV2:3350-3545`) is never accepted on the wire.

## 4. Streams and the projection model

### 4.1 Sequence model (both streams)

- Use **one global integer that only ever increases** across all projects and threads (reference: `S/persistence/OrchestrationEventStore.ts:640-672`). Each thread therefore sees gaps in its sequence. Clients only need it to increase.
- **Persist the high-water mark in SQLite.** The client caches its cursors and the acknowledged-write sequences across server restarts. If the counter reset, writes would look "caught up" too early.
- Client dedupe:
  - shell: ignores `sequence <= snapshotSequence` (`CR/state/shellReducer.ts:138`)
  - thread: ignores `sequence <= lastSequence` (`CR/state/threads.ts:486`)

  Overlap is harmless; going backwards is silently dropped. Reference comment: "Subscribe before reading persisted events … clients dedupe overlap by sequence" (`S/ws.ts:944`).
- **On resume, re-sending a snapshot is always valid.** The client replaces its state wholesale on any snapshot:
  - shell: `CR/state/shell.ts:225-237`, for any snapshot *without* `resolvedRepositoryIdentityRoots`
  - thread: `CR/state/threads.ts:613-633`

  The reference server re-snapshots when the gap is too large, when `afterSequence > highWater`, or when the replay would exceed its budget (`S/ws.ts:956-1008,1210-1218`; `S/orchestration-v2/ThreadStream.ts:18-27,96-114`).
- **Always send `{"kind":"synchronized"}` after the initial snapshot or replay.** The client always sends `requestCompletionMarker:true`, and it stays in "synchronizing" until the marker arrives (`CR/state/shell.ts:218-223,252-255`; `CR/state/threads.ts:603-611`).
- Throttling: coalescing streaming updates in a 50 ms window per entity is enough (reference: `S/orchestration-v2/ThreadLiveEventCoalescer.ts:18-64`). Coalescing may skip sequences but must never reorder them. Remember the per-chunk `Ack` gating (`effect-rpc.md` §Streams): updates produced while waiting for an Ack are batched into the next `Chunk`.

### 4.2 `orchestration.subscribeShell` (M4) and `orchestration.subscribeThread` (M5)

**Shell input** `OrchestrationV2SubscribeShellInput` (`OV2:3649-3662`). All keys are optionalKey:
- `afterSequence`
- `requestCompletionMarker`
- `omitIdleSubagents`, `omitOlderSettled`: sent only when the gating flags are set, which j3 never does

Error: `OrchestrationV2GetShellSnapshotError`. The client builds the input at `CR/state/shell.ts:362-436`.

**Shell items** `OrchestrationV2ShellStreamItem` (`OV2:2214-2245`), discriminated by `kind`:

| kind | fields | client effect |
|---|---|---|
| `synchronized` | — | becomes live |
| `snapshot` | `snapshot: {schemaVersion: PositiveInt, snapshotSequence, threads: ThreadShell[], archivedThreads: ThreadShell[] (send `[]`), projects: ProjectShell[], olderSettled?}` (`OV2:2199-2211`); never send `resolvedRepositoryIdentityRoots` | replaces everything |
| `project.updated` | `sequence`, `project: OrchestrationProjectShell` | upsert |
| `project.removed` | `sequence`, `projectId` | remove |
| `thread.updated` | `sequence`, `location: "active"\|"archive"`, `thread: ThreadShell` | `active` upserts; `archive` removes from the active list (`CR/state/shellReducer.ts:160-182`) |
| `thread.removed` | `sequence`, `location`, `threadId` | removes from both lists |

On archive, the reference server sends `thread.removed` with `location:"active"` (`S/orchestration-v2/ShellStream.ts:668-698`).

**Thread input** `OrchestrationV2SubscribeThreadInput` (`OV2:3747-3757`):
- `threadId`: required
- optionalKey: `afterSequence`, `requestCompletionMarker`, `acceptBoundedSnapshot`, `acceptDeferredWork`, `acceptCompactTurnItems`, `acceptDedupedBodies`, `acceptCompactIds`

The client always sends `requestCompletionMarker:true` and `acceptDedupedBodies:true` (`CR/state/threads.ts:937-1029`). **Every `accept*` flag is an opt-in that j3 ignores.** An unknown thread fails with `OrchestrationV2GetThreadProjectionError`.

**Thread items** `OrchestrationV2ThreadStreamItem` (`OV2:3858-3881`). It is wrapped in `withRecordIdTable`, which does nothing without `idTable`:

| kind | fields |
|---|---|
| `synchronized` | — |
| `snapshot` | `snapshotSequence`, `projection: OrchestrationV2ThreadProjection`. Optional keys `historyCursor`, `hasMoreHistory`, `latestLocalTurnOrdinal`, `turnItemsOmitLocalVisible`, `idTable`: **omit all of them.** The client then treats the projection as the complete timeline (`CR/state/threads.ts:613-633`). |
| `event` | `sequence`, `event: OrchestrationV2DomainEvent` |

The client applies events with `applyOrchestrationV2ProjectionEvent` (`CR/state/orchestrationV2Projection.ts:259-420`). It ignores events whose `event.threadId` is not this thread (`:264`). A `thread.deleted` event marks the thread deleted and clears the cache (`CR/state/threads.ts:531-532,583-586`).

### 4.3 Projection data model

**`OrchestrationV2ThreadProjection`** (`OV2:1997-2022`). Every array is required and may be empty:
- `thread`
- `runs`, `attempts`, `nodes`, `subagents`
- `providerSessions`, `providerThreads`, `providerTurns`
- `runtimeRequests`, `messages`, `plans`, `turnItems`
- `checkpointScopes`, `checkpoints`, `contextHandoffs`, `contextTransfers`
- `visibleTurnItems`
- plus `updatedAt` (DateTimeUtc)

j3 sends `[]` for `nodes`, `subagents`, `checkpointScopes`, `checkpoints`, `contextHandoffs` and `contextTransfers`. Mobile resolves attempts through `attemptByRootNodeId.get(item.nodeId)` (`M/lib/threadActivity.ts:1992-2007`), so set each item's `nodeId` to its attempt's `rootNodeId`.

**`visibleTurnItems`** (`OV2:1831-1837`) is the feed. Mobile builds its rows only from this list (`M/lib/threadActivity.ts:1977-2168`). **The server must fill it in snapshots**; the client maintains it only incrementally, from events (`CR/state/orchestrationV2Projection.ts:372-407`).
- Each row: `{position: NonNegativeInt (0..n-1), visibility:"local", sourceThreadId: thread.id, sourceItemId: item.id, item}`.
- Order by `item.ordinal`, then `id` (`CR/state/orchestrationV2Projection.ts:229-256`).
- Hide items of `rolled_back` runs, and `user_message` items with `inputIntent:"queued_turn"` whose run is `cancelled` (`packages/shared/src/orchestrationV2Timeline.ts:72-108`).

**`OrchestrationV2AppThread`** (`OV2:441-512`):
- Required:
  - `createdBy`, `creationSource`, `id`, `projectId`
  - `title`: TrimmedNonEmptyString, **must be non-blank**
  - `providerInstanceId`: `"omp"`
  - `modelSelection`, `runtimeMode`, `interactionMode`
  - `branch`, `worktreePath`, `activeProviderThreadId`: NullOr
  - `lineage`: `{parentThreadId:null, relationshipToParent:null, rootThreadId:<own id>}` (`OV2:153-158`)
  - `forkedFrom`: NullOr
  - `createdAt`, `updatedAt`
  - `archivedAt`, `deletedAt`: NullOr
- Defaulted: `settledOverride`, `settledAt`, `lastVisitedAt`.
- Everything else is optional; omit it.

**`OrchestrationV2Run`** (`OV2:671-703`):
- Required:
  - `id`, `threadId`
  - `ordinal`: PositiveInt
  - `providerInstanceId`, `modelSelection`
  - `providerThreadId`: NullOr
  - `userMessageId`
  - `rootNodeId`, `activeAttemptId`: NullOr
  - `status`: `preparing|queued|starting|running|waiting|completed|interrupted|failed|cancelled|rolled_back` (`OV2:585-596`)
  - `requestedAt`
  - `startedAt`, `completedAt`, `checkpointId`, `contextHandoffId`: NullOr
- Optional: `queuePosition` (NullOr PositiveInt), `queueHeld` (boolean).
- The active run is the last run whose status is one of `preparing|starting|running|waiting` (`CR/state/threadWorkflows.ts:16,44-46`).

**`OrchestrationV2RunAttempt`** (`OV2:730-752`):
- `id`, `runId`
- `attemptOrdinal`: PositiveInt
- `rootNodeId`, `providerInstanceId`
- `providerThreadId`: **not nullable**
- `providerTurnId`: NullOr
- `reason`: `initial|steering_restart|retry|provider_recovery`
- `status`: `pending|running|completed|interrupted|failed|cancelled|superseded`
- `startedAt`, `completedAt`: NullOr

**Provider entities.** These are needed for the steer button and server-side `auto` resolution (`CR/state/threadWorkflows.ts:71-93,131-179`):
- `OrchestrationV2ProviderSession` (`OV2:941-952`):
  - `id`, `driver:"pi"`, `providerInstanceId:"omp"`
  - `status`: `starting|ready|running|waiting|stopped|error`
  - `cwd`
  - `model`: NullOr
  - `capabilities`: an `OrchestrationV2ProviderCapabilities` (`OV2:259-394`) where **every nested boolean is required**
  - `createdAt`, `updatedAt`
  - `lastError`: NullOr

  Copy `PiProviderCapabilitiesV2` (`PI/adapter.ts:91-182`). It has `turns.supportsActiveSteering:true`, `supportsQueuedMessages:true` and `supportsSteeringByInterruptRestart:false` (`:109-118`).
- `OrchestrationV2ProviderThread` (`OV2:1069-1103`):
  - `id`, `driver`, `providerInstanceId`
  - `providerSessionId`, `appThreadId`, `ownerNodeId`, `nativeThreadRef`, `nativeConversationHeadRef`: NullOr
  - `status`: `not_loaded|idle|active|archived|closed|error`
  - `firstRunOrdinal`, `lastRunOrdinal`: NullOr
  - `handoffIds`: `[]`
  - `forkedFrom`: NullOr
  - `createdAt`, `updatedAt`
- `OrchestrationV2ProviderTurn` (`OV2:1163-1182`):
  - `id`, `providerThreadId`, `nodeId`
  - `runAttemptId`, `nativeTurnRef`: NullOr
  - `ordinal`: PositiveInt
  - `status`: `pending|running|completed|interrupted|failed|cancelled`
  - `startedAt`, `completedAt`: NullOr
- `canPromoteToSteer` requires all of: the active run is `running`; its `activeAttemptId` is set; a `providerTurns` entry for that attempt has `status:"running"`; and the session has steering capability. The session is resolved via the run's `providerThreadId` → `providerThreads[].providerSessionId` → `providerSessions`.

**`OrchestrationV2RuntimeRequest`** (`OV2:1200-1224`):
- `id`, `nodeId`
- `providerTurnId`, `nativeRequestRef`: NullOr
- `kind`: `command|file-read|file-change|mcp-elicitation|permission|dynamic_tool_call|user_input|auth_refresh`
- `status`: `pending|resolved|expired|cancelled`
- `responseCapability`: `{type:"live", providerSessionId, supportsFeedback?}` \| `{type:"message"}` \| `{type:"not_resumable", reason}`
- `createdAt`
- `resolvedAt`: NullOr
- optional: `decision`, `feedback`, `answers`

A pending `user_input` request is shown **only if a `user_input_request` turn item with the same `requestId` exists**; otherwise it is silently skipped (`CR/state/threadRequests.ts:228-284`, esp. `:238-242`). Mobile shows the first pending request above the composer as `PendingUserInputCard` or `PendingApprovalCard` (`M/features/threads/ThreadDetailScreen.tsx:1606-1647`; `M/features/threads/PendingUserInputCard.tsx:99`; `M/features/threads/PendingApprovalCard.tsx:33`).

**`OrchestrationV2ConversationMessage`** (`OV2:1283-1312`):
- `createdBy`, `creationSource`, `id`
- `threadId`
- `runId`, `nodeId`: NullOr
- `role`: `user|assistant|system`
- `text`, `attachments`
- `streaming`: boolean
- `createdAt`, `updatedAt`
- optional `context`

Assistant messages use `createdBy:"agent"`, `creationSource:"provider"` (`PI/adapter.ts:910-915`). Mobile does not draw the feed from `messages`. It uses them for queued-run text (`CR/state/threadWorkflows.ts:146-162`) and for optimistic reconciliation (§3.4). Send a `message.updated` alongside each user/assistant `turn-item.updated`.

**`OrchestrationV2PlanArtifact`** (`OV2:1343-1364`):
- base: `{id: PlanId, threadId, runId|null, nodeId, status: draft|active|completed|superseded, detailInTurnItem?: true}`
- plus either `kind:"todo_list", steps: PlanStep[], explanation?` or `kind:"proposed_plan", markdown`
- `PlanStep` (`OV2:1315-1321`): `{id, text (both non-blank), status: pending|running|completed, durationAnchorAt?, durationMs?}`

**`OrchestrationV2ThreadShell`**, the thread-list row (`OV2:2058-2135`):
- Required:
  - `createdBy`, `creationSource`, `id`, `projectId`
  - `title`: plain string here
  - `providerInstanceId`, `modelSelection`, `runtimeMode`, `interactionMode`
  - `branch`, `worktreePath`: NullOr
  - `lineage`
  - `forkedFrom`, `activeProviderThreadId`, `latestRunId`, `activeRunId`: NullOr
  - `status`: `"idle"` ∪ RunStatus (`OV2:2033-2036`)
  - `pendingRuntimeRequest`: NullOr `{id, kind, createdAt, blocking?}` (`OV2:2039-2045`)
  - `latestVisibleMessage`: NullOr `{id, role, text, updatedAt}` (`OV2:2049-2054`)
  - `latestUserMessageAt`: NullOr
  - `hasActionableProposedPlan`: boolean
  - `itemCount`, `visibleItemCount`
  - `createdAt`, `updatedAt`
  - `archivedAt`, `settledOverride`, `settledAt`, `deletedAt`: NullOr
- Useful optionals: `activityRunStatus` (`preparing|starting|running|waiting`|null), `latestRunCompletedAt`, `latestRunStopRequested`, `lastError`.
- How the client presents it (`CR/state/models.ts:223-350`):
  - running state = `activityRunStatus ?? status`
  - `hasPendingUserInput` = `pendingRuntimeRequest.kind == "user_input"`
  - `idle` is shown as completed
- Lists hide threads whose lineage is `subagent` or `sideChat` (`OV2:162-163`).

**`OrchestrationProjectShell`** (`C/orchestrationProject.ts:8-25`):
- Required: `id`, `title`, `workspaceRoot` (both non-blank), `defaultModelSelection` (NullOr), `scripts` (`[]`), `createdAt`, `updatedAt` (IsoDateTime)
- Optional: `repositoryIdentity`, `faviconPath`, `projectIcon`, `pinnedAt`, `archivedAt`, …

```json
{"createdBy":"user","creationSource":"mobile","id":"1d3e…","projectId":"8c7d…","title":"Fix login bug","providerInstanceId":"omp",
 "modelSelection":{"instanceId":"omp","model":"anthropic/claude-sonnet-4-5"},"runtimeMode":"full-access","interactionMode":"default",
 "branch":null,"worktreePath":null,"lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":"1d3e…"},"forkedFrom":null,
 "activeProviderThreadId":"pth_1d3e","latestRunId":"run_2","activeRunId":"run_2","status":"running","activityRunStatus":"running",
 "pendingRuntimeRequest":{"id":"rq_1","kind":"user_input","createdAt":"2026-11-01T12:01:00.000Z","blocking":true},
 "latestVisibleMessage":{"id":"msg_a1","role":"assistant","text":"Looking at the auth flow…","updatedAt":"2026-11-01T12:00:59.000Z"},
 "latestUserMessageAt":"2026-11-01T12:00:05.000Z","hasActionableProposedPlan":false,"itemCount":6,"visibleItemCount":5,
 "createdAt":"2026-11-01T11:00:00.000Z","updatedAt":"2026-11-01T12:01:00.000Z","archivedAt":null,"settledOverride":null,"settledAt":null,"deletedAt":null}
```

### 4.4 Domain events and TurnItems

**Event base** (`OV2:1840-1849`):
- Required: `id: EventId`, `threadId`, `occurredAt: DateTimeUtc` (the client sets `projection.updatedAt` from it), `type`, `payload`
- Optional: `runId`, `nodeId`, `driver`, `providerInstanceId`, `rawEventId`

**Every event is a full replacement of one entity by `id`. There are no text deltas.** Streaming text means re-sending the whole item with the text accumulated so far.

Forward-compatibility:
- An unknown `event.type`, or a `turn-item.updated` whose `payload.type` is unknown, decodes to `unknown-event` and only advances the cursor (`OV2:1825-1856`).
- Unknown item types inside `turnItems` / `visibleTurnItems` are dropped (`OV2:1810-1829`).
- **A known type with a bad payload is a hard failure** (`OV2:1837`).

The 20 event types j3 emits (`OV2:1851-1994`):

| `type` | payload | used for |
|---|---|---|
| `thread.created` | AppThread | launch |
| `thread.archived`, `thread.unarchived`, `thread.deleted`, `thread.metadata-updated`, `thread.runtime-mode-updated`, `thread.interaction-mode-updated`, `thread.model-selection-updated` | AppThread (the full thread) | D4–D9; model change |
| `run.created`, `run.updated` | Run (full) | turn lifecycle, queue |
| `run-attempt.created`, `run-attempt.updated` | RunAttempt | turn lifecycle |
| `provider-session.attached`, `provider-session.updated` | ProviderSession | steer gating |
| `provider-thread.updated` | ProviderThread | steer gating |
| `provider-turn.updated` | ProviderTurn | steer gating |
| `runtime-request.updated` | RuntimeRequest | questions |
| `message.updated` | ConversationMessage | queued text, reconciliation |
| `turn-item.updated` | TurnItem | the feed |
| `plan.updated` | PlanArtifact | todos |

**TurnItem base fields** (`OV2:1483-1510`):
- `id`, `threadId`
- `runId`, `nodeId`, `providerThreadId`, `providerTurnId`, `nativeItemRef`, `parentItemId`: NullOr
- `ordinal`: NonNegativeInt. The thread-wide order key; **assign it once per item and never change it.** A changed ordinal causes a remove and re-insert (`CR/state/orchestrationV2Projection.ts:212-256`).
- `status`: `idle|pending|running|waiting|completed|failed|cancelled|interrupted` (`OV2:1420-1429`)
- `title`: NullOr string
- `startedAt`, `completedAt`: NullOr. The feed shows `startedAt ?? updatedAt` (`M/lib/threadActivity.ts:2086`).
- `updatedAt`
- optional: `deferredWork` (omit), `toolNonExecutionKind`, `toolSurface`, `toolIcon`, `toolSource`

The 15 kinds j3 emits (union `OV2:1583-1805`). Rendering goes through `buildThreadFeed` (`M/lib/threadActivity.ts:1977-2246`) and `renderFeedEntry` (`M/features/threads/ThreadFeed.tsx:1653-2328`):

| kind | def | extra fields (★ = required) | mobile rendering |
|---|---|---|---|
| `user_message` | `OV2:1589-1604` | ★`createdBy`, ★`creationSource`, ★`messageId`, ★`inputIntent: turn_start\|queued_turn\|steer\|promoted_queued_to_steer` (`OV2:1474-1479`), ★`text`, ★`attachments`, `context?` | user bubble with an intent badge (`M/features/threads/userMessageIntentBadge.ts:22`). Hidden while `queued_turn`+`pending` (`M/lib/threadActivity.ts:2063-2068`). |
| `assistant_message` | `OV2:1605-1613` | ★`messageId`, ★`text`, ★`streaming`, `attachments?` | markdown bubble. The feed id is `messageId` (`M/lib/threadActivity.ts:2125`). Meta is shown once it stops streaming (`M/features/threads/ThreadFeed.tsx:1877-1885`). |
| `reasoning` | `OV2:1614-1619` | ★`text`, ★`streaming` | "thinking" work-log row (`M/lib/threadActivity.ts:476,652`) |
| `command_execution` | `OV2:1653-1664` | ★`input`, `output?`, `outputOmitted?`, `exitCode?` | tool row; preview is `input`. Send `output` inline and never `outputOmitted`; then G1 and M16 are not needed. |
| `file_change` | `OV2:1642-1652` | ★`fileName` (non-blank), `additions?`, `deletions?`, `diffStr?`, `newStr?`, `changes?` | tool row |
| `file_search` | `OV2:1665-1670` | `pattern?`, `results?: [{fileName, line?, column?, preview?}]` | tool row |
| `web_search` | `OV2:1671-1676` | `patterns?`, `results?: [{title?, url?, snippet?}]` | tool row |
| `dynamic_tool` | `OV2:1795-1804` | ★`toolName` (NullOr), ★`input` (any JSON; the key is required), `output?` | generic tool row |
| `user_input_request` | `OV2:1634-1641` | ★`requestId`, ★`questions: [{id, header, question (all non-blank), options: [{label, description (both non-blank), value?, preview?}], multiSelect?, allowCustomAnswer?, initialAnswer?, required?}]` (`OV2:1324-1340`), `questionAnswer?: {requestId, answers, attachmentsByQuestionId, questionTextById?}` (`C/providerPolicy.ts:79-84`), `responseMode?: "message"` | pending card while its request is pending; history afterwards (`M/features/threads/QuestionAnswerHistory.tsx`) |
| `todo_list` | `OV2:1627-1633` | ★`planId`, ★`steps`, `explanation?` | **dropped by stock mobile** (`M/lib/threadActivity.ts:2062`). Needs the client edit in §6.3. |
| `run_interrupt_request` | `OV2:1698-1704` | ★`message`, `unresponsiveAt?` | hidden (`M/lib/threadActivity.ts:2071`); setting `unresponsiveAt` offers Force stop |
| `run_interrupt_result` | `OV2:1705-1709` | ★`message` | row |
| `error` | `OV2:1715-1720` | ★`failure: {class: usage_limit\|provider_error\|transport_error\|permission_error\|validation_error\|unknown, message (non-blank ≤4096), code: string\|null, retryable: bool\|null, resetAt?}`, `retry?: {attempt, maxAttempts\|null, retryDelayMs\|null}` (`OV2:1442-1471`) | error row |
| `system_notice` | `OV2:1710-1714` | ★`message` | prominent row |
| `compaction` | `OV2:1721-1728` | ★`driver` (NullOr), `summary?`, `beforeTokenCount?`, `afterTokenCount?` | divider (`M/features/threads/ThreadFeed.tsx:1837-1864`) |

j3 never emits: `proposed_plan`, `approval_request` (omp runs yolo), `notification`, `checkpoint`, `handoff`, `fork`, `thread_created`, `secret_request`, `link_request`, `subagent`.

### 4.5 Queue, steer and stop in the projection

- **Queued message.** Emit a `message.updated` for the user message, a `run.created` with `status:"queued"`, `userMessageId`, and `queuePosition`, and a `user_message` item with `inputIntent:"queued_turn"` and `status:"pending"` (`CR/state/threadWorkflows.ts:115-162`). When the run starts: run → `starting`/`running`, item → `completed`. On cancel: run → `cancelled`, and the item is hidden by the visibility rule.
- **Held queue.** Any queued run with `queueHeld:true` makes the composer show Resume (`CR/state/threadWorkflows.ts:167`; `M/state/use-thread-composer-state.ts:364,755`). `run.interrupt{holdQueue:true}` must set this flag on every remaining queued run.
- **Steer.** Emit a `user_message` item with `inputIntent:"steer"` (or `"promoted_queued_to_steer"`) in the active run. There is no separate pending-steer entity.
- **Stop.**
  1. Emit a `run_interrupt_request` item (`status:"completed"`, `message` = reason). Reference: `S/orchestration-v2/Orchestrator.ts:10564-10592`.
  2. Set shell `latestRunStopRequested:true`.
  3. When it lands: emit a `run_interrupt_result` item, and set the run → `interrupted` and the attempt and provider turn → `interrupted`.
- **The "working" indicator** follows the active run's status and the shell's `activityRunStatus`. A terminal `run.updated` is what stops the spinner; `streaming:false` on the assistant item does not.

### 4.6 One full turn on the wire

Abbreviations: `B` = the item base `"threadId":"T","runId":"run_1","nodeId":"node_r1","providerThreadId":"pth_T","providerTurnId":"ptu_1","nativeItemRef":null,"parentItemId":null,"title":null`, and `E(n)` = `{"kind":"event","sequence":n,"event":{"id":"evt_n","threadId":"T","runId":"run_1","occurredAt":"…",…}}`.

```json
{"kind":"snapshot","snapshotSequence":41,"projection":{"thread":{…AppThread…},"runs":[],"attempts":[],"nodes":[],"subagents":[],
  "providerSessions":[{…session with PiProviderCapabilitiesV2…}],"providerThreads":[{…}],"providerTurns":[],"runtimeRequests":[],
  "messages":[],"plans":[],"turnItems":[],"checkpointScopes":[],"checkpoints":[],"contextHandoffs":[],"contextTransfers":[],
  "visibleTurnItems":[],"updatedAt":"2026-11-01T12:00:00.000Z"}}
{"kind":"synchronized"}
E(42) "type":"message.updated","payload":{"createdBy":"user","creationSource":"mobile","id":"msg_u1","threadId":"T","runId":"run_1","nodeId":null,"role":"user","text":"Why does login fail?","attachments":[],"streaming":false,"createdAt":"…","updatedAt":"…"}
E(43) "type":"run.created","payload":{"id":"run_1","threadId":"T","ordinal":1,"providerInstanceId":"omp","modelSelection":{…},"providerThreadId":"pth_T","userMessageId":"msg_u1","rootNodeId":"node_r1","activeAttemptId":null,"status":"starting","requestedAt":"…","startedAt":null,"completedAt":null,"checkpointId":null,"contextHandoffId":null}
E(44) "type":"turn-item.updated","payload":{"id":"ti_u1",B,"ordinal":1,"status":"completed","startedAt":"…","completedAt":"…","updatedAt":"…","type":"user_message","createdBy":"user","creationSource":"mobile","messageId":"msg_u1","inputIntent":"turn_start","text":"Why does login fail?","attachments":[]}
E(45) "type":"run-attempt.created","payload":{"id":"att_1","runId":"run_1","attemptOrdinal":1,"rootNodeId":"node_r1","providerInstanceId":"omp","providerThreadId":"pth_T","providerTurnId":"ptu_1","reason":"initial","status":"running","startedAt":"…","completedAt":null}
E(46) "type":"run.updated","payload":{…full run…,"activeAttemptId":"att_1","status":"running","startedAt":"…"}
E(47) "type":"provider-turn.updated","payload":{"id":"ptu_1","providerThreadId":"pth_T","nodeId":"node_r1","runAttemptId":"att_1","nativeTurnRef":null,"ordinal":1,"status":"running","startedAt":"…","completedAt":null}
E(48) "type":"turn-item.updated","payload":{"id":"ti_r1",B,"ordinal":2,"status":"running","startedAt":"…","completedAt":null,"updatedAt":"…","type":"reasoning","text":"Looking at the auth flow","streaming":true}
E(53) "type":"turn-item.updated","payload":{"id":"ti_a1",B,"ordinal":3,"status":"running",…,"type":"assistant_message","messageId":"msg_a1","text":"The login fails because","streaming":true}
E(57) "type":"turn-item.updated","payload":{"id":"ti_c1",B,"ordinal":4,"status":"running","title":"bash",…,"type":"command_execution","input":"npm test -- auth"}
E(60) "type":"turn-item.updated","payload":{"id":"ti_c1",B,"ordinal":4,"status":"completed",…,"type":"command_execution","input":"npm test -- auth","output":"1 failing","exitCode":1}
E(62) "type":"runtime-request.updated","payload":{"id":"rq_1","nodeId":"node_r1","providerTurnId":"ptu_1","nativeRequestRef":null,"kind":"user_input","status":"pending","responseCapability":{"type":"live","providerSessionId":"ps_T"},"createdAt":"…","resolvedAt":null}
E(63) "type":"turn-item.updated","payload":{"id":"ti_q1",B,"ordinal":5,"status":"waiting",…,"type":"user_input_request","requestId":"rq_1","questions":[{"id":"q1","header":"Fix","question":"Rotate the refresh token on every login?","options":[{"label":"Yes","description":"Yes","value":"Yes"},{"label":"No","description":"No","value":"No"}],"allowCustomAnswer":true}]}
E(64) "type":"run.updated","payload":{…,"status":"waiting"}
E(70) "type":"runtime-request.updated","payload":{…rq_1…,"status":"resolved","resolvedAt":"…","answers":{"q1":"Yes"}}
E(71) "type":"turn-item.updated","payload":{"id":"ti_q1",…,"status":"completed","completedAt":"…","type":"user_input_request","requestId":"rq_1","questions":[…],"questionAnswer":{"requestId":"rq_1","answers":{"q1":"Yes"},"attachmentsByQuestionId":{}}}
E(72) "type":"plan.updated","payload":{"id":"plan_T","threadId":"T","runId":"run_1","nodeId":"node_r1","status":"active","kind":"todo_list","steps":[{"id":"0.0","text":"Reproduce","status":"completed"},{"id":"0.1","text":"Patch refresh","status":"running"}],"detailInTurnItem":true}
E(73) "type":"turn-item.updated","payload":{"id":"ti_p_T_1",B,"ordinal":6,"status":"running",…,"type":"todo_list","planId":"plan_T","steps":[…same…]}
E(80) "type":"turn-item.updated","payload":{"id":"ti_a1",…,"status":"completed","completedAt":"…","type":"assistant_message","messageId":"msg_a1","text":"<final>","streaming":false}
E(81) "type":"message.updated","payload":{…msg_a1…,"createdBy":"agent","creationSource":"provider","role":"assistant","text":"<final>","streaming":false}
E(82) "type":"provider-turn.updated","payload":{…ptu_1…,"status":"completed","completedAt":"…"}
E(83) "type":"run-attempt.updated","payload":{…att_1…,"status":"completed","completedAt":"…"}
E(84) "type":"run.updated","payload":{…run_1…,"status":"completed","completedAt":"…"}
```

On the shell stream, every change above is followed by `{"kind":"thread.updated","sequence":n,"location":"active","thread":{…ThreadShell…}}`, carrying the updated `status`, `activeRunId`, `activityRunStatus`, `pendingRuntimeRequest` and `latestVisibleMessage`.

## 5. Sourcing each concept from omp RPC

omp runs as `omp --mode rpc-ui --approval-mode=yolo`, one process per thread, with `cwd` set to the project. `rpc-ui` is required for UI requests (R:28, R:1294-1302). After `ready`, j3 sends:
- `negotiate_protocol{protocolVersion:2}` (R:53-72)
- `set_ask_dialog{enabled:true}` (R:727-748)
- `set_event_filter{events:null, messageUpdates:"delta"}` (R:923-927)

The Pi adapter (`PI/adapter.ts`) does the same job for Pi, and its line numbers are given as working references.

| Concept (t3 shape) | omp source | Mapping rules | Ref |
|---|---|---|---|
| Model list (`ServerProvider.models`) | `get_available_models` → `ModelInfo{id, name, provider, reasoning, input, thinking?{efforts}}` (R:169-171; W:2557-2588) | `slug = "<provider>/<id>"`, `name = name`. On `set_model` split at the first `/` into `{provider, modelId}` (R:167; W:6668). Pick `isDefault` from `get_state.model`. Push changes through `providerStatuses` (M1). | |
| Thinking picker (descriptor `thinking`) | `ModelInfo.reasoning` + `thinking.efforts` (W:2534-2553); levels `off\|minimal\|low\|medium\|high\|xhigh\|max` (R:179; W:2447-2464) | When `reasoning` is true, emit a select with `off` followed by `efforts`, and mark `get_state.thinkingLevel` as `isDefault`. When it is false, emit `optionDescriptors:[]`. Before a prompt whose `modelSelection.options.thinking` differs from the current level, send `set_thinking_level{level}` (W:6717). | `PI/thinkingCapabilities.ts:24-44`; `PI/adapter.ts:2300-2360` |
| Slash commands and skills | `get_available_commands` → `{name, source, description?, input?{hint}}` (R:1029-1039; W:3338-3349); push event `available_commands_update` (W:4131) | `source:"skill"` → `skills[]`, name with the `skill:` prefix stripped and `path` from the source info. Everything else → `slashCommands[]`. Serve them per cwd in `workspaceSnapshots` (M3). | `PI/commands.ts:44-90` |
| Skill invocation | prompt text | The composer inserts `$<skill> ` (`M/features/threads/use-composer-command-menu.ts:196`). omp expands one leading `/skill:<name>` (R:406). Either rewrite the first `$name` that matches a known skill into a leading `/skill:name`, or apply the client edit in §6.3. Slash commands pass through as `/name args`. A prompt ack with `agentInvoked:false` means a local-only command with no run: emit no run, or complete it at once (R:345). | `PI/mcpExtensionSource.ts:258-280` |
| Thread (AppThread, ThreadShell) | j3 SQLite index; omp `get_state{sessionId, sessionFile, model, thinkingLevel}` (R:452-579; W:3048-3113) | j3 owns `id`, `title`, `projectId`, `archivedAt`, `deletedAt`, `createdAt`/`updatedAt` and `modelSelection`. Mirroring the title with `set_session_name` is optional (R:243). | |
| Launch (M7) | spawn omp → `get_state` → `set_model` / `set_thinking_level` → `prompt{message, images}` | Emit `thread.created`, the user message, `run.created`, then return the projection. `generateTitle` is ignored, because the title already comes from the prompt. | |
| D1 start (idle) | `prompt{message, images?}` (R:130, R:329-359; W:6101-6118) | Images: read the uploaded bytes and send them as `{type:"image", mimeType, data: base64}` (W:278-288). Correlate through the command `id` → `prompt_result.id` (R:122,345-355; W:4104-4115). | |
| D1 steer (`auto` while running) | `steer{message, images?}` (R:131; W:6122) | Emit a `user_message` with `inputIntent:"steer"` in the active run. | `PI/adapter.ts:2562-2664` |
| D1 queue / D12–D16 | **queue owned by j3** | Keep queued runs in j3, not in omp's followUp queue. omp's `queue_update` (R:889-906; W:3905-3912) carries text only, with no ids, model or attachments, so it cannot back edit, hold or per-message models. On `session_settled`, start the next queued run unless the queue is held. D14 promote → `steer`. | |
| D2/D3 stop | `abort` (R:135; W:6195) | Run → `interrupted` when `prompt_result.status:"aborted"` arrives. With `holdQueue`, set `queueHeld` on the queued runs. M18 force stop: kill the process. | `PI/adapter.ts:2705-2719` |
| Model change mid-thread | `set_model` / `set_thinking_level` before a prompt; event `thinking_level_changed` (W:3883-3892); `model_changed` → re-read `get_state` (W:3840) | Emit `thread.model-selection-updated` whenever the effective selection changes. | |
| Run lifecycle (Run, RunAttempt, ProviderTurn, ProviderSession) | `agent_start` (W:3623), `turn_start` / `turn_end` (W:3652-3659), `agent_end{yielded, isTerminal}` (W:3630-3649), `prompt_result{status: completed\|aborted\|error, error?}` (W:4104-4115), `session_settled` (W:4119; R:361-369) | `agent_start` → run `running`, attempt and provider turn `running`, session `running`. **A run ends at `session_settled`**: `completed`, or `interrupted` / `failed` according to the last `prompt_result.status`. On an error, also emit an `error` item from `PromptError{message, retryable, httpStatus?, code?}` (W:4087-4100). Mint ids yourself: `run_<n>`, `att_<n>`, `ptu_<n>`, `ps_<thread>`, `pth_<thread>`. | `PI/adapter.ts:1697-1745,1950-1971` |
| Assistant text (`assistant_message` + `message.updated`) | `message_start` / `message_update` / `message_end` (W:3662-3685); delta mode: `text_start{block}`, `text_delta{contentIndex, delta}`, `text_end{content}` (R:925; W:1792-1884) | Make one item per (omp message, text `contentIndex`). Append deltas and flush at most every 50 ms with `streaming:true`, `status:"running"`. `text_end` / `message_end` are authoritative: `streaming:false`, `completed`. omp's `messageId` is unique only within one process (R:921), so derive item ids from j3's run and content index. | `PI/adapter.ts:1746-1797` |
| Thinking (`reasoning`) | `thinking_start` / `_delta` / `_end` (W:1933-2025) | Same as text. A thinking block with `narration:true` is answer text, so render it as `assistant_message` (W:188-189). Hide `redactedThinking`. | `PI/adapter.ts:925-935` |
| Tool calls | `toolcall_start` / `_delta` / `_end` (W:2123-2215); `tool_execution_start{toolCallId, toolName, args, intent?}`, `_update{partialResult}`, `_end{result, isError}` (W:3688-3731) | Key the item by `toolCallId`. Kind by tool name: `bash` → `command_execution{input: args.command, output, exitCode}`; `edit`/`write` → `file_change{fileName: args.path, diffStr?, newStr?}`; `grep`/`find` → `file_search`; `web_search` → `web_search`; anything else → `dynamic_tool{toolName, input: args, output}`. `title` = `intent ?? toolName`. Status: `running` from the start event, then `completed`, or `failed` when `isError`. Skip the `ask` tool's own row; the `user_input_request` item represents it. | `PI/adapter.ts:1015-1086,1798-1809` |
| Questions (`runtime-request.updated` + `user_input_request`) | `extension_ui_request{id, method}` (R:1304-1331; W:4436-4561) | `ask{questions:[{id, question, options:[{label, description?}], header?, multi, recommended?}]}` → one request with `questions[i] = {id, header: header ?? "Question", question, options: [{label, description: description ?? label, value: label}], multiSelect: multi, allowCustomAnswer: true}`. omp never includes "Other", so the host offers free text (R:1320-1321). `select{title, options[]}`, `input{title, placeholder?}` and `editor{title, prefill?}` → a single question (with `initialAnswer` for `editor`). `confirm{title, message}` → two options, Yes and No, as `user_input` (or `approval_request` + `kind:"command"`, as the Pi adapter does). `timeout` → mark the request `expired` when it elapses. The runtime request has `kind:"user_input"` and `responseCapability:{type:"live", providerSessionId}`; set the run to `waiting`. | `PI/adapter.ts:1285-1420,3314-3348` |
| D10 answer | `extension_ui_response` (R:1345-1367; W:4629-4669) | `ask` → `{answers:[{id, selectedOptions: string[], customInput?}]}`, one per question in order. A string or string[] that matches option labels goes in `selectedOptions`; any other string goes in `customInput`. A single-select answer carries an option *or* `customInput`, never both (R:1364-1367). `select`/`input`/`editor` → `{value}`. `confirm` → `{confirmed: answer=="Yes"}` (or map `decision`). Then emit the resolved request and the completed item with `questionAnswer`. | `PI/adapter.ts:3353-3367` |
| D11 dismiss; UI cancel | `extension_ui_response{cancelled:true}`; inbound `extension_ui_request{method:"cancel", targetId}` (W:4502-4506; R:1359-1362) | Request → `cancelled`, item → `cancelled`. | |
| Notices | `extension_ui_request{method:"notify"}` (W:4509); event `notice{level, message}` (W:3874) | `system_notice` item. Ignore `setStatus`, `setWidget`, `setTitle`, `set_editor_text` and `open_url`. | `PI/adapter.ts:1289-1299` |
| Todos (`todo_list` + `plan.updated`) | `get_state.todoPhases: [{name, tasks:[{content, status, …}]}]` (W:2884-2900; R:516-526). There is no todo event and no `get_todos`. | Re-read `get_state` after each `todo` tool's `tool_execution_end` and at `agent_end`. Flatten phases into steps with `id = "<phase>.<task>"` and `text = content`. Status: `in_progress` → `running`; `completed` and `abandoned` → `completed`; `pending` and `blocked` → `pending`. Use one `PlanId` per thread and replace the whole step list each time. Clear on `todo_auto_clear` (W:3858-3866). | |
| Compaction | `auto_compaction_start` / `_end` (W:3734-3752) | `compaction` item, `driver:null`, `summary` from the result. | `PI/adapter.ts:1810-1869` |
| Retry | `auto_retry_start{attempt, maxAttempts, delayMs, errorMessage}`, `auto_retry_end` (W:3778-3799) | `error` item `{failure:{class:"provider_error", message, code:null, retryable:true}, retry:{attempt, maxAttempts, retryDelayMs: delayMs}}`. | `PI/adapter.ts:1870-1942` |
| History in snapshots (`turnItems` of settled turns) | `get_messages_page{cursor?, limit≤256}` → `{messages, totalMessages, nextCursor?}` (R:257-261; W:7169, W:3313). Refused while streaming (`session_busy`). Stable ids come from `get_entries` (R:1054-1058). | Messages map to items in order: `user` → `user_message` (skip `synthetic` and `attribution:"agent"`); each `assistant` content block → `reasoning` / `assistant_message` / tool item, with the tool output joined from the matching `toolResult.toolCallId`; `compactionSummary` → `compaction`. Derive item ids deterministically, e.g. `ti_<entryId>_<blockIndex>`, so a re-snapshot keeps the same ids. Timestamps are epoch ms [INFERENCE] → ISO. Serve the in-flight turn from j3's memory buffer (PLAN.md "Live turns"). | `PI/adapter.ts:2817-2850` |
| Archive, delete, rename, projects | none in omp | j3 SQLite. Delete stops the process and drops the thread from the index. | |
| Remote directory listing (M13) | none in omp | Native `read_dir`. | |

## 6. Trim: mobile modules to delete, and the protocol that disappears with them

Paths are relative to `apps/mobile/`.

### 6.1 Delete

| Module | Unused protocol once deleted |
|---|---|
| `src/features/agent-awareness`, `src/widgets`, `modules/t3-agent-notifications`, `modules/t3-subscription-widget`, `modules/t3-widget-expiry`, `plugins/withWidgetLogoAsset.cjs`, `plugins/lib/addWidgetAssetCatalog.cjs` | push registration; `serverRefreshProviders` from `src/widgets/useSubscriptionUsage.ts:20` (still used by the composer) |
| `src/features/cloud`, `ConnectOnboarding` route | `/api/connect/*`, relay/DPoP, Clerk. `src/lib/runtime.ts:8-9` must stop importing `cloud/dpop` and `cloud/managedRelayLayer`. |
| `src/features/browser`, `src/state/preview.ts`, `src/types/mobile-preview-stream.d.ts` | `preview.*` (9), `subscribePreviewEvents`, `subscribeDiscoveredLocalServers`, `/api/preview-stream` |
| `src/features/devices`, `src/state/device.ts`, `src/types/mobile-device-stream.d.ts` | `device.*` (10), `subscribeDeviceState`, `/api/device-hub`, `/api/auth/websocket-ticket/media` |
| `src/features/diffs`, `src/features/review`, `src/state/review.ts`, `src/state/reviewTurnDiffs.ts`, `modules/t3-review-diff` | `review.*` (3), `orchestration.getTurnDiff`, `orchestration.getFullThreadDiff` |
| `src/features/files` (move `filePath.ts`, `AttachmentFileScreen`, `FileMarkdownPreview` and `FilePreviewFeedback` out first), `modules/t3-source-text` | `projects.listEntries` |
| `src/features/terminal`, `src/state/terminal.ts`, `src/state/use-terminal-session.ts`, `modules/t3-terminal` | `terminal.*` (7), `subscribeTerminalMetadata` |
| `src/features/pull-requests` (move `pullRequestGlyphs.ts` and `pullRequestStatus.ts` first; 8 kept files import them), `src/state/pull-requests.ts`, `use-thread-pr.ts`, `thread-pr-demand.tsx`, `thread-pr-presentation.ts`, `src/connection/pull-request-cache-store.ts` | `pullRequests.*` (36), `POST /api/pull-requests/diff`, `thread.pull-request.*` commands |
| `src/state/git.ts`, `vcs.ts`, `use-vcs-action-state.ts`, `use-selected-thread-git-*.ts`, `use-selected-thread-worktree.ts`, `src/features/threads/git/*`, `GitActionProgressOverlay`, `ThreadGitControls*`, `worktree-setup-*`, `use-worktree-setup`, `checkout-new-task-branch`, `new-task-branch-checkout` | `vcs.*` (8), `git.*` (3), `subscribeVcsStatus`, `subscribeWorktreeSetup`, `worktreeSetup.cancel`, `prepared-run.*` |
| `src/state/sourceControl.ts`, `projectClones.ts`, `ProjectCloneBanner`, `AddProjectRepository`/`AddProjectDestination` routes | `sourceControl.*`, `projectClone.*`, `subscribeProjectClones`, `server.discoverSourceControl` |
| `src/state/peerLinks.ts`, `peerPairingCode.ts`, `LinkRequestCard`, `LinkedEnvironmentsSection` | `peerLinks.*` (4), `/api/auth/pairing-token` |
| `src/state/mcpApps.ts`, `McpApp*`, `mcpAppSizing` | `mcpApps.*` (4) |
| `src/features/usage`, `src/state/usage.ts`, `ComposerUsageLimits`, `UsageLimitRecoveryCard`, `ChatGpt*` | `server.getUsageSummary`, `provider.consumeResetCredit`, `provider.chatgpt.*` |
| `src/features/triage` | `server.prepareTriage` |
| scheduled-task settings screens, `ScheduledTaskPromptField`, `ScheduledTaskPickerScreens` | `scheduledTasks.*` (10) |
| Provider accounts, Auth/Waitlist, Notifications, Microphone, ClientStorage, Diagnostics, Usage, server update and maintenance settings screens; `src/features/diagnostics`, `src/features/updates` | `provider.auth.*` (6), `server.updateServer*`, `server.commitDesktopUpdate`, `subscribeServerLifecycle`, `server.getDatabaseStorage`, `server.updateProvider`, diagnostics methods |
| `src/features/sharing`, `plugins/withShareExtensionDisplayName.cjs`, `ThreadGuestShareBar`, `guestMessageStyle` | `guestShare.create`, `thread.guest-*` commands |
| `src/features/voice-input`, `src/native/voiceTranscription*.ts` | none (local only) |
| `src/features/prompt-links`, `shortcuts`, `showcase`, `development` | none |
| `turnThreadIntoProject`, `CustomSnoozeSheet*`, `thread-handoff-row`, `ThreadInlineFilePreview`, `thread-inspector-preview` | `projects.createFromThread`; `thread.snooze` UI |
| Android: `*.android.tsx`, `plugins/withAndroid*.cjs` (7), `app.config.ts` android block (`:287-305`) | none |
| `app.config.ts`: `widgetsPlugin` (`:129-198,429`), `sharingPlugin` (`:200-222,340`), `expo-notifications` (`:342-346`), `@clerk/expo` (`:350`), `expo-quick-actions` (`:353-365`), `extra.relay`/`extra.clerk` (`:442-453`), `ios.associatedDomains` (`:252-255`) | none. **Keep** `NSAllowsArbitraryLoads` (`:265-267`) and `NSLocalNetworkUsageDescription` (`:268-269`) for plain `http://` tailnet hosts. |

Routes to delete from `src/Stack.tsx` (imports at `:2-150`):
- `ComponentCatalog` `:603`
- all `PullRequest*` routes, `:626-715`
- `NewTaskBranch` `:739`
- `AddProjectRepository` `:753`, `AddProjectDestination` `:758`
- files routes `:773,778,848,856`
- `ThreadTerminal` `:799`, `ThreadDevicePreview` `:811`, `ThreadBrowserPreview` `:822`
- `ThreadReview` `:831,836`, `ThreadMcpApp` `:861`
- git and PR sheets `:914-954`
- `ConnectOnboarding` `:972`
- settings routes `:265,275,285,290,295,368,384-406,418,426,436,456,464,469`

### 6.2 Keep, but edit (cut features reached from kept code)

| Call site | Protocol it would otherwise need |
|---|---|
| `src/features/threads/ThreadFeed.tsx:1468`, `ThreadQueueControl.tsx:82` (`serverEnvironment.scheduledTasksLive`) | `scheduledTasks.subscribe`, opened on every thread view |
| `src/state/assets.ts:20,34` (`environmentProjectCloneListAtom`) | `subscribeProjectClones`, on every screen that shows favicons |
| `src/features/threads/ThreadRouteScreen.tsx:42-43,550` and others (`vcsEnvironment.status`) | `subscribeVcsStatus` / `vcs.refreshStatus` |
| `src/state/threadOutboxDelivery.ts:1001` | `git.preparePullRequestThread` |
| `src/features/projects/AddProjectScreen.tsx:81,1016-1027` | `server.discoverSourceControl`, `sourceControl.*` |
| `src/features/threads/ThreadComposer.tsx:70`, `new-task-flow-provider.tsx:41`, `src/components/ComposerEditor.tsx:30` | `peerLinks.list` |
| `src/state/use-thread-composer-state.ts:203` | `provider.uploadFeedback` |
| `src/features/keyboard/CommandPalette.tsx:283` | `server.prepareTriage` |
| `src/features/settings/SettingsEnvironmentDetailRouteScreen.tsx:64,68-69` | `server.updateServer*`, `server.updateProvider` |
| `src/state/queries.ts:106,268-288,445` | `pullRequests.*`, `vcs.listRefs`, diffs |
| `src/features/home/useThreadListActions.ts:89-93,213-220,566-569`, `ThreadDetailScreen.tsx:548-549` | the 9 no-op triage commands in §3.5 |
| `src/features/threads/thread-work-log.tsx:853` | `prepared-run.retry` |
| `src/connection/runtime.ts:3,23`; `src/persistence/layer.ts:3,15` | PR diff loader and PR cache (no protocol, but delete with the PR code) |
| `src/dependency-graph.test.ts:206` | asserts more than 500 files; lower it after the trim |

Other large kept files that import cut modules: `ThreadRouteScreen.tsx` (12 cut imports), `ThreadFeed.tsx` (about 14) and `ThreadDetailScreen.tsx` (about 10). `ComposerContextSheet.tsx:4,18-19,32` and `settings/appearance/useAppearanceCodeSurface.ts:7-8` reuse review, terminal and file code.

### 6.3 Client edits that add or adjust features

1. **No-auth host connection.** See §1.5.
2. **Todo display.** Remove `item.type === "todo_list"` from the skip at `src/lib/threadActivity.ts:2062`. The row then gets the existing summary ("Plan updated") and preview ("n/m completed") (`:641,693`). Alternatively, add a card that renders `steps` from the latest `todo_list` item.
3. **Skill token.** Change `src/features/threads/use-composer-command-menu.ts:196` from `` `$${item.skill.name} ` `` to `` `/skill:${item.skill.name} ` ``. Otherwise j3 rewrites `$name` on the server (§5).
4. **Optional.** Add `OrchestrationV2DispatchCommandError` to the draft-restore switch in `src/state/thread-outbox-model.ts:255-289`, so a server rejection puts the text back into the composer.

### 6.4 Protocol that becomes unnecessary

**HTTP:**
- `/oauth/token`, `/api/auth/websocket-ticket[/rpc|/media]`, `/api/auth/pairing-token`, `/api/auth/browser-session`
- `/api/connect/*`, `/api/pull-requests/diff`, `/api/preview-stream`, `/api/device-hub`
- `…/transcript`, `/api/projects*`, mcpOAuth, webhooks

**WS methods:** every entry in `WS_METHODS` (`C/rpc.ts:407-612`) and `ORCHESTRATION_V2_WS_METHODS` (`OV2:3549-3569`) other than M1–M22 and G1–G10, i.e.:
- `vcs.*`, `git.*`, `review.*`, `terminal.*`, `preview.*`, `device.*`
- `pullRequests.*`, `scheduledTasks.*`, `peerLinks.*`, `mcpApps.*`
- `provider.*`, `cloud.*`, `guestShare.*`, `sourceControl.*`, `projectClone.*`, `agentSessions.*`
- `shell.openInEditor`
- `projects.listEntries`, `projects.listIgnoredFiles`, `projects.searchContents`, `projects.createFromThread`
- `server.*` except `probe`, `refreshProviders`, `reportClientActivity`, `updateSettings` and `getConfig`
- every `subscribe*` except `subscribeServerConfig`
- `orchestration.getTurnDiff`, `getFullThreadDiff`, `searchThread`, `searchThreadStream`, `getWorkflowScript`, `getWorktreeThreadIds`

**Commands:** the 29 variants in §3.6.
