//! The terminal methods (`C/terminal.ts` = `T`, tags in `C/rpc.ts:227-234`):
//! `terminal.open`, the streaming `terminal.attach`, `terminal.write`,
//! `terminal.resize` and `terminal.close`. Sessions are keyed by thread and a
//! client-chosen terminal id (`term-1`, `term-2`, ...) and run on the server
//! host. `terminal.clear`, `terminal.restart` and the terminal event and
//! metadata subscriptions are cut.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ids::ThreadId;

/// `terminal.open` (`WsTerminalOpenRpc`, `C/rpc.ts:723-727`): success
/// [`TerminalSessionSnapshot`].
pub const TERMINAL_OPEN: &str = "terminal.open";
/// `terminal.attach`, a stream of [`TerminalAttachStreamEvent`]
/// (`WsTerminalAttachRpc`, `C/rpc.ts:729-734`).
pub const TERMINAL_ATTACH: &str = "terminal.attach";
/// `terminal.write` (`C/rpc.ts:736-739`): success void.
pub const TERMINAL_WRITE: &str = "terminal.write";
/// `terminal.resize` (`C/rpc.ts:741-744`): success void.
pub const TERMINAL_RESIZE: &str = "terminal.resize";
/// `terminal.close` (`C/rpc.ts:757-760`): success void.
pub const TERMINAL_CLOSE: &str = "terminal.close";

/// `DEFAULT_TERMINAL_ID` (`T:9`): the first shell of a thread.
pub const DEFAULT_TERMINAL_ID: &str = "term-1";
/// Largest `cols` the contract accepts (`T:12-14`).
pub const MAX_COLS: u16 = 1000;
/// Largest `rows` the contract accepts (`T:15-17`).
pub const MAX_ROWS: u16 = 500;

/// `TerminalEnvSchema` (`T:19-25`): extra environment for the shell.
pub type TerminalEnv = BTreeMap<String, String>;

/// `TerminalOpenInput` (`T:39-46`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenInput {
    pub thread_id: ThreadId,
    pub terminal_id: String,
    pub cwd: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub worktree_path: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<TerminalEnv>,
}

/// `TerminalAttachInput` (`T:49-57`). j3 starts a missing session in the
/// thread's project folder when `cwd` is absent (t3 needs a `cwd`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAttachInput {
    pub thread_id: ThreadId,
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::double_option"
    )]
    pub worktree_path: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<TerminalEnv>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_if_not_running: Option<bool>,
}

/// `TerminalWriteInput` (`T:60-63`). `data` is 1 to 65 536 characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWriteInput {
    pub thread_id: ThreadId,
    pub terminal_id: String,
    pub data: String,
}

/// `TerminalResizeInput` (`T:66-70`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizeInput {
    pub thread_id: ThreadId,
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

/// `TerminalCloseInput` (`T:86-90`). Without `terminalId` every terminal of
/// the thread closes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCloseInput {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_history: Option<bool>,
}

/// `TerminalSessionStatus` (`T:93`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSessionStatus {
    Starting,
    Running,
    Exited,
    Error,
}

/// `TerminalSessionSnapshot` (`T:96-110`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionSnapshot {
    pub thread_id: ThreadId,
    pub terminal_id: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub status: TerminalSessionStatus,
    pub pid: Option<u32>,
    /// Everything the shell printed so far (capped by the server).
    pub history: String,
    pub exit_code: Option<i64>,
    pub exit_signal: Option<i64>,
    /// Display title, at most 128 characters.
    pub label: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

/// `TerminalEventBaseSchema` (`T:152-156`): the fields every live event
/// shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEventBase {
    pub thread_id: ThreadId,
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

/// `TerminalAttachSnapshotEvent` (`T:218-221`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttachSnapshot {
    pub snapshot: TerminalSessionSnapshot,
}

/// `TerminalOutputEvent` (`T:164-168`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOutput {
    #[serde(flatten)]
    pub base: TerminalEventBase,
    pub data: String,
}

/// `TerminalExitedEvent` (`T:170-175`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExited {
    #[serde(flatten)]
    pub base: TerminalEventBase,
    pub exit_code: Option<i64>,
    pub exit_signal: Option<i64>,
}

/// `TerminalClosedEvent` / `TerminalClearedEvent` (`T:177-180`, `T:188-191`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalNotice {
    #[serde(flatten)]
    pub base: TerminalEventBase,
}

/// `TerminalErrorEvent` (`T:182-186`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalErrorEvent {
    #[serde(flatten)]
    pub base: TerminalEventBase,
    pub message: String,
}

/// `TerminalRestartedEvent` (`T:193-197`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRestarted {
    #[serde(flatten)]
    pub base: TerminalEventBase,
    pub snapshot: TerminalSessionSnapshot,
}

/// `TerminalActivityEvent` (`T:199-204`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalActivity {
    #[serde(flatten)]
    pub base: TerminalEventBase,
    pub has_running_subprocess: bool,
    pub label: String,
}

open_union! {
    /// `TerminalAttachStreamEvent` (`T:223-232`), tagged by `type`: a
    /// `snapshot` first, then live events.
    pub enum TerminalAttachStreamEvent {
        tag = "type";
        newtype {
            "snapshot" => Snapshot(TerminalAttachSnapshot),
            "output" => Output(TerminalOutput),
            "exited" => Exited(TerminalExited),
            "closed" => Closed(TerminalNotice),
            "error" => Error(TerminalErrorEvent),
            "cleared" => Cleared(TerminalNotice),
            "restarted" => Restarted(TerminalRestarted),
            "activity" => Activity(TerminalActivity),
        }
        unit {}
    }
}

/// `TerminalHistoryError.operation` (`T:279`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalHistoryOperation {
    Read,
    Truncate,
    Migrate,
}

/// `TerminalError` (`T:235-352`), tagged by `_tag`. t3 builds each one's
/// `message` from its fields; j3 also sends it, so a client sees the same
/// sentence. `cause` (`Schema.Defect`) is a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum TerminalError {
    /// Message `Terminal cwd does not exist: {cwd}`.
    TerminalCwdNotFoundError { cwd: String, message: String },
    /// Message `Terminal cwd is not a directory: {cwd}`.
    TerminalCwdNotDirectoryError { cwd: String, message: String },
    /// Message `Failed to access terminal cwd: {cwd}`.
    TerminalCwdStatError {
        cwd: String,
        cause: String,
        message: String,
    },
    /// Message `Failed to {operation} terminal history for thread: ...`.
    #[serde(rename_all = "camelCase")]
    TerminalHistoryError {
        operation: TerminalHistoryOperation,
        thread_id: ThreadId,
        terminal_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<String>,
        message: String,
    },
    /// Message `Unknown terminal thread: {threadId}, terminal: {terminalId}`.
    #[serde(rename_all = "camelCase")]
    TerminalSessionLookupError {
        thread_id: ThreadId,
        terminal_id: String,
        message: String,
    },
    /// Message `Terminal is not running for thread: ..., terminal: ...`.
    #[serde(rename_all = "camelCase")]
    TerminalNotRunningError {
        thread_id: ThreadId,
        terminal_id: String,
        message: String,
    },
    /// Message `Failed to write to terminal for thread: ..., PID: ...`.
    #[serde(rename_all = "camelCase")]
    TerminalWriteError {
        thread_id: ThreadId,
        terminal_id: String,
        terminal_pid: u32,
        cause: String,
        message: String,
    },
    /// Message `Failed to resize terminal for thread: ... to {cols}x{rows}`.
    #[serde(rename_all = "camelCase")]
    TerminalResizeError {
        thread_id: ThreadId,
        terminal_id: String,
        terminal_pid: u32,
        cols: u16,
        rows: u16,
        cause: String,
        message: String,
    },
}

impl TerminalError {
    pub fn cwd_not_found(cwd: impl Into<String>) -> Self {
        let cwd = cwd.into();
        Self::TerminalCwdNotFoundError {
            message: format!("Terminal cwd does not exist: {cwd}"),
            cwd,
        }
    }

    pub fn cwd_not_directory(cwd: impl Into<String>) -> Self {
        let cwd = cwd.into();
        Self::TerminalCwdNotDirectoryError {
            message: format!("Terminal cwd is not a directory: {cwd}"),
            cwd,
        }
    }

    pub fn cwd_stat(cwd: impl Into<String>, cause: impl Into<String>) -> Self {
        let cwd = cwd.into();
        Self::TerminalCwdStatError {
            message: format!("Failed to access terminal cwd: {cwd}"),
            cwd,
            cause: cause.into(),
        }
    }

    pub fn session_lookup(thread_id: impl Into<String>, terminal_id: impl Into<String>) -> Self {
        let (thread_id, terminal_id) = (thread_id.into(), terminal_id.into());
        Self::TerminalSessionLookupError {
            message: format!("Unknown terminal thread: {thread_id}, terminal: {terminal_id}"),
            thread_id,
            terminal_id,
        }
    }

    pub fn not_running(thread_id: impl Into<String>, terminal_id: impl Into<String>) -> Self {
        let (thread_id, terminal_id) = (thread_id.into(), terminal_id.into());
        Self::TerminalNotRunningError {
            message: format!(
                "Terminal is not running for thread: {thread_id}, terminal: {terminal_id}"
            ),
            thread_id,
            terminal_id,
        }
    }

    pub fn write(
        thread_id: impl Into<String>,
        terminal_id: impl Into<String>,
        terminal_pid: u32,
        cause: impl Into<String>,
    ) -> Self {
        let (thread_id, terminal_id) = (thread_id.into(), terminal_id.into());
        Self::TerminalWriteError {
            message: format!(
                "Failed to write to terminal for thread: {thread_id}, terminal: {terminal_id}, PID: {terminal_pid}"
            ),
            thread_id,
            terminal_id,
            terminal_pid,
            cause: cause.into(),
        }
    }

    pub fn resize(
        thread_id: impl Into<String>,
        terminal_id: impl Into<String>,
        terminal_pid: u32,
        (cols, rows): (u16, u16),
        cause: impl Into<String>,
    ) -> Self {
        let (thread_id, terminal_id) = (thread_id.into(), terminal_id.into());
        Self::TerminalResizeError {
            message: format!(
                "Failed to resize terminal for thread: {thread_id}, terminal: {terminal_id}, PID: {terminal_pid} to {cols}x{rows}"
            ),
            thread_id,
            terminal_id,
            terminal_pid,
            cols,
            rows,
            cause: cause.into(),
        }
    }

    /// The human-readable sentence.
    pub fn message(&self) -> &str {
        match self {
            Self::TerminalCwdNotFoundError { message, .. }
            | Self::TerminalCwdNotDirectoryError { message, .. }
            | Self::TerminalCwdStatError { message, .. }
            | Self::TerminalHistoryError { message, .. }
            | Self::TerminalSessionLookupError { message, .. }
            | Self::TerminalNotRunningError { message, .. }
            | Self::TerminalWriteError { message, .. }
            | Self::TerminalResizeError { message, .. } => message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> TerminalEventBase {
        TerminalEventBase {
            thread_id: "t1".into(),
            terminal_id: "term-1".into(),
            sequence: None,
        }
    }

    #[test]
    fn attach_input_omits_absent_fields_and_keeps_explicit_null() {
        let input = TerminalAttachInput {
            thread_id: "t1".into(),
            terminal_id: "term-1".into(),
            cwd: None,
            worktree_path: None,
            cols: Some(80),
            rows: Some(24),
            env: None,
            restart_if_not_running: Some(true),
        };
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            json!({"threadId": "t1", "terminalId": "term-1", "cols": 80, "rows": 24,
                   "restartIfNotRunning": true})
        );
        let decoded: TerminalAttachInput = serde_json::from_value(json!({
            "threadId": "t1", "terminalId": "term-2", "cwd": "/repo", "worktreePath": null,
            "env": {"FOO": "bar"}
        }))
        .unwrap();
        assert_eq!(decoded.worktree_path, Some(None));
        assert_eq!(decoded.env.unwrap()["FOO"], "bar");
        assert_eq!(decoded.cols, None);
    }

    #[test]
    fn snapshot_events_round_trip() {
        let wire = json!({"type": "snapshot", "snapshot": {
            "threadId": "t1", "terminalId": "term-1", "cwd": "/repo", "worktreePath": null,
            "status": "running", "pid": 42, "history": "$ ", "exitCode": null,
            "exitSignal": null, "label": "Terminal 1", "updatedAt": "2026-01-01T00:00:00.000Z"
        }});
        let event: TerminalAttachStreamEvent = serde_json::from_value(wire.clone()).unwrap();
        let TerminalAttachStreamEvent::Snapshot(TerminalAttachSnapshot { snapshot }) = &event
        else {
            panic!("expected a snapshot, got {event:?}");
        };
        assert_eq!(snapshot.status, TerminalSessionStatus::Running);
        assert_eq!(snapshot.pid, Some(42));
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }

    #[test]
    fn live_events_carry_the_shared_fields_and_their_type() {
        let output = TerminalAttachStreamEvent::Output(TerminalOutput {
            base: base(),
            data: "hi\r\n".into(),
        });
        assert_eq!(
            serde_json::to_value(&output).unwrap(),
            json!({"type": "output", "threadId": "t1", "terminalId": "term-1", "data": "hi\r\n"})
        );
        let exited: TerminalAttachStreamEvent = serde_json::from_value(json!({
            "type": "exited", "threadId": "t1", "terminalId": "term-1", "sequence": 7,
            "exitCode": 0, "exitSignal": null
        }))
        .unwrap();
        assert_eq!(
            exited,
            TerminalAttachStreamEvent::Exited(TerminalExited {
                base: TerminalEventBase {
                    sequence: Some(7),
                    ..base()
                },
                exit_code: Some(0),
                exit_signal: None,
            })
        );
        let closed = TerminalAttachStreamEvent::Closed(TerminalNotice { base: base() });
        assert_eq!(
            serde_json::to_value(&closed).unwrap(),
            json!({"type": "closed", "threadId": "t1", "terminalId": "term-1"})
        );
        let activity: TerminalAttachStreamEvent = serde_json::from_value(json!({
            "type": "activity", "threadId": "t1", "terminalId": "term-1",
            "hasRunningSubprocess": true, "label": "vim"
        }))
        .unwrap();
        assert!(matches!(
            activity,
            TerminalAttachStreamEvent::Activity(TerminalActivity {
                has_running_subprocess: true,
                ..
            })
        ));
    }

    #[test]
    fn unknown_event_types_are_kept_verbatim() {
        let wire = json!({"type": "bell", "threadId": "t1", "terminalId": "term-1"});
        let event: TerminalAttachStreamEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(event.tag(), Some("bell"));
        assert!(matches!(event, TerminalAttachStreamEvent::Unknown(_)));
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }

    #[test]
    fn errors_are_tagged_and_carry_their_message() {
        assert_eq!(
            serde_json::to_value(TerminalError::not_running("t1", "term-1")).unwrap(),
            json!({"_tag": "TerminalNotRunningError", "threadId": "t1", "terminalId": "term-1",
                   "message": "Terminal is not running for thread: t1, terminal: term-1"})
        );
        let resize = TerminalError::resize("t1", "term-1", 9, (80, 24), "EBADF");
        assert_eq!(
            serde_json::to_value(&resize).unwrap(),
            json!({"_tag": "TerminalResizeError", "threadId": "t1", "terminalId": "term-1",
                   "terminalPid": 9, "cols": 80, "rows": 24, "cause": "EBADF",
                   "message": "Failed to resize terminal for thread: t1, terminal: term-1, PID: 9 to 80x24"})
        );
        let decoded: TerminalError = serde_json::from_value(json!({
            "_tag": "TerminalCwdNotFoundError", "cwd": "/gone",
            "message": "Terminal cwd does not exist: /gone"
        }))
        .unwrap();
        assert_eq!(decoded, TerminalError::cwd_not_found("/gone"));
        assert_eq!(decoded.message(), "Terminal cwd does not exist: /gone");
    }
}
