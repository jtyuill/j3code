# j3 wire protocol, version 2

The protocol between a j3 server (`j3-core`, embedded in the desktop or run as `j3-server`) and its clients (desktop, iOS). The earlier protocol it replaced and the mapping between them are recorded in `docs/m8.md` and in git history.

Design rules:

- One JSON message per WebSocket text frame. No batching of messages in arrays.
- Field names are `camelCase`. Optional fields are omitted, never `null`, unless `null` carries meaning (stated per field). Empty lists and `false` flags may be omitted too; absent means empty or false.
- Timestamps are RFC 3339 UTC strings with millisecond precision (`2026-11-09T11:18:00.000Z`).
- Ids are opaque strings. Clients never parse them.
- Unknown fields are ignored by both sides. Unknown enum values are a decode error on the server and are kept as "unknown" by clients, so a newer server can add item kinds without breaking an older client.
- The protocol version changes only for breaking changes. Additions are announced through `capabilities` (§2).

## 1. Transport

| | |
|---|---|
| Discovery | `GET /api/v1/info` (§2) |
| WebSocket | `GET /api/v1/ws?protocol=2` |
| Attachment upload | `POST /api/v1/attachments` (§8) |
| Attachment download | `GET /api/v1/attachments/<id>` (§8) |
| Project image download | `GET /api/v1/assets/<token>` (§8) |

A WebSocket upgrade whose `protocol` query parameter is missing or differs from the server's version is refused with HTTP `426` and body `{"code":"protocol_mismatch","message":…,"protocol":2}`.

There is no authentication. Servers listen on loopback and the host's Tailscale address only; the tailnet is the boundary.

## 2. Discovery

`GET /api/v1/info` → `200`:

```json
{
  "serverId": "env-ef26e90a70c8ff1b",
  "name": "shieldy",
  "protocol": 2,
  "version": "0.1.0",
  "capabilities": ["terminal", "git", "files", "usage", "goals", "btw"],
  "platform": "linux",
  "home": "/home/jacob"
}
```

| Field | Meaning |
|---|---|
| `serverId` | Stable for the life of the server's database. Clients key cached state and saved hosts by it. |
| `name` | Host name, for display. |
| `protocol` | The one protocol version constant (`j3_protocol::PROTOCOL`). |
| `version` | j3 build version, for display and bug reports. |
| `capabilities` | Optional method groups this server serves. A client hides features whose capability is absent. |
| `platform`, `home` | Host OS family and the server user's home directory, for the project picker. |

## 3. Envelope

Every frame is an object with a `t` (type) field.

Client → server:

| `t` | Fields | Meaning |
|---|---|---|
| `call` | `id`, `method`, `params` | Unary call or stream subscription. `id` is a non-negative integer unique among the connection's open calls. `params` is an object; `{}` when the method takes none. |
| `ack` | `id`, `n` | Grants stream `id` `n` more `items` frames (§3.2). |
| `cancel` | `id` | Ends call `id`. The server answers with `err` `cancelled` unless the call already ended. |
| `ping` | — | Keepalive (§3.3). |

Server → client:

| `t` | Fields | Meaning |
|---|---|---|
| `ok` | `id`, `result` | Unary success. Ends the call. |
| `items` | `id`, `items` | Stream batch: a non-empty array of stream items. |
| `end` | `id` | Normal stream end. Ends the call. |
| `err` | `id`, `error` | Failure of a call or a stream. Ends the call. `id` is `null` for a connection-level error, after which the server closes the socket. |
| `pong` | — | Answer to `ping`. |

### 3.1 Errors

```json
{"code": "rejected", "message": "The title can't be empty.", "data": {}}
```

| `code` | Meaning | Client handling |
|---|---|---|
| `bad_request` | `params` failed to decode, or the frame is malformed. `message` names the field. | Bug; log it. |
| `unknown_method` | The server has no such method. | Bug, or a capability check was skipped. |
| `not_found` | A named entity (thread, project, item, request, terminal…) does not exist. `data` has `{kind, id}`. | Drop the stale reference. |
| `rejected` | The server refused a valid command for a domain reason. `message` is written for the user. | Show `message`. |
| `conflict` | The command raced another change (e.g. a queue entry already started). | Refresh and let the user retry. |
| `too_large` | An upload or a request exceeds a limit. `data` has `{limit}`. | Show `message`. |
| `unavailable` | A host tool failed or is missing (omp, git, a PTY). `message` is the tool's error. | Show `message`. |
| `cancelled` | The call was cancelled by the client. | None. |
| `internal` | Server bug. | Show a generic failure; log `message`. |

`data` is optional and method-specific; each method lists the codes it returns besides `bad_request`, `cancelled` and `internal`, which any call may return.

### 3.2 Stream flow control

A stream starts with a window of 8 `items` frames. Each `items` frame uses one unit; the server sends nothing more on that stream when the window is empty. The client sends `ack {id, n}` after it has consumed frames, normally `n = 1` per frame. Items produced while the window is empty are merged into the next frame. For entity streams the server may coalesce merged updates of the same entity (§6.4) but never reorders sequences.

### 3.3 Keepalive

The client sends `ping` every 5 s. Any inbound frame counts as liveness. A client that receives nothing for 15 s closes the socket and reconnects. The server answers every `ping` and closes connections silent for 30 s.

### 3.4 Idempotent commands

Methods that change state take an optional `commandId` (client-generated, unique). The server stores the result of each `commandId` and answers a repeat with the stored result without acting again, so a client may resend a command after a reconnect without knowing whether the first send arrived.

## 4. Entities

### 4.1 Project

| Field | Type | Notes |
|---|---|---|
| `id` | string | |
| `title` | string | |
| `path` | string | Absolute directory on the server host. |
| `defaultModel` | `ModelRef?` | Used for new threads in this project. |
| `createdAt`, `updatedAt` | time | |

### 4.2 Thread

The thread index row: everything the thread list needs, without the conversation.

| Field | Type | Notes |
|---|---|---|
| `id` | string | |
| `projectId` | string | |
| `title` | string | |
| `createdAt`, `updatedAt` | time | |
| `archivedAt` | time? | |
| `status` | `ThreadStatus` | §4.2.1 |
| `model` | `ModelRef` | Effective model of the next turn. |
| `mode` | `"default"` \| `"plan"` | |
| `activeTurnId` | string? | The running turn. |
| `pendingRequest` | `RequestSummary?` | The oldest open request (§4.5), for the list badge. |
| `queued` | integer | Number of queue entries. |
| `lastMessage` | `MessageSummary?` | `{role: "user" \| "assistant", text, at}`; `text` cut to 300 characters. |
| `latestTurn` | `TurnSummary?` | §4.3 |
| `latestUserMessageAt` | time? | Newest user message that reached omp (queued ones excluded). |
| `goal` | `Goal?` | §4.4, kind `goal`. |
| `triage` | `Triage` | §4.2.2 |

#### 4.2.1 ThreadStatus

| Value | When |
|---|---|
| `idle` | No active turn. |
| `starting` | A turn was accepted and omp is starting. |
| `running` | omp is working on the active turn. |
| `waiting` | The active turn is blocked on an open request. |
| `stopping` | A stop was requested and omp has not settled. |
| `failed` | The last turn failed and no turn has started since. |

#### 4.2.2 Triage

The per-thread state of the Inbox workflow. Semantics, preconditions and every transition are in `docs/triage.md`; this table maps its fields.

| Field | Type | `docs/triage.md` (v1 name) |
|---|---|---|
| `visitedAt` | time? | `lastVisitedAt`, the read watermark |
| `settled` | `"settled"` \| `"active"` ? | `settledOverride`; absent is "no explicit choice" |
| `settledAt` | time? | `settledAt` |
| `unsettledAt` | time? | `unsettledAt` |
| `snoozedUntil` | time? | `snoozedUntil` |
| `snoozedAt` | time? | `snoozedAt` |
| `lastWakeAt` | time? | `lastSnoozeWakeAt` |
| `pinnedAt` | time? | `pinnedAt` |
| `pinOrder` | string? | `pinOrderKey` (order key, triage §5.4) |
| `activeOrder` | string? | `activeOrderKey` |
| `autoSettleOffAt` | time? | `autoSettleDisabledAt` |
| `bumpedAt` | time | `updatedAt` as triage uses it (the bump rules of triage §6); separate from the row's `updatedAt`, which changes on every write |

The derived inputs triage reads are on the thread row itself: `status`, `activeTurnId`, `latestTurn`, `latestUserMessageAt` and `pendingRequest`.

### 4.3 Turn

One user prompt and everything omp did for it, until omp reports `session_settled`.

| Field | Type | Notes |
|---|---|---|
| `id` | string | |
| `threadId` | string | |
| `ordinal` | integer | 1-based, per thread, increasing; may have gaps. |
| `status` | `"running"` \| `"waiting"` \| `"completed"` \| `"failed"` \| `"stopped"` | |
| `model` | `ModelRef` | |
| `requestedAt` | time | |
| `startedAt`, `endedAt` | time? | |
| `error` | `{message, kind}`? | `kind`: `"usage_limit"`, `"provider"`, `"omp"`, `"crashed"`, `"refused"`. `usage_limit` drives the Inbox "Limited" state. |
| `wokenBy` | `"background_job"`? | Set when omp started the turn itself (a background job finished), not a user prompt. |

Queued prompts are not turns; they are queue entries (§4.6) until they start.

`TurnSummary` (the thread row's `latestTurn`): `{id, status, requestedAt, startedAt?, endedAt?, errorKind?}`.

### 4.4 Item

One row of the conversation feed.

| Field | Type | Notes |
|---|---|---|
| `id` | string | |
| `threadId`, `turnId` | string | |
| `ordinal` | integer | Feed order within the thread; strictly increasing. |
| `status` | `"running"` \| `"completed"` \| `"failed"` \| `"cancelled"` | |
| `startedAt` | time | |
| `endedAt` | time? | |
| `kind` | string | One of the kinds below; the kind's fields sit beside the common fields. |

| `kind` | Fields |
|---|---|
| `user` | `text` (as the user wrote it; §8 says how attachments reach omp), `attachments: Attachment[]`, `delivery: "prompt" \| "steer"` |
| `text` | `text` (assistant answer, Markdown) |
| `thinking` | `text` |
| `tool` | `callId`, `name`, `intent?` (omp's one-line `i`), `input` (object), `output?: ToolOutput`, `detail?: ToolDetail` |
| `todo` | `phases: [{name, tasks: [{text, status}]}]` (`status` is omp's: `pending`, `in_progress`, `completed`, `abandoned`, `blocked`) |
| `subagent` | `subagentId`, `agent`, `description?` (omp's short task description), `prompt`, `model?`, `progress?`, `result?` |
| `goal` | `objective`, `state` (omp's: `active`, `paused`, `budget-limited`, `complete`, `dropped`), `tokensUsed?`, `budget?` |
| `compaction` | `summary?`, `tokensBefore?` |
| `notice` | `level: "info" \| "warning" \| "error"`, `text` |
| `stop` | `requestedAt`, `stoppedAt?` (a user stop of the turn) |

`ToolOutput`: `{text, truncated?: boolean, images?: [{attachmentId, mimeType}], exitCode?: integer, background?: {jobId, state}}`.

`ToolDetail`, by `type`:

| `type` | Fields | From |
|---|---|---|
| `command` | `command` | `bash` |
| `file_change` | `path`, `diff?`, `additions?`, `deletions?`, `content?` (whole file for `write`) | `edit`, `write` naming a file |
| `search` | `pattern`, `matches?: [{path, line?, text?}]` | `grep`, `find`, `glob` |
| `web_search` | `query`, `results?: [{title, url, snippet?}]` | `web_search` |

`ask` tool calls have no `tool` item; their questions are requests (§4.5).

### 4.5 Request

A question omp is waiting on (`ask`, `select`, `confirm`, `input`, `editor`).

| Field | Type | Notes |
|---|---|---|
| `id` | string | |
| `threadId`, `turnId` | string | |
| `kind` | `"ask"` \| `"select"` \| `"confirm"` \| `"input"` \| `"editor"` | |
| `status` | `"open"` \| `"answered"` \| `"dismissed"` \| `"expired"` | |
| `title` | string? | |
| `questions` | `Question[]` | `ask`: one per question; the other kinds: exactly one. |
| `answers` | `Answer[]`? | Once answered. |
| `openedAt` | time | |
| `expiresAt` | time? | omp's timeout. |
| `closedAt` | time? | |

`Question`: `{id, text, header?, options?: [{label, description?}], multiple?: boolean, allowOther?: boolean, default?: string}`.
`Answer`: `{questionId, values: string[], other?: string}`.

`RequestSummary` (on the thread row): `{id, kind, title?, openedAt}`.

### 4.6 QueueEntry

A prompt waiting for the active turn to end.

| Field | Type | Notes |
|---|---|---|
| `id` | string | |
| `threadId` | string | |
| `position` | integer | 0 is next. |
| `text` | string | |
| `attachments` | `Attachment[]` | |
| `model` | `ModelRef` | |
| `held` | boolean | True after a stop with "hold queue"; held entries wait for `queue.resume`. |
| `createdAt` | time | |

### 4.7 Shared types

- `ModelRef`: `{provider, id, thinking?}`; `thinking` is omp's level (`off`, `minimal`, `low`, `medium`, `high`, `xhigh`).
- `Attachment`: `{id, name, mimeType, size}`. Bytes are fetched through §8.

## 5. Methods

Every method lists its params, its result (unary) or items (stream), and the error codes it returns besides the ones any call may return (§3.1). `?` marks an optional field. State-changing methods also take `commandId?` (§3.4), not repeated below. Unless stated, a state-changing method returns `{sequence}`: the sequence of its commit, so a client knows when its own change has reached its streams.

### 5.1 server

| Method | Params | Result | Errors |
|---|---|---|---|
| `server.watch` | — | stream of `Catalog` (§5.1.1): the current one first, then one per change | — |
| `server.refresh` | `projectId?`, `fresh?` | `Catalog`; with `projectId`, the project's own slash commands are included; `fresh` reloads omp's command and skill lists | `not_found`, `unavailable` |
| `server.usage` | — | `Usage` (§5.1.2) | `unavailable` |

#### 5.1.1 Catalog

```json
{
  "models": [{"provider": "anthropic", "id": "claude-opus-5-5", "name": "Claude Opus 5.5",
              "thinking": ["off", "low", "medium", "high", "xhigh"], "default": true}],
  "skills": [{"name": "unslop", "description": "…"}],
  "commands": [{"name": "compact", "description": "…", "hint": "[focus]"}],
  "projectCommands": {"<projectId>": [{"name": "deploy", "description": "…"}]},
  "status": "ready",
  "message": null
}
```

`commands` always has `btw` (§5.13) on a server with the `btw` capability: omp lists it in its own interface only, and a client answers a `/btw` draft through `btw.ask` instead of sending it.

`thinking` is absent for models without reasoning. `status` is `ready`, `warning` (omp answered with problems, `message` says which) or `error` (omp could not be queried).

#### 5.1.2 Usage

`{generatedAt, accounts: [{provider, account?: {email?, plan?}, fetchedAt?, limits: [{label, window?: {label, resetsAt?}, used, limit, unit}]}]}`, from `omp usage --json` on the server host.

### 5.2 shell

| Method | Params | Items | Errors |
|---|---|---|---|
| `shell.watch` | `after?` | §6.5 | — |

The shell holds every project and every thread that is not deleted, archived ones included; clients filter on `archivedAt`.

### 5.3 projects

| Method | Params | Result | Errors |
|---|---|---|---|
| `projects.create` | `title`, `path`, `defaultModel?` | `{project, sequence}` | `rejected` (path missing, not a directory, already a project) |
| `projects.update` | `projectId`, `title?`, `defaultModel?` (`null` clears) | `{project, sequence}` | `not_found`, `rejected` |
| `projects.delete` | `projectId`, `force?` | `{sequence}` | `not_found`, `rejected` (has threads and not `force`) |
| `projects.browse` | `partialPath` | `{parentPath, entries: [{name, path}]}`: the directories matching the typed prefix (`~` is the server user's home; a relative path starts there), sorted | `rejected` (unreadable) |
| `projects.listEntries` | `projectId`, `dir?` | `{entries: [{path, kind: "file" \| "dir", ignored?}], truncated}`; at most 2,000 entries, `.git` left out | `not_found`, `rejected` |
| `projects.searchEntries` | `projectId`, `query`, `limit?` (≤ 200), `kind?`, `imagesOnly?` | `{entries: [{path, kind}], truncated}`: fuzzy match over a gitignore-aware walk, reused for 10 s | `not_found` |
| `projects.readFile` | `projectId`, `path`, `allowMissing?` | `{path, text, size, truncated, exists}`; at most 1 MiB. `path` is relative to the project (it may not leave it) or an absolute host path. A missing file with `allowMissing` is `exists: false` | `not_found` (project; or `{kind: "file", id: path}` for a missing file without `allowMissing`), `rejected` (binary, a relative path leaving the project) |
| `projects.imageUrl` | `projectId`, `path` | `{url, expiresAt}` for an image file in the project (§8) | `not_found`, `rejected`, `too_large` |

### 5.4 threads

| Method | Params | Result | Errors |
|---|---|---|---|
| `threads.create` | `projectId`, `model`, `mode?`, `title?`, `text`, `attachments?: string[]` (attachment ids) | `{thread, sequence}`; the first turn starts at once and the title is generated from the text unless given | `not_found`, `rejected` |
| `threads.watch` | `threadId`, `after?` | §6.6 | `not_found` |
| `threads.get` | `threadId` | `ThreadSnapshot` (§6.6) without subscribing | `not_found` |
| `threads.rename` | `threadId`, `title` | `{sequence}` | `not_found`, `rejected` (blank) |
| `threads.archive` / `threads.unarchive` | `threadId` | `{sequence}` | `not_found` |
| `threads.delete` | `threadId`, `onlyIfUnused?` | `{sequence}`; deleting a missing thread succeeds | `rejected` (`onlyIfUnused` and the thread has turns) |
| `threads.setMode` | `threadId`, `mode` | `{sequence}` | `not_found` |

### 5.5 turns

| Method | Params | Result | Errors |
|---|---|---|---|
| `turns.send` | `threadId`, `text`, `attachments?: string[]`, `model?`, `delivery` | `{sequence, turnId?, queueEntryId?}` | `not_found`, `rejected`, `conflict` |
| `turns.stop` | `threadId`, `holdQueue?`, `force?` | `{sequence}` | `not_found` |

`delivery`:

| Value | Thread idle | A turn is active |
|---|---|---|
| `auto` | starts a turn | queues |
| `queue` | starts a turn | queues |
| `steer` | starts a turn | steers the active turn (`delivery: "steer"` user item) |
| `restart` | starts a turn | stops the active turn and starts this one first |

`turns.stop` asks omp to abort the active turn. `holdQueue` holds every queue entry until `queue.resume`. `force` kills the omp process instead, for a thread that does not answer an abort.

### 5.6 requests

| Method | Params | Result | Errors |
|---|---|---|---|
| `requests.answer` | `threadId`, `requestId`, `answers: Answer[]` | `{sequence}` | `not_found`, `conflict` (no longer open) |
| `requests.dismiss` | `threadId`, `requestId` | `{sequence}` | `not_found`, `conflict` |

`confirm` and `select` requests carry their choices as `options`; the answer's `values` holds the chosen option label (`confirm`: `"Yes"` or `"No"`). For `input` and `editor`, `values` holds the one text. For `ask`, `values` holds the chosen labels and `other` the typed answer.

### 5.7 queue

| Method | Params | Result | Errors |
|---|---|---|---|
| `queue.edit` | `threadId`, `entryId`, `text`, `attachments?: string[]` (absent keeps them) | `{sequence}` | `not_found` (thread), `conflict` (the entry is no longer queued: started, cancelled or unknown) |
| `queue.cancel` | `threadId`, `entryId` | `{sequence}` | `not_found` (thread), `conflict` (no longer queued) |
| `queue.steer` | `threadId`, `entryId` | `{sequence}`: the entry is sent into the active turn as a steer | `not_found` (thread), `conflict` (no longer queued, or omp has not started the turn yet), `rejected` (no active turn) |
| `queue.start` | `threadId`, `entryId` | `{sequence}`: the entry starts now, stopping the active turn if one runs | `not_found` (thread), `conflict` (no longer queued) |
| `queue.resume` | `threadId` | `{sequence}`: clears `held` and starts the next entry if the thread is idle | `not_found` |

### 5.8 triage

The Inbox workflow's commands; preconditions, refusal texts and effects are in `docs/triage.md` §4.2, §6 and §7. Every command is `rejected` on an archived thread, except `triage.visit` and `triage.markUnread`.

| Method | Params | `docs/triage.md` (v1 command) |
|---|---|---|
| `triage.visit` | `threadId`, `visitedAt`, `opened?` | `thread.visit` |
| `triage.markUnread` | `threadId` | `thread.mark-unread` |
| `triage.settle` | `threadId`, `settledAt?` | `thread.settle` |
| `triage.unsettle` | `threadId` | `thread.unsettle` |
| `triage.snooze` | `threadId`, `until` | `thread.snooze` |
| `triage.wake` | `threadId` | `thread.unsnooze` |
| `triage.pin` | `threadId`, `order?` | `thread.pin` |
| `triage.unpin` | `threadId` | `thread.unpin` |
| `triage.orderPinned` | `threadId`, `order` | `thread.pin.reorder` |
| `triage.orderActive` | `threadId`, `order` | `thread.active.reorder` |
| `triage.setAutoSettle` | `threadId`, `enabled` | `thread.auto-settle.set` |

All return `{sequence}`; errors `not_found`, `rejected`.

### 5.9 goals and subagents

| Method | Params | Result | Errors |
|---|---|---|---|
| `goals.create` | `threadId`, `objective`, `budget?` (tokens) | `{sequence}` | `not_found`, `rejected` (a goal is active or paused, plan mode, goals disabled in omp) |
| `goals.pause` / `goals.resume` / `goals.drop` | `threadId` | `{sequence}` | `not_found`, `rejected` |
| `subagents.stop` | `threadId`, `subagentId` | `{sequence}` | `not_found`, `conflict` (already ended) |
| `subagents.transcript` | `threadId`, `subagentId`, `fromByte` | `{nextByte, reset, more, entries: TranscriptEntry[]}`: entries appended since `fromByte`, whole lines only, at most 4 MiB per call; `reset` when the file is shorter than `fromByte` | `not_found`, `unavailable` |

`TranscriptEntry`, by `kind`: `user {text}`, `assistant {text}`, `thinking {text}`, `toolCall {name, intent?, input}`, `toolResult {name, text, failed}`, `error {text}`.

### 5.10 git

Capability `git`.

| Method | Params | Result / items | Errors |
|---|---|---|---|
| `git.status` | `projectId` | `GitStatus` (below); the pull request comes from `gh`, cached 60 s | `not_found`, `unavailable` |
| `git.pull` | `projectId` | `{status: "pulled" \| "up_to_date", branch, upstream?}`; fast-forward only | `not_found`, `rejected`, `unavailable` |
| `git.run` | `projectId`, `action: "commit" \| "push" \| "pr" \| "commit_push" \| "commit_push_pr"`, `message?` (absent: omp writes it) | stream of items tagged by `type`: `phase {phase}`, then `finished {result: GitRunResult}`; a failure ends the stream with `err` whose `data` is `{phase}`. A commit stages every change (`git add -A`) | `not_found`, `rejected` (j3 refuses the action, e.g. detached HEAD, no GitHub remote for `pr`), `unavailable` (git, gh or omp failed) |

`phase`: `"branch"` \| `"commit"` \| `"push"` \| `"pr"`.

`GitStatus`:

| Field | Type | Notes |
|---|---|---|
| `isRepo` | boolean | False outside a git repository; the other fields are then empty. |
| `branch` | string? | Absent on a detached HEAD. |
| `onDefaultBranch` | boolean | The branch is the remote's default branch. |
| `hasRemote` | boolean | A remote to push to exists. |
| `github` | boolean | The remote is on GitHub, so pull requests work. |
| `hasUpstream` | boolean | |
| `ahead`, `behind` | integer | Commits relative to the upstream. |
| `dirty` | boolean | Uncommitted changes, untracked files included. |
| `files` | `[{path, additions, deletions}]` | Changed files. |
| `additions`, `deletions` | integer | Totals over `files`. |
| `pullRequest` | `{url, state: "open" \| "closed" \| "merged"}`? | The branch's newest pull request. |

`GitRunResult`: `{title, description?, next?}`, shown as a success toast. `next` is its button, by `kind`: `open_pr {label, url}` or `run {label, action}` (a follow-up `git.run`).

Commit messages come from `omp commit`; pull request text from `omp -p` (PLAN.md "omp integration").

### 5.11 terminal

Capability `terminal`. A terminal is a PTY shell owned by a thread, started in the thread's project directory.

| Method | Params | Result / items | Errors |
|---|---|---|---|
| `terminal.attach` | `threadId`, `terminalId`, `cols?`, `rows?`, `env?` (object of strings), `restart?` | stream of items tagged by `type` (below); starts the shell if it is not running | `not_found`, `unavailable` |
| `terminal.write` | `threadId`, `terminalId`, `data` (1–65,536 characters) | `{}` | `not_found` |
| `terminal.resize` | `threadId`, `terminalId`, `cols` (1–1000), `rows` (1–500) | `{}` | `not_found` |
| `terminal.close` | `threadId`, `terminalId?` (absent: all of the thread's), `deleteHistory?` | `{}`; closing an unknown terminal succeeds | — |

Terminal streams use a window of 64 frames instead of 8 (§3.2), so output keeps flowing while the client renders.

`terminal.attach` items:

| `type` | Fields | Notes |
|---|---|---|
| `snapshot` | `history`, `state: "running" \| "exited" \| "failed"`, `exitCode?`, `exitSignal?` | Always first. Replay `history` into an empty screen. |
| `output` | `data` | |
| `exited` | `exitCode?`, `exitSignal?` | |
| `failed` | `message` | The shell could not run; the stream ends. |
| `cleared` | — | Clear the screen. |
| `closed` | — | The terminal was closed; the stream ends. |
| `restarted` | as `snapshot` | A new shell replaced the old one; reset the screen. |

### 5.12 attachments

| Method | Params | Result | Errors |
|---|---|---|---|
| `attachments.delete` | `attachmentId` | `{}`; deleting an unknown id succeeds | — |

Uploads are plain HTTP (§8).

### 5.13 btw

Capability `btw`. A side question (omp's `/btw`): omp answers it from the thread's conversation, beside whatever the thread is doing, with no tools. The question and its answer are not turns or items and no client streams them but the caller. omp keeps them in the session's side-question history.

| Method | Params | Items | Errors |
|---|---|---|---|
| `btw.ask` | `threadId`, `question`, `recordId?` (follow up in that topic) | stream of items tagged by `type` (below); it ends after the `record` whose last turn is no longer `running` | `not_found`, `rejected` (blank question, or omp refused: unknown topic, no model), `unavailable` (omp could not start, or stopped before it answered), `conflict` (another `btw.ask` on the thread replaced this one) |

| `type` | Fields | Notes |
|---|---|---|
| `record` | `record: BtwRecord` | The whole topic: first, and on every status change. |
| `delta` | `recordId`, `append` | Text appended to the answer of the topic's last turn. |

`BtwRecord`: `{id, turns: BtwTurn[]}`, the question and its follow-ups, oldest first. `BtwTurn`: `{question, answer, status: "running" \| "complete" \| "cancelled" \| "error", error?}`; `answer` is Markdown.

omp answers one side question per thread at a time: a second `btw.ask` cancels the first, whose stream ends with `conflict`. Cancelling the call (`cancel`, or closing the socket) cancels the question. `btw.ask` starts the thread's omp if it is not running.

## 6. Streams and sequence

### 6.1 Sequence

The server keeps one integer, `sequence`, that increases with every committed change to any project, thread, turn, item, request or queue entry, and persists it across restarts. Every delta carries the `sequence` of its commit. Sequences seen on one stream have gaps; they only increase.

### 6.2 Subscribing and resuming

`shell.watch` and `threads.watch` take an optional `after` sequence. The server answers with either:

- a `snapshot` item (always when `after` is absent, when `after` is newer than the server's sequence, or when the changes since `after` are no longer available), or
- the deltas with `sequence > after`,

followed by one `synced` item, then live deltas. One commit can change several entities, so several deltas may carry the same `sequence`; a server sends all of a commit's deltas together and never splits a commit across a resume. A client replaces its state wholesale on every `snapshot` and drops any delta whose `sequence` is lower than the highest it has applied (equal is applied). A server that keeps no change history answers every subscription with a snapshot; that is always valid.

### 6.3 Item streaming

While an item's text grows, the server sends `item.delta` instead of the whole item. Structural changes (status, detail, images, the end of the item) come as `item` with the whole item. A snapshot always carries whole items.

### 6.4 Coalescing

The server may merge consecutive changes to one entity within 50 ms into one item carrying the last change's sequence. Merged `item.delta`s concatenate their `append`.

### 6.5 `shell.watch` items

| `type` | Fields |
|---|---|
| `snapshot` | `sequence`, `projects: Project[]`, `threads: Thread[]` |
| `synced` | — |
| `project` | `sequence`, `project` (upsert) |
| `project.removed` | `sequence`, `projectId` |
| `thread` | `sequence`, `thread` (upsert) |
| `thread.removed` | `sequence`, `threadId` (deleted) |

### 6.6 `threads.watch` items

| `type` | Fields |
|---|---|
| `snapshot` | `ThreadSnapshot`: `sequence`, `thread`, `turns: Turn[]`, `items: Item[]` (by `ordinal`), `requests: Request[]` (open ones and those of the latest turn), `queue: QueueEntry[]` (by `position`) |
| `synced` | — |
| `thread` | `sequence`, `thread` |
| `turn` | `sequence`, `turn` (upsert) |
| `item` | `sequence`, `item` (upsert) |
| `item.delta` | `sequence`, `itemId`, `field: "text" \| "output"`, `append` |
| `request` | `sequence`, `request` (upsert) |
| `queue` | `sequence`, `queue: QueueEntry[]` (the whole queue) |
| `thread.removed` | `sequence`; the stream then ends |

## 7. omp → v2

How the server derives the entities from omp's RPC events and session file. The same mapping serves live events and the session replay (`j3-core` `v2::tools`, `v2::replay`), so a thread looks the same streamed or rebuilt.

| omp | v2 |
|---|---|
| `prompt` accepted (user message) | `Turn` `running` + `user` item; a steer is a `user` item with `delivery: "steer"` in the active turn |
| `btw_record`, `btw_delta` | `btw.ask` items (§5.13); never thread state |
| `agent_start` … `session_settled` | the turn's lifetime; `session_settled` ends it, not `agent_end` (background jobs can wake the agent after it yields) |
| assistant `thinking` / `text` blocks | `thinking` / `text` items, streamed with `item.delta` |
| tool call (except `ask`, `todo`) | `tool` item; `detail` per §4.4 |
| tool result | `output`, `status` (`failed` on `isError`), images as attachments, `detail` filled from `details` (`diff`, `files`, `response.sources`, `exitCode`) |
| tool result with `details.async` | `output.background` `running`; the job's result (an `async-result` delivery or a `wait` result) replaces `output.text` and sets the job state |
| `todo` tool | `todo` item; phases from the result's `details.phases` |
| `task` tool | `tool` item plus one `subagent` item per spawned agent; each ends with its `<task-result>` |
| `ask` / `select` / `confirm` / `input` / `editor` UI request | `Request` `open`; the turn becomes `waiting` |
| assistant `stopReason: "aborted"` | `stop` item; turn `stopped` |
| assistant `stopReason: "error"` | turn `failed`, `error.message` = omp's `errorMessage` |
| compaction start / end | `compaction` item |
| goal state | `goal` item on each change; the thread row's `goal` |
| `model_change`, `thinking_level_change` | the thread's `model` |

## 8. Attachments and assets

**Upload.** `POST /api/v1/attachments` with the file bytes as the body, `Content-Type` set to its MIME type and `Content-Length` set, and query `name=<file name>`. Images (`image/gif`, `image/jpeg`, `image/png`, `image/webp`) may be up to 10 MiB, other files up to 50 MiB. The answer is `201` with the `Attachment`; `413` with an error object for too much data, `400` for a missing name or length.

**Download.** `GET /api/v1/attachments/<id>` serves the bytes with the stored MIME type, `ETag: "<id>"`, `Cache-Control: private, max-age=3600`, and single-range `Range` support. Images are served inline, other files as downloads with a sandboxing CSP.

**How omp gets them.** An image of one of the four types above goes to omp inline with the message. Any other file goes as an omp file mention: the server appends one `@"<path>"` per file to the text it gives omp, on their own lines after the user's text, where `<path>` is a path on the server host that ends in the file's upload name. omp reads a mentioned file into the prompt itself (text as text, PDFs and office documents converted, images and videos as pictures; files it cannot read are named to the model, which can still open them with its tools), exactly as for an `@path` the user typed. The `user` item keeps the text as written. A steer names the files the same way, but omp reads mentions only on a prompt.

Attachment ids are 128-bit random and act as the only capability needed to read the bytes; with no authentication on the tailnet, that is the boundary (PLAN.md "Auth").

**Project images.** `projects.imageUrl` grants a URL (`/api/v1/assets/<token>`) to an image file inside a project, valid for an hour.

Tool output images are attachments (`ToolOutput.images[].attachmentId`), so no separate asset kind is needed.
