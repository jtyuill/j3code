# j3 protocol extensions: omp goals, subagents and usage

j3 adds three omp features to the t3 wire protocol (`ios-surface.md`): **goal mode**, **subagents** and **usage limits**. Rust types: `j3_protocol::t3::orchestration::{ProviderGoal, Subagent, SubagentItem}`, `j3_protocol::t3::commands::SubagentStopCommand`, `j3_protocol::j3`. Server mapping: `crates/j3-core/src/engine/model/agents.rs`. Client API: `j3_client::ThreadState::{goal, subagents, subagent}` and `j3_client::Connection::{create_goal, pause_goal, resume_goal, drop_goal, stop_subagent, subagent_transcript, usage}`.

## Placement

Everything the server sends on t3's methods uses shapes the t3 contracts already define. `packages/contracts` decodes every frame and the iOS client renders it as it is. Only three things are new: four `j3.goal.*` variants of `orchestration.dispatchCommand`, the `j3.getUsage` method ([Usage](#usage)) and the `j3.getSubagentTranscript` method ([Subagent transcripts](#subagent-transcripts)). t3 clients never send any of them.

| Data | Wire shape | Why |
|---|---|---|
| Goal | `OrchestrationV2ProviderGoal` in `ProviderThread.goal` (projection, `provider-thread.updated`) and in `ThreadShell.goal` (shell rows) | t3 already models a provider goal (Codex goals). omp's goal maps onto it field for field; only omp's `id`, `createdAt`/`updatedAt` and `mode` have no place. Mobile shows it as a goal chip ("Pursuing goal", "Goal paused", …) on the thread screen and list. |
| Subagent | `OrchestrationV2Subagent` in the projection's `subagents` (`subagent.updated`), plus one `subagent` turn item each (`turn-item.updated`) | t3's provider-native subagent fits omp's: prompt, status, progress text, result, model, start and end times. Mobile renders the rows read-only. It offers no stop button: `resolveSubagentStopTarget` only targets workflows and `claudeAgent`/app-owned subagents. |
| Goal control | `j3.goal.create`, `j3.goal.pause`, `j3.goal.resume`, `j3.goal.drop` | t3 has no command for these: its server reads `/goal …` text messages instead. j3 sends the `/goal` text to omp unchanged, so typed commands are needed. |
| Subagent stop | t3's own `subagent.stop` | It already means "stop one subagent of a run without stopping the run". |

Verified with `tests/conformance` (37/37 against a server with these changes). A one-off contracts client then ran one real omp subagent and a real goal create/pause on the same server and decoded `subscribeThread` and `subscribeShell` through the contracts' RPC client: no `unknown-event`; every `subagent.updated` and `subagent` item, the snapshot's `subagents` and `providerThreads[].goal`, and the shell row's `goal` all came through typed.

t3's thread triage (settle, snooze, pin, reorder, mark-unread, visit, auto-settle) is **not** an extension. j3 implements t3's commands, events and fields unchanged (`ios-surface.md` §3.5), so t3 clients need no changes. Server: `crates/j3-core/src/engine/model/triage.rs`. Client: `j3_client::Connection`. Where j3 differs from t3:

- Auto-settle uses t3's defaults only: 3 days without activity, swept at startup and every 5 minutes. There are no pull-request rules, no server settings and no per-project scope, so j3 does not advertise `threadAutoSettlementScope`.
- `thread.visit` moves the read watermark but does not record `opens` (t3 uses them only to rank search results).
- Every j3 runtime request blocks, and j3 has no automatic queued messages (notifications, delegated completions). So `thread.settle` refuses any pending request or queued message instead of cancelling the non-blocking ones, and every sent message wakes a snoozed thread.
- When one commit makes several thread changes (a sent message that un-settles and wakes the thread), it emits one thread event, not one per change. Its payload is the final thread.

## omp settings

- `goal.enabled` (default `true`): goal mode and the hidden `goal` tool. With it off, `j3.goal.create` fails with omp's message.
- `goal.continuationModes` (default `["interactive"]`): goals continue on their own over RPC only when the list contains `"rpc"`. Without it, a goal adds the `goal` tool and goal context, and the agent works on it only when prompted. With it, omp starts hidden goal-continuation turns; j3 shows each one as an agent-started run (like a background-job wake). To turn it on, put `goal: {continuationModes: [interactive, rpc]}` in `~/.omp/agent/config.yml`.
- Subagents need no setting. j3 sends `set_subagent_subscription {level:"progress"}` on every omp process it starts. That level carries lifecycle and progress frames but not the subagents' own session events.

## Goals

Sources: `get_state.goal` (when omp starts, and on every refresh after `agent_end`), `goal_updated` events, and the result of j3's own `goal` commands.

| omp `Goal` | `ProviderGoal` |
|---|---|
| `objective` | `objective` (trimmed) |
| `status`: `active` / `paused` / `budget-limited` / `complete` | `status`: `active` / `paused` / `budget_limited` / `complete` |
| `status: dropped`, or `goal: null` | no goal (`goal: null` on the provider thread; the shell omits `goal`) |
| `tokensUsed` | `tokensUsed` |
| `tokenBudget` | `tokenBudget` |
| `timeUsedSeconds` (float) | `timeUsedSeconds` (rounded) |

- omp clears `get_state.goal` once a goal completes. j3 keeps showing the `complete` goal until a new goal replaces it or `j3.goal.drop` clears it. For a completed goal, j3 handles the drop itself and does not call omp.
- omp pauses a goal it restores when its process starts. So j3 does not close a thread's omp for idleness while the goal is active. When the process ends anyway (crash, force stop, server restart), j3 marks the goal `paused`, which is the state omp reports when it starts again.

```json
{"type":"provider-thread.updated","payload":{"id":"pth_T", "…":"…",
 "goal":{"objective":"Keep the PONG answer handy","status":"paused","tokensUsed":0,"tokenBudget":100000,"timeUsedSeconds":0}}}
```

## Subagents

Sources: `subagent_lifecycle` (`started` / `completed` / `failed` / `aborted`) and `subagent_progress` (the raw `AgentProgress` record).

| `Subagent` field | From omp |
|---|---|
| `id` | the subagent id (e.g. `CivilRat`); also `nativeTaskRef: {driver:"pi", nativeId:<id>, strength:"strong"}` |
| `runId`, `parentNodeId` | the run that was live when it first appeared (else the latest run) and that run's root node |
| `origin`, `createdBy`, `driver`, `providerInstanceId` | `provider_native`, `agent`, `pi`, `omp` |
| `providerThreadId`, `childThreadId` | `null`: subagents have no j3 thread |
| `title` | `description`, else the agent name (`task`) |
| `prompt` | the progress `assignment`, else `task` |
| `model` | `resolvedModelIdentity`, else `resolvedModel` |
| `status` | lifecycle `started` → `running`, `completed` → `completed`, `failed` → `failed`, `aborted` → `cancelled`; progress `pending`/`running` while running; `interrupted` when the omp process ends with it running |
| `progress` | one line: the current intent (`currentToolIntent`, else `lastIntent`, else `currentTool`), tool calls and tokens, e.g. `Sleeping 60 seconds · 1 tool call · 13.9k tokens` |
| `result` | the final assistant text of the subagent's transcript (`get_subagent_messages`, read when it completes or fails), else its last `recentOutput` lines |
| `startedAt`, `completedAt`, `updatedAt` | when j3 saw it start or finish, and its last change |

- The `subagent` turn item mirrors the entity: base `status`, `title` and times, plus `subagentId`, `prompt`, `progress` and `result`. Its `parentItemId` is the row of the `task` tool call that started it, and it never ends with its run, only with its own lifecycle.
- A revived subagent (`write agent://<id>` after a cancel) has the same id. It reuses its entity and feed row, which go back to `running` and lose the old result.
- Progress-only updates go through the coalescing window like streaming text. Status changes are committed at once.

```json
{"type":"subagent.updated","payload":{"id":"CivilRat","threadId":"T","runId":"run_1","parentNodeId":"node_1",
 "origin":"provider_native","createdBy":"agent","driver":"pi","providerInstanceId":"omp","providerThreadId":null,
 "childThreadId":null,"nativeTaskRef":{"driver":"pi","nativeId":"CivilRat","strength":"strong"},
 "prompt":"Reply with exactly the word PONG and nothing else. Do not use any tools.","title":"Reply with PONG only",
 "model":"opencode-go/glm-5.3-flash","status":"completed","progress":"13.9k tokens","result":"PONG",
 "startedAt":"2026-11-01T15:18:13.017Z","completedAt":"2026-11-01T15:18:17.197Z","updatedAt":"2026-11-01T15:18:17.197Z"}}
{"type":"turn-item.updated","payload":{"id":"ti_3","threadId":"T","runId":"run_1","nodeId":"node_1","providerThreadId":"pth_T",
 "providerTurnId":"ptu_1","nativeItemRef":null,"parentItemId":"ti_2","ordinal":3,"status":"completed","title":"Reply with PONG only",
 "startedAt":"…","completedAt":"…","updatedAt":"…","type":"subagent","subagentId":"CivilRat","origin":"provider_native",
 "driver":"pi","providerInstanceId":"omp","childThreadId":null,
 "prompt":"Reply with exactly the word PONG and nothing else. Do not use any tools.","progress":"13.9k tokens","result":"PONG"}}
```

## Commands

All of them go through `orchestration.dispatchCommand`, succeed with `{sequence}` and fail with `OrchestrationV2DispatchCommandError`. When omp refuses, the error's `message` is omp's own text (e.g. `A goal is already active. Drop it before creating another.`).

| `type` | Fields besides `commandId`, `threadId` | omp |
|---|---|---|
| `j3.goal.create` | `objective` (non-blank), `tokenBudget?` (positive integer) | `goal {op:"create", objective, token_budget}`; starts omp first when needed |
| `j3.goal.pause` | — | `goal {op:"pause"}` |
| `j3.goal.resume` | — | `goal {op:"resume"}` (a paused or budget-limited goal) |
| `j3.goal.drop` | — | `goal {op:"drop"}`; a `complete` goal is cleared by j3 alone |
| `subagent.stop` (t3) | `runId`, `subagentId` | `cancel_subagent {subagentId}`; refused unless that subagent of this thread is running. The `aborted` lifecycle frame then sets it `cancelled`. |

j3 applies the result of each `goal` command right away. omp's `goal_updated` frame carries the same state.

## Subagent transcripts

`j3.getSubagentTranscript` (payload `{threadId, subagentId, fromByte?}`) reads one subagent's omp session file, `<thread session stem>/<subagentId>.jsonl` next to the thread's own session file, from byte `fromByte` (default 0). It reads the file directly, so it works while the thread's omp process is stopped and after server restarts. Only whole lines are read, at most 4 MiB per call. The answer is `{nextByte, reset, more, entries}`: pass `nextByte` as the next `fromByte`; `reset` means the file got shorter than `fromByte`, so reading restarted at zero and the client drops what it has; `more` means the read was capped and more lines are already waiting. Each `message` line becomes `entries` in order, tagged by `kind` (`j3_protocol::j3::TranscriptEntry`):

| `kind` | Fields | From |
|---|---|---|
| `user` | `text` | user messages (the assignment, steering) |
| `assistant` | `text` | assistant text blocks |
| `thinking` | `text` | assistant thinking blocks |
| `toolCall` | `id`, `name`, `intent?`, `arguments` (compact JSON) | assistant tool calls |
| `toolResult` | `toolCallId`, `name`, `text`, `isError` | tool results (text blocks only) |
| `error` | `text` | an assistant message's `errorMessage` |

Failures (unknown thread, no omp session yet, an invalid id, a missing or unreadable file) are a typed `{"_tag":"J3SubagentTranscriptError","message":…}`. Server: `crates/j3-core/src/transcript.rs`. Client API: `j3_client::Connection::subagent_transcript`.

The desktop's agents hub uses it. Double-tap ← (40–500 ms apart) in an empty composer of a thread with subagents opens the hub in place of the feed. The roster on the left lists the subagents, running ones first. The detail on the right shows the selected subagent's transcript, polled every 500 ms while it runs. ↑/↓ or j/k select, PageUp/PageDown scroll, x stops the selected subagent (`subagent.stop`), and Esc or ←← closes the hub.

## Usage

`j3.getUsage` (payload `{}`) runs `omp usage --json` on the server host, with the omp binary and environment the server launches omp with, and answers with the part of omp's snapshot j3 clients show (`j3_protocol::j3::UsageSnapshot`). Every environment reports its own host's omp logins, like the model list. omp answers from its own report cache when it can, so a report's `fetchedAt` can be older than the snapshot's `generatedAt`. Failures (omp missing, non-zero exit, 60 s timeout, unexpected output) are a typed `{"_tag":"J3UsageError","message":…}`. Servers older than this method answer with a `Die`: `j3.getUsage is not supported by this server`. Client API: `j3_client::Connection::usage`.

```json
{"generatedAt":1793566616901,"reports":[{"provider":"anthropic","fetchedAt":1793566332008,
 "metadata":{"email":"me@example.com","planType":"max"},
 "limits":[{"id":"anthropic:5h","label":"Claude 5 Hour","status":"ok",
  "window":{"label":"5 Hour","resetsAt":1793573042000},
  "amount":{"usedFraction":0.78,"used":78,"limit":100,"unit":"percent"}}]}]}
```

The desktop's composer has a usage gauge beside the model picker: the highest used fraction of the chosen model's provider on the composer's environment, with a popover listing its limits. While a composer is open, its environment is asked for a new snapshot once a minute (a snapshot less than a minute old is not asked for again), and opening the popover asks at once. The last snapshot per environment is cached in `usage.json` next to `desktop.json` and stays on screen while a request is in flight and when one fails.

## Project files

The desktop's file pane browses a thread's project where the thread runs, so a shieldy thread shows shieldy's files. It uses two t3 methods, unchanged on the wire: `projects.listEntries` (which mobile never calls) and `projects.readFile` (M19 in `ios-surface.md`, which also covers its answers and failures). Wire types: `j3_protocol::t3::project::{ProjectListEntriesInput, ProjectListEntriesResult, ProjectListEntriesError}`. Server: `crates/j3-core/src/files.rs`. Client API: `j3_client::Connection::{list_entries, read_file}`.

`projects.listEntries` (payload `{cwd, directoryPath?}`) lists one directory level, unlike t3's server, which answers with its whole search index. `directoryPath` is relative to `cwd`; absent or empty lists `cwd` itself.

- Each entry's `path` is relative to `cwd` with `/` separators (`src/main.rs`), so it can be passed back as `directoryPath` or `relativePath`.
- Directories come first, then names in case-insensitive order. `.git` is left out. Symlinks list as what they point at.
- When `cwd` is in a git work tree, one `git check-ignore --stdin -z` per listing marks ignored entries `ignored: true`. Inside an ignored directory every entry is ignored. Without git, nothing is marked.
- At most 2000 entries; past that `truncated` is `true`.
- A `directoryPath` that is absolute, climbs out of `cwd` or resolves outside it through a symlink fails, as does a missing or unreadable directory: `{"_tag":"ProjectListEntriesError","cwd":…,"failure":"directory_list_failed","message":…}`. A missing `cwd` is `workspace_root_not_found`; a file is `workspace_root_not_directory`.

```json
{"entries":[{"path":"crates/j3-core/src","kind":"directory"},
 {"path":"crates/j3-core/target","kind":"directory","ignored":true},
 {"path":"crates/j3-core/Cargo.toml","kind":"file"}],"truncated":false}
```

The pane opens from the thread header's folder button or ⌘⇧E (Ctrl+Shift+E on Linux) and sits to the right of the conversation, resizable. Directories load as they are expanded. With the tree focused, ↑/↓ move, → expands a directory (or enters it), ← collapses it (or goes to the parent) and Enter opens a file. A file opens in a read-only, syntax-highlighted preview under the tree, with its path, a Copy path button and notices for truncated and binary files. Closing the pane returns focus to the composer; reopening it keeps its expanded directories and open file. The pane belongs to the thread view, so another thread opens without it.
