# j3 protocol extensions: omp goals and subagents

j3 adds two omp features to the t3 wire protocol (`ios-surface.md`): **goal mode** and **subagents**. Rust types: `j3_protocol::t3::orchestration::{ProviderGoal, Subagent, SubagentItem}`, `j3_protocol::t3::commands::SubagentStopCommand`, `j3_protocol::j3`. Server mapping: `crates/j3-core/src/engine/model/agents.rs`. Client API: `j3_client::ThreadState::{goal, subagents, subagent}` and `j3_client::Connection::{create_goal, pause_goal, resume_goal, drop_goal, stop_subagent}`.

## Placement

Everything the server sends uses shapes the t3 contracts already define. `packages/contracts` decodes every frame and the iOS client renders it as it is. Only the commands are new: four `j3.goal.*` variants of `orchestration.dispatchCommand`, which t3 clients never send.

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
