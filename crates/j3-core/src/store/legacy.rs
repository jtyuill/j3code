//! Migration 2: from the first protocol's store to protocol v2's.
//!
//! - Projects become v2 [`Project`]s; deleted ones are dropped.
//! - The thread index keeps its rows. Each row's wire row is built from the
//!   old thread list row and thread entity: title, model, mode, archive and
//!   every triage field (`docs/triage.md` §2), plus the goal of the old
//!   provider-thread entity. Rows that were not deleted are marked for
//!   replay: their turns and items are rebuilt from the omp session file
//!   when the server next opens ([`crate::app`]), because only omp's file
//!   holds the conversation in a form v2 can read.
//! - The old entities and command results are dropped.

use j3_protocol::v2::entities::{
    Goal, GoalState, ModelRef, Project, Settled, ThinkingLevel, Thread, ThreadMode, ThreadStatus,
    Triage,
};
use rusqlite::{Transaction, params};
use serde_json::Value;

use super::Result;

/// Runs migration 2 inside `tx`.
pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    migrate_projects(tx)?;
    tx.execute_batch(
        "CREATE TABLE threads_v2 (
             id TEXT PRIMARY KEY,
             project_id TEXT NOT NULL,
             session_file TEXT,
             deleted INTEGER NOT NULL DEFAULT 0,
             replay INTEGER NOT NULL DEFAULT 0,
             row TEXT NOT NULL
         );
         CREATE TABLE records (
             thread_id TEXT NOT NULL,
             kind TEXT NOT NULL,
             id TEXT NOT NULL,
             ordinal INTEGER NOT NULL,
             json TEXT NOT NULL,
             PRIMARY KEY (thread_id, kind, id)
         );",
    )?;
    migrate_threads(tx)?;
    tx.execute_batch(
        "DROP TABLE entities;
         DROP TABLE threads;
         ALTER TABLE threads_v2 RENAME TO threads;
         DELETE FROM receipts;",
    )?;
    Ok(())
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// An old model selection (`{instanceId, model: "provider/id", options:
/// [{id: "thinking", value}]}`).
fn model_ref(selection: &Value) -> Option<ModelRef> {
    let model = text(selection, "model")?;
    let (provider, id) = match model.split_once('/') {
        Some((provider, id)) => (provider.to_owned(), id.to_owned()),
        None => (text(selection, "instanceId").unwrap_or_default(), model),
    };
    let thinking = selection
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|option| option.get("id").and_then(Value::as_str) == Some("thinking"))
        .and_then(|option| option.get("value").cloned())
        .and_then(|level| serde_json::from_value::<ThinkingLevel>(level).ok());
    Some(ModelRef {
        provider,
        id,
        thinking,
    })
}

fn migrate_projects(tx: &Transaction<'_>) -> Result<()> {
    let rows: Vec<(String, String)> = tx
        .prepare("SELECT id, json FROM projects")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, json) in rows {
        let old: Value = serde_json::from_str(&json)?;
        if old.get("deletedAt").is_some_and(|at| !at.is_null()) {
            tx.execute("DELETE FROM projects WHERE id = ?1", [&id])?;
            continue;
        }
        let created_at = text(&old, "createdAt").unwrap_or_default();
        let project = Project {
            id: id.clone(),
            title: text(&old, "title").unwrap_or_else(|| "Project".to_owned()),
            path: text(&old, "workspaceRoot").unwrap_or_default(),
            default_model: old.get("defaultModelSelection").and_then(model_ref),
            updated_at: text(&old, "updatedAt").unwrap_or_else(|| created_at.clone()),
            created_at,
        };
        tx.execute(
            "UPDATE projects SET json = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&project)?],
        )?;
    }
    Ok(())
}

/// The goal of an old provider-thread entity.
fn goal(old: &Value) -> Option<Goal> {
    let state = match old.get("status")?.as_str()? {
        "active" => GoalState::Active,
        "paused" | "blocked" | "usage_limited" | "usageLimited" | "usage-limited" => {
            GoalState::Paused
        }
        "budget_limited" | "budgetLimited" | "budget-limited" => GoalState::BudgetLimited,
        "complete" => GoalState::Complete,
        _ => return None,
    };
    Some(Goal {
        objective: text(old, "objective")?,
        state,
        tokens_used: old.get("tokensUsed").and_then(Value::as_u64),
        budget: old.get("tokenBudget").and_then(Value::as_u64),
    })
}

/// The old entity of `kind` of thread `id`, as JSON.
fn entity(tx: &Transaction<'_>, thread_id: &str, kind: &str) -> Result<Option<Value>> {
    let json: Option<String> = tx
        .prepare_cached("SELECT json FROM entities WHERE thread_id = ?1 AND kind = ?2 LIMIT 1")?
        .query_row([thread_id, kind], |row| row.get(0))
        .ok();
    Ok(json.map(|json| serde_json::from_str(&json)).transpose()?)
}

/// The v2 row of an old thread. Its conversation-derived fields (status,
/// turns, messages) are filled in by the replay.
fn thread_row(
    id: &str,
    project_id: &str,
    archived: bool,
    shell: &Value,
    thread: Option<&Value>,
    goal: Option<Goal>,
) -> Thread {
    // The thread entity holds every triage field; older rows only the list row.
    let field = |key: &str| {
        thread
            .and_then(|thread| text(thread, key))
            .or_else(|| text(shell, key))
    };
    let created_at = field("createdAt").unwrap_or_default();
    let updated_at = field("updatedAt").unwrap_or_else(|| created_at.clone());
    let settled = field("settledOverride").and_then(|value| match value.as_str() {
        "settled" => Some(Settled::Settled),
        "active" => Some(Settled::Active),
        _ => None,
    });
    let selection = thread
        .and_then(|thread| thread.get("modelSelection"))
        .or_else(|| shell.get("modelSelection"));
    Thread {
        id: id.to_owned(),
        project_id: project_id.to_owned(),
        title: field("title").unwrap_or_else(|| "New thread".to_owned()),
        archived_at: field("archivedAt").or_else(|| archived.then(|| updated_at.clone())),
        status: ThreadStatus::Idle,
        model: selection.and_then(model_ref).unwrap_or(ModelRef {
            provider: String::new(),
            id: String::new(),
            thinking: None,
        }),
        mode: if field("interactionMode").as_deref() == Some("plan") {
            ThreadMode::Plan
        } else {
            ThreadMode::Default
        },
        active_turn_id: None,
        pending_request: None,
        queued: 0,
        last_message: None,
        latest_turn: None,
        latest_user_message_at: text(shell, "latestUserMessageAt"),
        goal,
        triage: Triage {
            visited_at: field("lastVisitedAt"),
            settled,
            settled_at: field("settledAt"),
            unsettled_at: field("unsettledAt"),
            snoozed_until: field("snoozedUntil"),
            snoozed_at: field("snoozedAt"),
            last_wake_at: field("lastSnoozeWakeAt"),
            pinned_at: field("pinnedAt"),
            pin_order: field("pinOrderKey"),
            active_order: field("activeOrderKey"),
            auto_settle_off_at: field("autoSettleDisabledAt"),
            bumped_at: updated_at.clone(),
        },
        created_at,
        updated_at,
    }
}

fn migrate_threads(tx: &Transaction<'_>) -> Result<()> {
    type OldRow = (String, String, Option<String>, bool, bool, String);
    let rows: Vec<OldRow> = tx
        .prepare("SELECT id, project_id, session_file, archived, deleted, shell FROM threads ORDER BY rowid")?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (id, project_id, session_file, archived, deleted, shell) in rows {
        let shell: Value = serde_json::from_str(&shell)?;
        let thread = entity(tx, &id, "thread")?;
        let goal = entity(tx, &id, "providerThread")?
            .as_ref()
            .and_then(|provider| provider.get("goal"))
            .and_then(goal);
        let row = thread_row(&id, &project_id, archived, &shell, thread.as_ref(), goal);
        tx.execute(
            "INSERT INTO threads_v2 (id, project_id, session_file, deleted, replay, row)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                project_id,
                session_file,
                deleted,
                !deleted,
                serde_json::to_string(&row)?
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;

    use super::super::{MIGRATIONS, Migration, Store};
    use super::*;

    /// A database as the first protocol's server left it.
    fn old_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        let Migration::Sql(schema) = &MIGRATIONS[0] else {
            unreachable!()
        };
        conn.execute_batch(schema).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('environment_id', 'env-1'), ('sequence', '41')",
            [],
        )
        .unwrap();
        let project = json!({"id": "p1", "title": "Demo", "workspaceRoot": "/w",
            "defaultModelSelection": {"instanceId": "omp", "model": "anthropic/claude",
                "options": [{"id": "thinking", "value": "high"}]},
            "scripts": [], "createdAt": "2026-11-01T00:00:00.000Z",
            "updatedAt": "2026-11-02T00:00:00.000Z", "deletedAt": null});
        let gone = json!({"id": "p2", "title": "Gone", "workspaceRoot": "/g",
            "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z",
            "deletedAt": "2026-11-03T00:00:00.000Z"});
        for project in [project, gone] {
            conn.execute(
                "INSERT INTO projects (id, json) VALUES (?1, ?2)",
                params![project["id"].as_str().unwrap(), project.to_string()],
            )
            .unwrap();
        }
        let shell = json!({"id": "t1", "projectId": "p1", "title": "Fix it",
            "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-04T00:00:00.000Z",
            "archivedAt": null, "interactionMode": "plan",
            "modelSelection": {"instanceId": "omp", "model": "openai/gpt",
                "options": [{"id": "thinking", "value": "low"}]},
            "settledOverride": "settled", "settledAt": "2026-11-04T00:00:00.000Z",
            "pinnedAt": null, "pinOrderKey": null, "activeOrderKey": "m",
            "snoozedUntil": null, "snoozedAt": null, "unsettledAt": null,
            "autoSettleDisabledAt": "2026-11-02T00:00:00.000Z",
            "lastVisitedAt": "2026-11-03T00:00:00.000Z",
            "latestUserMessageAt": "2026-11-03T00:00:00.000Z"});
        conn.execute(
            "INSERT INTO threads (id, project_id, session_file, archived, deleted, shell)
             VALUES ('t1', 'p1', '/s/t1.jsonl', 0, 0, ?1),
                    ('t2', 'p1', NULL, 1, 0, ?2),
                    ('gone', 'p1', NULL, 0, 1, ?2)",
            params![
                shell.to_string(),
                json!({"title": "Old", "updatedAt": "2026-11-05T00:00:00.000Z"}).to_string()
            ],
        )
        .unwrap();
        let thread = json!({"id": "t1", "lastSnoozeWakeAt": "2026-11-02T12:00:00.000Z"});
        let provider = json!({"id": "pth_t1", "goal": {"objective": "Ship", "status": "active",
            "tokensUsed": 10, "tokenBudget": 100}});
        for (kind, json) in [
            ("thread", thread),
            ("providerThread", provider),
            ("turnItem", json!({})),
        ] {
            conn.execute(
                "INSERT INTO entities (thread_id, kind, id, ordinal, json) VALUES ('t1', ?1, ?1, 0, ?2)",
                params![kind, json.to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO receipts (command_id, result) VALUES ('c', '{}')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn migration_keeps_projects_and_the_thread_index_with_triage() {
        let store = Store::init(old_database()).unwrap();
        assert_eq!(store.server_id(), "env-1");
        assert_eq!(store.high_water(), 41);
        let projects = store.projects().unwrap();
        assert_eq!(projects.len(), 1, "deleted projects are dropped");
        assert_eq!(projects[0].path, "/w");
        let default = projects[0].default_model.as_ref().unwrap();
        assert_eq!(
            (default.provider.as_str(), default.id.as_str()),
            ("anthropic", "claude")
        );
        assert_eq!(default.thinking, Some(ThinkingLevel::High));

        let threads = store.threads().unwrap();
        assert_eq!(threads.len(), 2, "deleted threads stay deleted");
        let t1 = &threads[0];
        assert!(t1.replay);
        assert_eq!(t1.session_file.as_deref(), Some("/s/t1.jsonl"));
        let row = &t1.row;
        assert_eq!(row.title, "Fix it");
        assert_eq!(row.mode, ThreadMode::Plan);
        assert_eq!(row.model.id, "gpt");
        assert_eq!(row.model.thinking, Some(ThinkingLevel::Low));
        assert_eq!(row.triage.settled, Some(Settled::Settled));
        assert_eq!(row.triage.active_order.as_deref(), Some("m"));
        assert_eq!(
            row.triage.auto_settle_off_at.as_deref(),
            Some("2026-11-02T00:00:00.000Z")
        );
        assert_eq!(
            row.triage.visited_at.as_deref(),
            Some("2026-11-03T00:00:00.000Z")
        );
        assert_eq!(
            row.triage.last_wake_at.as_deref(),
            Some("2026-11-02T12:00:00.000Z")
        );
        assert_eq!(row.triage.bumped_at, "2026-11-04T00:00:00.000Z");
        assert_eq!(row.goal.as_ref().unwrap().state, GoalState::Active);
        let t2 = &threads[1];
        assert_eq!(
            t2.row.archived_at.as_deref(),
            Some("2026-11-05T00:00:00.000Z")
        );
        assert!(store.thread("gone").unwrap().unwrap().deleted);
        assert!(store.records("t1").unwrap().items.is_empty());
        assert_eq!(store.receipt("c").unwrap(), None);
        assert_eq!(store.pending_replays().unwrap().len(), 2);
    }

    #[test]
    fn a_fresh_database_gets_the_current_schema() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.threads().unwrap().is_empty());
        assert!(store.server_id().starts_with("env-"));
    }
}
