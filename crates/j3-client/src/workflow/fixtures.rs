//! Shell rows for the workflow tests: a decoded `ThreadShell` with t3's
//! defaults, patched per test like t3's `makeRawThreadShell`.

use j3_protocol::t3::orchestration::ThreadShell;
use serde_json::{Value, json};

/// A patch value that removes the key, for t3's `undefined` (an older server
/// that does not send the field).
pub const UNDEFINED: &str = "<undefined>";

pub fn shell(id: &str, patch: Value) -> ThreadShell {
    let mut row = json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": id,
        "projectId": "project-1",
        "title": id,
        "providerInstanceId": "omp",
        "modelSelection": { "instanceId": "omp", "model": "model" },
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": id },
        "forkedFrom": null,
        "activeProviderThreadId": null,
        "latestRunId": null,
        "activeRunId": null,
        "status": "idle",
        "pendingRuntimeRequest": null,
        "latestVisibleMessage": null,
        "latestUserMessageAt": null,
        "hasActionableProposedPlan": false,
        "itemCount": 0,
        "visibleItemCount": 0,
        "createdAt": "2026-03-09T10:00:00.000Z",
        "updatedAt": "2026-03-09T10:00:00.000Z",
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "lastVisitedAt": null,
        "deletedAt": null,
    });
    let object = row.as_object_mut().expect("fixture is an object");
    for (key, value) in patch.as_object().expect("patch is an object") {
        if value.as_str() == Some(UNDEFINED) {
            object.remove(key);
        } else {
            object.insert(key.clone(), value.clone());
        }
    }
    serde_json::from_value(row).expect("fixture decodes as a ThreadShell")
}

/// A pending runtime request summary of `kind` (`"user_input"`, `"command"`…).
pub fn request(kind: &str) -> Value {
    json!({ "id": "request-1", "kind": kind, "createdAt": "2026-03-09T10:01:00.000Z" })
}
