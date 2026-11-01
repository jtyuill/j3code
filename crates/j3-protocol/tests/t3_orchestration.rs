//! Orchestration V2 shapes: spec examples (docs/protocol/ios-surface.md §2–§4),
//! real t3 data, forward-compatible unions, and helpers.
//!
//! `fixtures/t3/real/` holds real t3 data re-encoded through t3's own Effect
//! schemas (`Schema.toCodecJson`, contracts @ 8760d063): the persisted
//! projection rows in `scripts/fixtures/issue-21397.json` and
//! `scripts/fixtures/desktop-launch/statev2.sqlite`, assembled into
//! projections, thread stream items (snapshot, synchronized, one event per
//! entity) and shell stream items, plus client-runtime's
//! `orchestrationV2TestFixtures.ts`. t3code ships no recorded wire
//! transcripts for orchestration v2; its `*_transcript.ndjson` files are
//! provider-side (Pi/Codex/Claude RPC), not t3 wire.

use j3_protocol::t3::commands::*;
use j3_protocol::t3::methods::*;
use j3_protocol::t3::model::*;
use j3_protocol::t3::orchestration::*;
use j3_protocol::t3::{iso_from_unix_ms, now_iso};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Decode into `T`, re-encode, and require the JSON to be unchanged.
#[track_caller]
fn round_trip<T: Serialize + DeserializeOwned + std::fmt::Debug>(value: Value) -> T {
    let typed: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|err| panic!("decode failed: {err}\n{value:#}"));
    let encoded = serde_json::to_value(&typed).unwrap();
    assert_eq!(encoded, value, "re-encoding changed the JSON");
    typed
}

const T: &str = "1d3e5c77-0000-4000-8000-000000000001";
const P: &str = "8c7d0000-0000-4000-8000-000000000002";

fn model_selection() -> Value {
    json!({"instanceId":"omp","model":"anthropic/claude-sonnet-4-5","options":[{"id":"thinking","value":"high"}]})
}

fn app_thread() -> Value {
    json!({"createdBy":"user","creationSource":"mobile","id":T,"projectId":P,
        "title":"Fix the flaky login test","providerInstanceId":"omp","modelSelection":model_selection(),
        "runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,
        "activeProviderThreadId":"pth_T",
        "lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":T},
        "forkedFrom":null,"createdAt":"2026-11-01T12:00:00.000Z","updatedAt":"2026-11-01T12:00:00.000Z",
        "archivedAt":null,"settledOverride":null,"settledAt":null,"lastVisitedAt":null,"deletedAt":null})
}

fn provider_session() -> Value {
    json!({"id":"ps_T","driver":"pi","providerInstanceId":"omp","status":"ready","cwd":"/home/jacob/Developer/j3code",
        "model":"anthropic/claude-sonnet-4-5","capabilities":pi_capabilities(),
        "createdAt":"2026-11-01T12:00:00.000Z","updatedAt":"2026-11-01T12:00:00.000Z","lastError":null})
}

fn provider_thread() -> Value {
    json!({"id":"pth_T","driver":"pi","providerInstanceId":"omp","providerSessionId":"ps_T","appThreadId":T,
        "ownerNodeId":null,"nativeThreadRef":null,"nativeConversationHeadRef":null,"status":"active",
        "firstRunOrdinal":1,"lastRunOrdinal":1,"handoffIds":[],"forkedFrom":null,"pendingBackgroundTasks":[],
        "contextUsage":null,"nativeMetadata":null,"goal":null,
        "createdAt":"2026-11-01T12:00:00.000Z","updatedAt":"2026-11-01T12:00:00.000Z"})
}

/// `PiProviderCapabilitiesV2`, transcribed from
/// `packages/provider-pi/src/server/adapter.ts:91-182`.
fn pi_capabilities() -> Value {
    json!({
        "runtimePolicy": {"enforcement": "client-boundary"},
        "sessions": {"supportsMultipleProviderThreadsPerSession": false, "supportsModelSwitchInSession": true,
            "supportsProviderSwitchingViaHandoff": true, "supportsRuntimeModeSwitchInSession": false,
            "pendingRequestsSurviveRestart": false},
        "threads": {"canCreateEmptyThread": true, "canReadThreadSnapshot": true, "canRollbackThread": true,
            "canForkThread": true, "canForkFromTurn": true, "canForkFromSubagentThread": false,
            "exposesNativeThreadId": true},
        "turns": {"exposesNativeTurnId": false, "emitsTurnStarted": true, "emitsTurnCompleted": true,
            "supportsInterrupt": true, "supportsActiveSteering": true, "supportsSteeringByInterruptRestart": false,
            "supportsQueuedMessages": true, "terminalStatusQuality": "strong"},
        "streaming": {"streamsAssistantText": true, "streamsReasoning": true, "streamsToolOutput": true,
            "streamsPlanText": false, "emitsMessageCompleted": true},
        "tools": {"exposesToolItemIds": true, "emitsToolStarted": true, "emitsToolCompleted": true,
            "emitsToolOutput": true, "supportsMcpTools": true, "supportsDynamicToolCallbacks": false},
        "approvals": {"supportsCommandApproval": true, "supportsFileReadApproval": false,
            "supportsFileChangeApproval": true, "supportsApplyPatchApproval": false,
            "approvalsHaveNativeRequestIds": true, "approvalCallbacksAreLiveOnly": true,
            "approvalsCanOriginateFromSubagents": false},
        "planning": {"emitsPlanUpdated": false, "emitsTodoList": false, "emitsProposedPlan": false,
            "supportsStructuredQuestions": true, "planDeltasHaveItemIds": false},
        "subagents": {"supportsSubagents": true, "exposesSubagentThreadIds": false, "emitsSubagentLifecycle": true,
            "canWaitForSubagents": false, "canCloseSubagents": false, "canForkSubagentThread": false},
        "context": {"acceptsSystemContext": false, "acceptsDeveloperContext": false,
            "acceptsSyntheticUserContext": true, "canGenerateSummaries": false, "canConsumeHandoffSummaries": true,
            "supportsDeltaHandoff": true, "supportsFullThreadHandoff": true, "maxRecommendedHandoffChars": null},
        "checkpointing": {"appCanCheckpointFilesystem": true, "supportsNestedCheckpointScopes": false,
            "providerCanRollbackConversation": true, "providerRollbackReturnsSnapshot": true,
            "providerCanReadConversationSnapshot": true},
        "identity": {"nativeThreadIds": "strong", "nativeTurnIds": "weak", "nativeItemIds": "strong",
            "nativeRequestIds": "strong"}
    })
}

fn empty_projection() -> Value {
    json!({"thread":app_thread(),"runs":[],"attempts":[],"nodes":[],"subagents":[],
        "providerSessions":[provider_session()],"providerThreads":[provider_thread()],"providerTurns":[],
        "runtimeRequests":[],"messages":[],"plans":[],"turnItems":[],"checkpointScopes":[],"checkpoints":[],
        "contextHandoffs":[],"contextTransfers":[],"visibleTurnItems":[],"updatedAt":"2026-11-01T12:00:00.000Z"})
}

fn thread_shell() -> Value {
    json!({"createdBy":"user","creationSource":"mobile","id":T,"projectId":P,"title":"Fix login bug","providerInstanceId":"omp",
        "modelSelection":{"instanceId":"omp","model":"anthropic/claude-sonnet-4-5"},"runtimeMode":"full-access","interactionMode":"default",
        "branch":null,"worktreePath":null,"lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":T},"forkedFrom":null,
        "activeProviderThreadId":"pth_1d3e","latestRunId":"run_2","activeRunId":"run_2","status":"running","activityRunStatus":"running",
        "pendingRuntimeRequest":{"id":"rq_1","kind":"user_input","createdAt":"2026-11-01T12:01:00.000Z","blocking":true},
        "latestVisibleMessage":{"id":"msg_a1","role":"assistant","text":"Looking at the auth flow…","updatedAt":"2026-11-01T12:00:59.000Z"},
        "latestUserMessageAt":"2026-11-01T12:00:05.000Z","hasActionableProposedPlan":false,
        "pendingBackgroundTasks":[],"providerInstanceHistory":[],"itemCount":6,"visibleItemCount":5,
        "createdAt":"2026-11-01T11:00:00.000Z","updatedAt":"2026-11-01T12:01:00.000Z","archivedAt":null,"settledOverride":null,"settledAt":null,"deletedAt":null})
}

fn project_shell() -> Value {
    json!({"id":P,"title":"j3code","workspaceRoot":"/home/jacob/Developer/j3code","defaultModelSelection":null,
        "scripts":[],"createdAt":"2026-11-01T12:00:00.000Z","updatedAt":"2026-11-01T12:00:00.000Z"})
}

// ---------------------------------------------------------------------------
// Spec examples
// ---------------------------------------------------------------------------

#[test]
fn launch_thread_example() {
    let input: ThreadLaunchInput = round_trip(json!({
        "commandId":"6f1c2a4e-0000-4000-8000-000000000003","creationSource":"mobile","threadId":T,"projectId":P,
        "title":"Fix the flaky login test","generateTitle":true,"modelSelection":model_selection(),
        "runtimeMode":"full-access","interactionMode":"default","workspaceStrategy":{"type":"root"},
        "initialMessage":{"messageId":"a9e0b0d2-0000-4000-8000-000000000004","text":"Fix the flaky login test",
            "attachments":[{"type":"image","id":"att_9k2m","name":"shot.png","mimeType":"image/png","sizeBytes":183422}]}
    }));
    assert_eq!(
        input.workspace_strategy,
        WorkspaceStrategy::Root { branch: None }
    );
    assert_eq!(
        input.model_selection.option("thinking"),
        Some(&ProviderOptionSelectionValue::String("high".into()))
    );
    let result: ThreadLaunchResult = round_trip(json!({
        "threadId":T,"projection":empty_projection(),"resumed":false,"sequence":1843
    }));
    assert_eq!(
        result.projection.provider_sessions[0].capabilities,
        ProviderCapabilities::omp()
    );
}

#[test]
fn dispatch_command_examples() {
    let examples = [
        json!({"type":"message.dispatch","commandId":"0c6b","createdBy":"user","creationSource":"mobile","threadId":T,"messageId":"e2f4",
            "text":"Now add a regression test","attachments":[],"modelSelection":model_selection(),
            "dispatchMode":{"type":"start_immediately"}}),
        json!({"type":"message.dispatch","commandId":"c2","createdBy":"user","creationSource":"mobile","threadId":T,"messageId":"m2","text":"use vitest","attachments":[],
            "titleSeed":"use vitest","deliveryIntent":"auto","dispatchMode":{"type":"start_immediately"}}),
        json!({"type":"run.interrupt","commandId":"c3","threadId":T,"runId":"run_17","holdQueue":true}),
        json!({"type":"thread.metadata.update","commandId":"c4","threadId":T,"title":"Login test fix"}),
        json!({"type":"runtime-request.respond","commandId":"c5","threadId":T,"requestId":"rq_10","answers":{"db":"SQLite","features":["Auth","Search"]}}),
        json!({"type":"queued-message.promote-to-steer","commandId":"c6","threadId":T,"queuedRunId":"run_18","targetRunId":"run_17"}),
    ];
    for example in examples {
        let command: OrchestrationV2Command = round_trip(example);
        assert!(!matches!(command, OrchestrationV2Command::Unknown(_)));
        assert!(command.command_id().is_some());
        assert_eq!(command.thread_id(), Some(T));
    }
}

#[test]
fn every_accepted_command_type_is_typed() {
    let base = |ty: &str, extra: Value| {
        let mut v = json!({"type": ty, "commandId": format!("cmd-{ty}"), "threadId": T});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    let cases = [
        base(
            "thread.stop",
            json!({"reason":"user","stopDelegatedTasks":true}),
        ),
        base("thread.archive", json!({})),
        base("thread.unarchive", json!({})),
        base("thread.delete", json!({"onlyIfUnused":true})),
        base(
            "thread.runtime-mode.set",
            json!({"runtimeMode":"auto-accept-edits"}),
        ),
        base(
            "thread.interaction-mode.set",
            json!({"interactionMode":"plan"}),
        ),
        base(
            "runtime-request.respond",
            json!({"requestId":"rq","decision":"acceptForSession","feedback":"ok"}),
        ),
        base("thread.user-input.dismiss", json!({"requestId":"rq"})),
        base("queued-run.cancel", json!({"runId":"r"})),
        base(
            "queued-run.edit",
            json!({"runId":"r","text":"edited","attachments":[],"context":{"version":1,"records":[{"x":1}]}}),
        ),
        base("queue.resume", json!({})),
        base("queued-run.start", json!({"runId":"r"})),
        base(
            "thread.visit",
            json!({"visitedAt":"2026-11-01T12:00:00.000Z","opened":true,"searchQuery":"q"}),
        ),
        base(
            "thread.metadata.update",
            json!({"branch":null,"regenerateTitle":false,"limitRecovery":null}),
        ),
        base(
            "thread.settle",
            json!({"settledAt":"2026-11-01T12:00:00.000Z"}),
        ),
        base("thread.unsettle", json!({"reason":"user"})),
        base(
            "thread.snooze",
            json!({"snoozedUntil":"2026-11-02T12:00:00.000Z"}),
        ),
        base("thread.unsnooze", json!({"reason":"user"})),
        base("thread.auto-settle.set", json!({"enabled":false})),
        base("thread.pin", json!({"orderKey":"a0"})),
        base("thread.unpin", json!({})),
        base("thread.pin.reorder", json!({"orderKey":"a1"})),
        base("thread.active.reorder", json!({"orderKey":"a2"})),
        base(
            "message.dispatch",
            json!({"createdBy":"user","creationSource":"mobile","messageId":"m","text":"q","attachments":[],
            "dispatchMode":{"type":"queue_after_active"}}),
        ),
        base(
            "message.dispatch",
            json!({"createdBy":"user","creationSource":"mobile","messageId":"m","text":"s","attachments":[],
            "deliveryIntent":"steer","dispatchMode":{"type":"steer_active","targetRunId":"run_1"}}),
        ),
        base(
            "message.dispatch",
            json!({"createdBy":"user","creationSource":"mobile","messageId":"m","text":"d","attachments":[],
            "dispatchMode":{"type":"defer_start","workspaceStrategy":{"type":"worktree","baseRef":"main","startFromOrigin":true}}}),
        ),
        base("run.interrupt", json!({"runId":"r","reason":"stop"})),
        base(
            "queued-message.promote-to-steer",
            json!({"queuedRunId":"a","targetRunId":"b"}),
        ),
        base(
            "subagent.stop",
            json!({"runId":"r","subagentId":"PingTest"}),
        ),
        base(
            "j3.goal.create",
            json!({"objective":"Ship it","tokenBudget":200000}),
        ),
        base("j3.goal.pause", json!({})),
        base("j3.goal.resume", json!({})),
        base("j3.goal.drop", json!({})),
    ];
    let mut types = std::collections::BTreeSet::new();
    for case in cases {
        let command: OrchestrationV2Command = round_trip(case);
        assert!(
            !matches!(command, OrchestrationV2Command::Unknown(_)),
            "{command:?}"
        );
        types.insert(command.tag().unwrap().to_owned());
    }
    assert_eq!(types.len(), 31);
    assert_eq!(OrchestrationV2Command::KNOWN_TAGS.len(), 31);
}

#[test]
fn cut_commands_decode_to_unknown_and_reencode() {
    for value in [
        json!({"type":"thread.create","commandId":"c","threadId":T,"projectId":P,"title":"x","createdBy":"user","creationSource":"web",
            "modelSelection":{"instanceId":"omp","model":"m"},"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null}),
        json!({"type":"thread.fork","commandId":"c","threadId":T}),
        json!({"type":"provider.switch","commandId":"c","threadId":T,"modelSelection":{"instanceId":"x","model":"y"}}),
        json!({"type":"some.future.command","commandId":"c","weird":[1,{"a":null}]}),
    ] {
        let command: OrchestrationV2Command = round_trip(value);
        let OrchestrationV2Command::Unknown(_) = &command else {
            panic!("expected Unknown, got {command:?}");
        };
        assert_eq!(command.command_id(), Some("c"));
    }
    // A known type with a bad body is an error, never Unknown.
    assert!(
        serde_json::from_value::<OrchestrationV2Command>(
            json!({"type":"thread.archive","commandId":"c"})
        )
        .is_err()
    );
}

#[test]
fn thread_shell_example() {
    let shell: ThreadShell = round_trip(thread_shell());
    assert_eq!(shell.status, ShellThreadStatus::Running);
    assert_eq!(
        shell.activity_run_status,
        Some(Some(ActivityRunStatus::Running))
    );
}

#[test]
fn archived_shell_snapshot_example() {
    let mut archived = thread_shell();
    archived["archivedAt"] = json!("2026-10-30T09:00:00.000Z");
    archived["status"] = json!("completed");
    let _: ArchivedShellSnapshot = round_trip(json!({
        "schemaVersion":1,"snapshotSequence":1842,"projects":[project_shell()],"threads":[archived]
    }));
    let _: EmptyInput = round_trip(json!({}));
}

#[test]
fn method_payloads_and_answers() {
    let _: GetThreadProjectionInput = round_trip(json!({"threadId":T}));
    let _: SubscribeShellInput = round_trip(json!({"requestCompletionMarker":true}));
    let _: SubscribeShellInput =
        round_trip(json!({"afterSequence":41,"requestCompletionMarker":true}));
    let _: SubscribeThreadInput = round_trip(
        json!({"threadId":T,"afterSequence":41,"requestCompletionMarker":true,"acceptDedupedBodies":true}),
    );
    let _: GetTurnItemInput = round_trip(json!({"threadId":T,"itemId":"ti_c1","revision":"3"}));
    let none: GetTurnItemResult = round_trip(json!({"item":null}));
    assert!(none.item.is_none());
    let _: SearchThreadsInput =
        round_trip(json!({"query":"login","limit":20,"projectIds":[P],"titlesOnly":true}));
    let _: SearchThreadsResult = round_trip(json!({"matches":[]}));
    let _: SearchThreadsResult = round_trip(
        json!({"matches":[{"threadId":T,"projectId":P,"source":"user",
        "snippet":"Fix login bug","messageCreatedAt":"2026-11-01T12:00:05.000Z"}]}),
    );
    let _: ForceStopThreadInput = round_trip(json!({"threadId":T,"runId":"run_2"}));
    let _: ForceStopThreadResult = round_trip(json!({"outcome":"killed","sharedWithThreadIds":[]}));
    let _: DispatchCommandResult = round_trip(json!({"sequence":0}));
    let _: ThreadBoundedSnapshot = round_trip(
        json!({"snapshotSequence":41,"projection":empty_projection(),
        "historyCursor":null,"hasMoreHistory":false,"latestLocalTurnOrdinal":null}),
    );
}

/// §4.6 "One full turn on the wire", with `B`, `E(n)` and `…` expanded.
#[test]
fn full_turn_example() {
    let at = "2026-11-01T12:00:45.000Z";
    let base = |id: &str, ordinal: u64, status: &str| {
        json!({"id":id,"threadId":T,"runId":"run_1","nodeId":"node_r1","providerThreadId":"pth_T","providerTurnId":"ptu_1",
            "nativeItemRef":null,"parentItemId":null,"title":null,"ordinal":ordinal,"status":status,
            "startedAt":at,"completedAt":null,"updatedAt":at})
    };
    let item = |base: Value, body: Value| {
        let mut v = base;
        v.as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        v
    };
    let event = |n: u64, ty: &str, payload: Value| json!({"kind":"event","sequence":n,"event":{"id":format!("evt_{n}"),"threadId":T,"runId":"run_1","occurredAt":at,"type":ty,"payload":payload}});
    let run = |status: &str, attempt: Value, started: Value, completed: Value| {
        json!({"id":"run_1","threadId":T,"ordinal":1,"providerInstanceId":"omp","modelSelection":model_selection(),"providerThreadId":"pth_T",
            "userMessageId":"msg_u1","rootNodeId":"node_r1","activeAttemptId":attempt,"status":status,"requestedAt":at,
            "startedAt":started,"completedAt":completed,"checkpointId":null,"contextHandoffId":null})
    };
    let attempt = |status: &str, completed: Value| {
        json!({"id":"att_1","runId":"run_1","attemptOrdinal":1,"rootNodeId":"node_r1","providerInstanceId":"omp","providerThreadId":"pth_T",
            "providerTurnId":"ptu_1","reason":"initial","status":status,"startedAt":at,"completedAt":completed})
    };
    let provider_turn = |status: &str, completed: Value| {
        json!({"id":"ptu_1","providerThreadId":"pth_T","nodeId":"node_r1","runAttemptId":"att_1","nativeTurnRef":null,"ordinal":1,
            "status":status,"startedAt":at,"completedAt":completed})
    };
    let request = |status: &str, resolved: Value, answers: Option<Value>| {
        let mut v = json!({"id":"rq_1","nodeId":"node_r1","providerTurnId":"ptu_1","nativeRequestRef":null,"kind":"user_input","status":status,
            "responseCapability":{"type":"live","providerSessionId":"ps_T"},"createdAt":at,"resolvedAt":resolved});
        if let Some(answers) = answers {
            v["answers"] = answers;
        }
        v
    };
    let questions = json!([{"id":"q1","header":"Fix","question":"Rotate the refresh token on every login?",
        "options":[{"label":"Yes","description":"Yes","value":"Yes"},{"label":"No","description":"No","value":"No"}],"allowCustomAnswer":true}]);
    let steps = json!([{"id":"0.0","text":"Reproduce","status":"completed"},{"id":"0.1","text":"Patch refresh","status":"running"}]);
    let message = |id: &str, by: &str, source: &str, role: &str, text: &str| {
        json!({"createdBy":by,"creationSource":source,"id":id,"threadId":T,"runId":"run_1","nodeId":null,"role":role,
            "text":text,"attachments":[],"streaming":false,"createdAt":at,"updatedAt":at})
    };
    let completed = |b: Value| {
        let mut b = b;
        b["completedAt"] = json!(at);
        b
    };

    let items = vec![
        json!({"kind":"snapshot","snapshotSequence":41,"projection":empty_projection()}),
        json!({"kind":"synchronized"}),
        event(
            42,
            "message.updated",
            message("msg_u1", "user", "mobile", "user", "Why does login fail?"),
        ),
        event(
            43,
            "run.created",
            run("starting", Value::Null, Value::Null, Value::Null),
        ),
        event(
            44,
            "turn-item.updated",
            item(
                completed(base("ti_u1", 1, "completed")),
                json!({"type":"user_message","createdBy":"user",
            "creationSource":"mobile","messageId":"msg_u1","inputIntent":"turn_start","text":"Why does login fail?","attachments":[]}),
            ),
        ),
        event(45, "run-attempt.created", attempt("running", Value::Null)),
        event(
            46,
            "run.updated",
            run("running", json!("att_1"), json!(at), Value::Null),
        ),
        event(
            47,
            "provider-turn.updated",
            provider_turn("running", Value::Null),
        ),
        event(
            48,
            "turn-item.updated",
            item(
                base("ti_r1", 2, "running"),
                json!({"type":"reasoning","text":"Looking at the auth flow","streaming":true}),
            ),
        ),
        event(
            53,
            "turn-item.updated",
            item(
                base("ti_a1", 3, "running"),
                json!({"type":"assistant_message","messageId":"msg_a1","text":"The login fails because","streaming":true}),
            ),
        ),
        event(
            57,
            "turn-item.updated",
            item(
                base("ti_c1", 4, "running"),
                json!({"title":"bash","type":"command_execution","input":"npm test -- auth"}),
            ),
        ),
        event(
            60,
            "turn-item.updated",
            item(
                completed(base("ti_c1", 4, "completed")),
                json!({"title":"bash","type":"command_execution","input":"npm test -- auth","output":"1 failing","exitCode":1}),
            ),
        ),
        event(
            62,
            "runtime-request.updated",
            request("pending", Value::Null, None),
        ),
        event(
            63,
            "turn-item.updated",
            item(
                base("ti_q1", 5, "waiting"),
                json!({"type":"user_input_request","requestId":"rq_1","questions":questions}),
            ),
        ),
        event(
            64,
            "run.updated",
            run("waiting", json!("att_1"), json!(at), Value::Null),
        ),
        event(
            70,
            "runtime-request.updated",
            request("resolved", json!(at), Some(json!({"q1":"Yes"}))),
        ),
        event(
            71,
            "turn-item.updated",
            item(
                completed(base("ti_q1", 5, "completed")),
                json!({"type":"user_input_request","requestId":"rq_1",
            "questions":questions,"questionAnswer":{"requestId":"rq_1","answers":{"q1":"Yes"},"attachmentsByQuestionId":{}}}),
            ),
        ),
        event(
            72,
            "plan.updated",
            json!({"id":"plan_T","threadId":T,"runId":"run_1","nodeId":"node_r1","status":"active","kind":"todo_list",
            "steps":steps,"detailInTurnItem":true}),
        ),
        event(
            73,
            "turn-item.updated",
            item(
                base("ti_p_T_1", 6, "running"),
                json!({"type":"todo_list","planId":"plan_T","steps":steps}),
            ),
        ),
        event(
            80,
            "turn-item.updated",
            item(
                completed(base("ti_a1", 3, "completed")),
                json!({"type":"assistant_message","messageId":"msg_a1","text":"<final>","streaming":false}),
            ),
        ),
        event(
            81,
            "message.updated",
            message("msg_a1", "agent", "provider", "assistant", "<final>"),
        ),
        event(
            82,
            "provider-turn.updated",
            provider_turn("completed", json!(at)),
        ),
        event(83, "run-attempt.updated", attempt("completed", json!(at))),
        event(
            84,
            "run.updated",
            run("completed", json!("att_1"), json!(at), json!(at)),
        ),
    ];
    for value in items {
        let item: ThreadStreamItem = round_trip(value);
        match &item {
            ThreadStreamItem::Event(e) => {
                assert!(
                    !matches!(e.event.body, DomainEventBody::Unknown(_)),
                    "{item:?}"
                );
                if let DomainEventBody::TurnItemUpdated(ti) = &e.event.body {
                    assert!(!matches!(ti.kind, TurnItemKind::Unknown(_)), "{item:?}");
                }
            }
            ThreadStreamItem::Snapshot(_) | ThreadStreamItem::Synchronized => {}
            ThreadStreamItem::Unknown(_) => panic!("unexpected Unknown"),
        }
    }

    let shell: ShellStreamItem = round_trip(
        json!({"kind":"thread.updated","sequence":84,"location":"active","thread":thread_shell()}),
    );
    assert!(matches!(shell, ShellStreamItem::ThreadUpdated(_)));
}

#[test]
fn remaining_turn_item_kinds() {
    let base = json!({"id":"ti","threadId":T,"runId":null,"nodeId":null,"providerThreadId":null,"providerTurnId":null,
        "nativeItemRef":{"driver":"pi","nativeId":null,"strength":"none"},"parentItemId":null,"ordinal":0,"status":"completed",
        "title":"t","startedAt":null,"completedAt":null,"updatedAt":"2026-11-01T12:00:00.000Z"});
    let bodies = [
        json!({"type":"file_change","fileName":"src/a.rs","additions":3,"deletions":1,"diffStr":"@@","newStr":"x",
            "changes":[{"operation":"update","path":"src/a.rs"}]}),
        json!({"type":"file_search","pattern":"fn main","results":[{"fileName":"src/main.rs","line":1,"column":1,"preview":"fn main()"}]}),
        json!({"type":"web_search","patterns":["rust serde"],"results":[{"title":"serde","url":"https://serde.rs","snippet":"…"}]}),
        json!({"type":"dynamic_tool","toolName":"lsp","input":{"op":"hover"},"output":null}),
        json!({"type":"dynamic_tool","toolName":null,"input":null}),
        json!({"type":"run_interrupt_request","message":"Stop requested","unresponsiveAt":"2026-11-01T12:00:10.000Z"}),
        json!({"type":"run_interrupt_result","message":"Stopped"}),
        json!({"type":"system_notice","message":"Heads up"}),
        json!({"type":"error","failure":{"class":"provider_error","message":"overloaded","code":null,"retryable":true},
            "retry":{"attempt":1,"maxAttempts":3,"retryDelayMs":2000}}),
        json!({"type":"compaction","driver":null,"summary":"Earlier work","beforeTokenCount":100000,"afterTokenCount":9000}),
        json!({"type":"user_message","createdBy":"user","creationSource":"mobile","messageId":"m","inputIntent":"steer","text":"also",
            "context":{"version":1,"records":[]},"attachments":[{"type":"file","id":"f1","name":"a.txt","mimeType":"text/plain","sizeBytes":3},
            {"type":"pdf-ish","id":"u1","name":"x","mimeType":"application/x","sizeBytes":0}]}),
    ];
    for body in bodies {
        let mut v = base.clone();
        v.as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        let item: TurnItem = round_trip(v);
        assert!(!matches!(item.kind, TurnItemKind::Unknown(_)), "{item:?}");
    }
}

/// The t3 shapes j3 fills from omp goals and subagents
/// (`docs/protocol/j3-extensions.md`).
#[test]
fn goal_and_subagent_shapes() {
    let goal = json!({"objective":"Ship it","status":"budget_limited","tokensUsed":15950,"tokenBudget":200000,
        "timeUsedSeconds":7});
    let typed: ProviderGoal = round_trip(goal.clone());
    assert_eq!(typed.status, ProviderGoalStatus::BudgetLimited);
    let _: ProviderGoal = round_trip(json!({"objective":"x","status":"active","tokenBudget":null}));
    let mut thread = provider_thread();
    thread["goal"] = goal.clone();
    let thread: ProviderThread = round_trip(thread);
    assert_eq!(thread.goal.unwrap().tokens_used, Some(15950));
    let mut shell = thread_shell();
    shell["goal"] = goal;
    let _: ThreadShell = round_trip(shell);

    let subagent = json!({"id":"PingTest","threadId":T,"runId":"run_1","parentNodeId":"node_r1","origin":"provider_native",
        "createdBy":"agent","driver":"pi","providerInstanceId":"omp","providerThreadId":null,"childThreadId":null,
        "nativeTaskRef":{"driver":"pi","nativeId":"PingTest","strength":"strong"},"prompt":"Reply PONG",
        "title":"Ping","model":"opencode-go/glm-5.3-flash","status":"completed","progress":"1 request",
        "result":"PONG","startedAt":"2026-11-01T12:00:00.000Z","completedAt":"2026-11-01T12:00:03.000Z",
        "updatedAt":"2026-11-01T12:00:03.000Z"});
    let typed: Subagent = round_trip(subagent.clone());
    assert!(!typed.status.is_active());
    let item: ThreadStreamItem = round_trip(
        json!({"kind":"event","sequence":9,"event":{"id":"e","threadId":T,"runId":"run_1",
        "occurredAt":"2026-11-01T12:00:00.000Z","type":"subagent.updated","payload":subagent.clone()}}),
    );
    let ThreadStreamItem::Event(event) = item else {
        panic!("expected an event");
    };
    assert!(matches!(
        event.event.body,
        DomainEventBody::SubagentUpdated(_)
    ));
    let mut projection = empty_projection();
    projection["subagents"] = json!([subagent]);
    let _: ThreadProjection = round_trip(projection);

    let item: TurnItem = round_trip(
        json!({"id":"ti_4","threadId":T,"runId":"run_1","nodeId":"node_r1","providerThreadId":"pth_T","providerTurnId":"ptu_1",
        "nativeItemRef":null,"parentItemId":"ti_3","ordinal":4,"status":"running","title":"Ping",
        "startedAt":"2026-11-01T12:00:00.000Z","completedAt":null,"updatedAt":"2026-11-01T12:00:00.000Z",
        "type":"subagent","subagentId":"PingTest","origin":"provider_native","driver":"pi","providerInstanceId":"omp",
        "childThreadId":null,"prompt":"Reply PONG","progress":"Thinking","result":null}),
    );
    assert!(matches!(item.kind, TurnItemKind::Subagent(_)));
}

// ---------------------------------------------------------------------------
// Forward-compatible unions keep unknown variants verbatim
// ---------------------------------------------------------------------------

#[test]
fn unknown_turn_item_kind_is_preserved() {
    let value = json!({"id":"ti_x","threadId":T,"runId":null,"nodeId":null,"providerThreadId":null,"providerTurnId":null,
        "nativeItemRef":null,"parentItemId":null,"ordinal":7,"status":"completed","title":null,"startedAt":null,"completedAt":null,
        "updatedAt":"2026-11-01T12:00:00.000Z","type":"hologram","beams":[1,2,{"x":null}]});
    let item: TurnItem = round_trip(value.clone());
    let TurnItemKind::Unknown(body) = &item.kind else {
        panic!("expected Unknown");
    };
    assert_eq!(body["type"], "hologram");
    assert_eq!(item.kind.tag(), Some("hologram"));
    // In a projection row and in an event.
    let _: ProjectedTurnItem = round_trip(
        json!({"position":0,"visibility":"local","sourceThreadId":T,"sourceItemId":"ti_x","item":value.clone()}),
    );
    let _: ThreadStreamItem = round_trip(
        json!({"kind":"event","sequence":9,"event":{"id":"e","threadId":T,
        "occurredAt":"2026-11-01T12:00:00.000Z","type":"turn-item.updated","payload":value}}),
    );
    // A known kind with a bad body fails.
    let mut bad = json!({"id":"ti","threadId":T,"runId":null,"nodeId":null,"providerThreadId":null,"providerTurnId":null,
        "nativeItemRef":null,"parentItemId":null,"ordinal":0,"status":"completed","title":null,"startedAt":null,"completedAt":null,
        "updatedAt":"2026-11-01T12:00:00.000Z","type":"reasoning"});
    assert!(serde_json::from_value::<TurnItem>(bad.clone()).is_err());
    bad["text"] = json!("ok");
    bad["streaming"] = json!(false);
    let _: TurnItem = round_trip(bad);
}

#[test]
fn unknown_domain_event_type_is_preserved() {
    let value = json!({"kind":"event","sequence":12,"event":{"id":"evt","threadId":T,"runId":"run_1","driver":"pi",
        "occurredAt":"2026-11-01T12:00:00.000Z","type":"checkpoint.captured","payload":{"id":"cp","files":[]},"extra":true}});
    let item: ThreadStreamItem = round_trip(value);
    let ThreadStreamItem::Event(e) = item else {
        panic!("expected event")
    };
    assert_eq!(e.event.run_id.as_deref(), Some("run_1"));
    assert_eq!(e.event.body.tag(), Some("checkpoint.captured"));
    assert!(matches!(e.event.body, DomainEventBody::Unknown(_)));
    // Known type, bad payload: error.
    assert!(
        serde_json::from_value::<DomainEvent>(json!({"id":"e","threadId":T,"occurredAt":"x",
        "type":"run.updated","payload":{"id":"run_1"}}))
        .is_err()
    );
}

#[test]
fn unknown_stream_item_kinds_are_preserved() {
    let thread = json!({"kind":"keepalive","at":3});
    let item: ThreadStreamItem = round_trip(thread);
    assert!(matches!(item, ThreadStreamItem::Unknown(_)));
    let shell = json!({"kind":"project.archived","sequence":5,"projectId":P});
    let item: ShellStreamItem = round_trip(shell);
    assert!(matches!(item, ShellStreamItem::Unknown(_)));
    let _: ShellStreamItem = round_trip(json!({"kind":"synchronized"}));
    let _: ShellStreamItem = round_trip(
        json!({"kind":"snapshot","snapshot":{"schemaVersion":1,"snapshotSequence":3,
        "threads":[thread_shell()],"archivedThreads":[],"projects":[project_shell()]}}),
    );
    let _: ShellStreamItem =
        round_trip(json!({"kind":"project.updated","sequence":4,"project":project_shell()}));
    let _: ShellStreamItem =
        round_trip(json!({"kind":"project.removed","sequence":5,"projectId":P}));
    let _: ShellStreamItem =
        round_trip(json!({"kind":"thread.removed","sequence":6,"location":"active","threadId":T}));
}

// ---------------------------------------------------------------------------
// Real t3 data
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/t3/real/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"))
}

#[test]
fn real_thread_stream_items_decode() {
    let mut snapshots = 0;
    let mut synchronized = 0;
    let mut typed_events = 0;
    let mut unknown_events = Vec::new();
    let mut turn_items = 0;
    for line in fixture("thread_stream.ndjson").lines() {
        let item: ThreadStreamItem = round_trip(serde_json::from_str(line).unwrap());
        match item {
            ThreadStreamItem::Snapshot(s) => {
                snapshots += 1;
                turn_items += s.projection.turn_items.len();
            }
            ThreadStreamItem::Synchronized => synchronized += 1,
            ThreadStreamItem::Event(e) => match e.event.body {
                DomainEventBody::Unknown(body) => unknown_events.push(body["type"].clone()),
                DomainEventBody::TurnItemUpdated(item) => {
                    assert!(!matches!(item.kind, TurnItemKind::Unknown(_)));
                    turn_items += 1;
                    typed_events += 1;
                }
                _ => typed_events += 1,
            },
            ThreadStreamItem::Unknown(v) => panic!("unknown thread item {v}"),
        }
    }
    println!(
        "thread stream: {snapshots} snapshots, {synchronized} synchronized, {typed_events} typed events, \
         {} unknown events {unknown_events:?}, {turn_items} turn items",
        unknown_events.len()
    );
    assert_eq!((snapshots, synchronized, typed_events), (4, 3, 19));
    // `node.updated` is a real t3 event type j3 does not emit.
    assert_eq!(unknown_events, vec![json!("node.updated")]);
}

#[test]
fn real_shell_stream_items_decode() {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut threads = 0;
    for line in fixture("shell_stream.ndjson").lines() {
        let item: ShellStreamItem = round_trip(serde_json::from_str(line).unwrap());
        assert!(!matches!(item, ShellStreamItem::Unknown(_)));
        if let ShellStreamItem::Snapshot(s) = &item {
            threads += s.snapshot.threads.len();
        }
        *counts.entry(item.tag().unwrap().to_owned()).or_default() += 1;
    }
    println!("shell stream: {counts:?}, {threads} snapshot threads");
    assert_eq!(counts.values().sum::<usize>(), 16);
    assert_eq!(counts.len(), 6);
}

#[test]
fn real_projections_decode() {
    let projections: Vec<Value> = serde_json::from_str(&fixture("projections.json")).unwrap();
    let mut items = 0;
    for value in &projections {
        let projection: ThreadProjection = round_trip(value.clone());
        assert!(
            projection
                .turn_items
                .iter()
                .all(|item| !matches!(item.kind, TurnItemKind::Unknown(_)))
        );
        items += projection.turn_items.len();
        assert_eq!(
            projection.visible_turn_items.len(),
            projection.turn_items.len()
        );
    }
    println!(
        "projections: {} decoded, {items} turn items",
        projections.len()
    );
    assert_eq!(projections.len(), 4);
}

// ---------------------------------------------------------------------------
// Model selection, capabilities, errors, time
// ---------------------------------------------------------------------------

#[test]
fn model_selection_accepts_legacy_forms() {
    let canonical = json!({"instanceId":"omp","model":"m","options":[{"id":"thinking","value":"high"},{"id":"fast","value":true}]});
    let _: ModelSelection = round_trip(canonical.clone());
    let legacy: ModelSelection = serde_json::from_value(json!({"provider":"omp","model":"m",
        "options":{"thinking":" high ","fast":true,"":"x","blank":"  ","n":3}}))
    .unwrap();
    assert_eq!(legacy.instance_id, "omp");
    let mut options = legacy.options.clone().unwrap();
    options.sort_by(|a, b| b.id.cmp(&a.id));
    assert_eq!(
        serde_json::to_value(&options).unwrap(),
        json!([{"id":"thinking","value":"high"},{"id":"fast","value":true}])
    );
    let encoded = serde_json::to_value(&legacy).unwrap();
    assert!(encoded.get("provider").is_none());
    assert!(encoded["options"].is_array());
    // instanceId wins over provider; no options stays without options.
    let both: ModelSelection =
        serde_json::from_value(json!({"instanceId":"a","provider":"b","model":"m"})).unwrap();
    assert_eq!(
        serde_json::to_value(&both).unwrap(),
        json!({"instanceId":"a","model":"m"})
    );
    assert!(serde_json::from_value::<ModelSelection>(json!({"model":"m"})).is_err());
}

#[test]
fn omp_capabilities_equal_pi() {
    assert_eq!(
        serde_json::to_value(ProviderCapabilities::omp()).unwrap(),
        pi_capabilities()
    );
    // `runtimePolicy` has a decoding default.
    let mut without = pi_capabilities();
    without.as_object_mut().unwrap().remove("runtimePolicy");
    let decoded: ProviderCapabilities = serde_json::from_value(without).unwrap();
    assert_eq!(decoded, ProviderCapabilities::omp());
}

#[test]
fn typed_errors_carry_their_tag() {
    let mut dispatch =
        OrchestrationV2DispatchCommandError::new("c1", "thread.create", "Not supported by j3.");
    dispatch.detail = Some("use launchThread".into());
    assert_eq!(
        serde_json::to_value(&dispatch).unwrap(),
        json!({"_tag":"OrchestrationV2DispatchCommandError","commandId":"c1","commandType":"thread.create",
            "message":"Not supported by j3.","detail":"use launchThread"})
    );
    assert_eq!(
        serde_json::to_value(OrchestrationV2ThreadLaunchError::new("c", P, "nope")).unwrap(),
        json!({"_tag":"OrchestrationV2ThreadLaunchError","commandId":"c","projectId":P,"message":"nope"})
    );
    assert_eq!(
        serde_json::to_value(OrchestrationV2GetThreadProjectionError::new(
            T,
            "Thread not found."
        ))
        .unwrap(),
        json!({"_tag":"OrchestrationV2GetThreadProjectionError","threadId":T,"message":"Thread not found."})
    );
    assert_eq!(
        serde_json::to_value(OrchestrationV2GetShellSnapshotError::new("boom")).unwrap(),
        json!({"_tag":"OrchestrationV2GetShellSnapshotError","message":"boom"})
    );
    assert_eq!(
        serde_json::to_value(OrchestrationV2ForceStopThreadError::new(T)).unwrap(),
        json!({"_tag":"OrchestrationV2ForceStopThreadError","threadId":T})
    );
    let _: OrchestrationV2DispatchCommandError =
        round_trip(json!({"_tag":"OrchestrationV2DispatchCommandError",
        "commandId":"c","commandType":"x","message":"m","cause":{"anything":1}}));
    assert!(
        serde_json::from_value::<OrchestrationV2GetShellSnapshotError>(
            json!({"_tag":"Other","message":"m"})
        )
        .is_err()
    );
}

#[test]
fn iso_timestamps() {
    assert_eq!(iso_from_unix_ms(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(
        iso_from_unix_ms(1_793_534_400_123),
        "2026-11-01T12:00:00.123Z"
    );
    assert_eq!(iso_from_unix_ms(-1), "1969-12-31T23:59:59.999Z");
    let now = now_iso();
    assert_eq!(now.len(), 24, "{now}");
    assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T' && now.as_bytes()[19] == b'.');
}
