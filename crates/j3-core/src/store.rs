//! SQLite persistence: the server identity, the sequence high-water mark,
//! projects, the thread index (one row per thread, holding its wire row),
//! each thread's records (turns, items, requests and its queue), and command
//! results for idempotent commands (`docs/protocol/wire.md` §3.4).
//!
//! The store is synchronous; [`crate::hub::Hub`] owns it behind a mutex and
//! runs each commit as one transaction.

use std::path::Path;

use j3_protocol::v2::{
    entities::{Item, Project, QueueEntry, Request, RequestStatus, Thread, Turn},
    streams::ThreadSnapshot,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::de::DeserializeOwned;
use serde_json::Value;

mod legacy;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database schema version {found} is newer than this build's {known}")]
    TooNew { found: u32, known: u32 },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One schema migration.
enum Migration {
    Sql(&'static str),
    Rust(fn(&Transaction<'_>) -> Result<()>),
}

/// Schema migrations in order; migration `n` (1-based) moves a database from
/// `user_version` `n - 1` to `n`. Databases written before versioning have
/// `user_version` 0 and already hold migration 1's tables, which is why it
/// only creates what is missing.
const MIGRATIONS: &[Migration] = &[
    Migration::Sql(
        "
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    session_file TEXT,
    archived INTEGER NOT NULL DEFAULT 0,
    deleted INTEGER NOT NULL DEFAULT 0,
    shell TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS entities (
    thread_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    json TEXT NOT NULL,
    PRIMARY KEY (thread_id, kind, id)
);
CREATE TABLE IF NOT EXISTS receipts (
    command_id TEXT PRIMARY KEY,
    result TEXT NOT NULL
);
",
    ),
    Migration::Rust(legacy::migrate),
];

const KEY_SERVER: &str = "environment_id";
const KEY_SEQUENCE: &str = "sequence";

/// One row of the thread index.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadIndex {
    pub id: String,
    pub project_id: String,
    /// omp's session file, once omp has started for this thread.
    pub session_file: Option<String>,
    pub deleted: bool,
    /// The thread's records are still to be rebuilt from its session file.
    pub replay: bool,
    /// The thread's wire row.
    pub row: Thread,
}

/// One stored entity of a thread: a turn, item or request, or its queue.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub kind: &'static str,
    pub id: String,
    pub ordinal: i64,
    pub json: String,
}

impl Record {
    pub fn new(kind: &'static str, id: &str, ordinal: i64, value: &Value) -> Self {
        Self {
            kind,
            id: id.to_owned(),
            ordinal,
            json: value.to_string(),
        }
    }
}

/// Everything stored about one thread's conversation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadRecords {
    pub turns: Vec<Turn>,
    pub items: Vec<Item>,
    pub requests: Vec<Request>,
    pub queue: Vec<QueueEntry>,
}

pub struct Store {
    conn: Connection,
    server_id: String,
    high_water: u64,
}

fn decode<T: DeserializeOwned>(json: &str) -> Result<T> {
    Ok(serde_json::from_str(json)?)
}

impl Store {
    /// Opens (creating and migrating if needed) the database at `path` in WAL mode.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    /// Private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        migrate(&mut conn)?;
        let server_id = match meta(&conn, KEY_SERVER)? {
            Some(id) => id,
            None => {
                let id = format!("env-{}", random_hex(16));
                set_meta(&conn, KEY_SERVER, &id)?;
                id
            }
        };
        let high_water = meta(&conn, KEY_SEQUENCE)?
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        Ok(Self {
            conn,
            server_id,
            high_water,
        })
    }

    /// Generated on first open, then stable for the life of the database.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Highest sequence committed so far (0 before any commit).
    pub fn high_water(&self) -> u64 {
        self.high_water
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut statement = self
            .conn
            .prepare_cached("SELECT json FROM projects ORDER BY rowid")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|json| decode(&json?)).collect()
    }

    pub fn project(&self, id: &str) -> Result<Option<Project>> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM projects WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        json.map(|json| decode(&json)).transpose()
    }

    /// Every thread that is not deleted, in creation order.
    pub fn threads(&self) -> Result<Vec<ThreadIndex>> {
        self.thread_rows("WHERE deleted = 0", [])
    }

    /// Threads whose records are still to be rebuilt from their session file.
    pub fn pending_replays(&self) -> Result<Vec<ThreadIndex>> {
        self.thread_rows("WHERE deleted = 0 AND replay = 1", [])
    }

    /// Thread `id`, deleted or not.
    pub fn thread(&self, id: &str) -> Result<Option<ThreadIndex>> {
        Ok(self.thread_rows("WHERE id = ?1", [id])?.pop())
    }

    fn thread_rows<P: rusqlite::Params>(
        &self,
        filter: &str,
        params: P,
    ) -> Result<Vec<ThreadIndex>> {
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT id, project_id, session_file, deleted, replay, row FROM threads {filter} ORDER BY rowid"
        ))?;
        let rows = statement.query_map(params, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (id, project_id, session_file, deleted, replay, row) = row?;
            Ok(ThreadIndex {
                id,
                project_id,
                session_file,
                deleted,
                replay,
                row: decode(&row)?,
            })
        })
        .collect()
    }

    /// The stored conversation of one thread, each kind in ordinal order.
    pub fn records(&self, thread_id: &str) -> Result<ThreadRecords> {
        let mut statement = self.conn.prepare_cached(
            "SELECT kind, json FROM records WHERE thread_id = ?1 ORDER BY kind, ordinal, id",
        )?;
        let rows = statement.query_map([thread_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut records = ThreadRecords::default();
        for row in rows {
            let (kind, json) = row?;
            match kind.as_str() {
                "turn" => records.turns.push(decode(&json)?),
                "item" => records.items.push(decode(&json)?),
                "request" => records.requests.push(decode(&json)?),
                "queue" => records.queue = decode(&json)?,
                _ => {}
            }
        }
        Ok(records)
    }

    /// `threads.watch`'s snapshot of a thread that is not deleted (wire.md
    /// §6.6): its requests are the open ones and those of the latest turn.
    pub fn snapshot(&self, thread_id: &str) -> Result<Option<ThreadSnapshot>> {
        let Some(index) = self.thread(thread_id)?.filter(|index| !index.deleted) else {
            return Ok(None);
        };
        let records = self.records(thread_id)?;
        let latest = records.turns.last().map(|turn| turn.id.clone());
        Ok(Some(ThreadSnapshot {
            sequence: self.high_water,
            thread: index.row,
            turns: records.turns,
            items: records.items,
            requests: records
                .requests
                .into_iter()
                .filter(|request| {
                    request.status == RequestStatus::Open
                        || Some(&request.turn_id) == latest.as_ref()
                })
                .collect(),
            queue: records.queue,
        }))
    }

    pub fn receipt(&self, command_id: &str) -> Result<Option<Value>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT result FROM receipts WHERE command_id = ?1",
                [command_id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| decode(&json)).transpose()
    }

    /// Starts a write transaction. Dropping it without
    /// [`StoreTx::commit`] rolls back.
    pub fn transaction(&mut self) -> Result<StoreTx<'_>> {
        let high_water = self.high_water;
        Ok(StoreTx {
            tx: self.conn.transaction()?,
            high_water: &mut self.high_water,
            next_high_water: high_water,
        })
    }
}

/// Writes of one commit.
pub struct StoreTx<'a> {
    tx: Transaction<'a>,
    high_water: &'a mut u64,
    next_high_water: u64,
}

impl StoreTx<'_> {
    /// Reserves the next sequence number.
    pub fn next_sequence(&mut self) -> u64 {
        self.next_high_water += 1;
        self.next_high_water
    }

    pub fn upsert_record(&self, thread_id: &str, record: &Record) -> Result<()> {
        self.tx
            .prepare_cached(
                "INSERT INTO records (thread_id, kind, id, ordinal, json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (thread_id, kind, id)
                 DO UPDATE SET ordinal = excluded.ordinal, json = excluded.json",
            )?
            .execute(params![
                thread_id,
                record.kind,
                record.id,
                record.ordinal,
                record.json
            ])?;
        Ok(())
    }

    /// Writes a thread's index row; its records are current (no replay
    /// pending).
    pub fn upsert_thread(
        &self,
        id: &str,
        project_id: &str,
        session_file: Option<&str>,
        row: &Value,
    ) -> Result<()> {
        self.tx
            .prepare_cached(
                "INSERT INTO threads (id, project_id, session_file, deleted, replay, row)
                 VALUES (?1, ?2, ?3, 0, 0, ?4)
                 ON CONFLICT (id) DO UPDATE SET project_id = excluded.project_id,
                   session_file = excluded.session_file, deleted = 0, replay = 0,
                   row = excluded.row",
            )?
            .execute(params![id, project_id, session_file, row.to_string()])?;
        Ok(())
    }

    /// Drops a thread's records and marks its index row deleted (the row
    /// stays so a later lookup reports "deleted", not "unknown").
    pub fn delete_thread(&self, thread_id: &str) -> Result<()> {
        self.tx
            .execute("DELETE FROM records WHERE thread_id = ?1", [thread_id])?;
        self.tx
            .execute("UPDATE threads SET deleted = 1 WHERE id = ?1", [thread_id])?;
        Ok(())
    }

    pub fn upsert_project(&self, project: &Project) -> Result<()> {
        self.tx
            .prepare_cached(
                "INSERT INTO projects (id, json) VALUES (?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET json = excluded.json",
            )?
            .execute(params![project.id, serde_json::to_string(project)?])?;
        Ok(())
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.tx
            .execute("DELETE FROM projects WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn put_receipt(&self, command_id: &str, result: &Value) -> Result<()> {
        self.tx.execute(
            "INSERT OR REPLACE INTO receipts (command_id, result) VALUES (?1, ?2)",
            params![command_id, result.to_string()],
        )?;
        Ok(())
    }

    /// Persists the high-water mark and commits.
    pub fn commit(self) -> Result<u64> {
        set_meta(&self.tx, KEY_SEQUENCE, &self.next_high_water.to_string())?;
        self.tx.commit()?;
        *self.high_water = self.next_high_water;
        Ok(self.next_high_water)
    }
}

fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Applies every migration past the database's `user_version`, each in its
/// own transaction together with the version bump.
fn migrate(conn: &mut Connection) -> Result<()> {
    let known = MIGRATIONS.len() as u32;
    let found: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > known {
        return Err(Error::TooNew { found, known });
    }
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let tx = conn.transaction()?;
        match migration {
            Migration::Sql(sql) => tx.execute_batch(sql)?,
            Migration::Rust(run) => run(&tx)?,
        }
        tx.pragma_update(None, "user_version", index as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// `len` lowercase hex characters from the OS random source.
pub fn random_hex(len: usize) -> String {
    use std::io::Read;
    let mut bytes = vec![0u8; len.div_ceil(2)];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .expect("reading /dev/urandom");
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out.truncate(len);
    out
}

#[cfg(test)]
mod tests {
    use j3_protocol::v2::entities::{ModelRef, ThreadMode, ThreadStatus, Triage};
    use serde_json::json;

    use super::*;

    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            title: "P".into(),
            path: "/p".into(),
            default_model: None,
            created_at: "2026-11-01T00:00:00.000Z".into(),
            updated_at: "2026-11-01T00:00:00.000Z".into(),
        }
    }

    pub(super) fn row(id: &str) -> Thread {
        Thread {
            id: id.into(),
            project_id: "p".into(),
            title: "T".into(),
            created_at: "2026-11-01T00:00:00.000Z".into(),
            updated_at: "2026-11-01T00:00:00.000Z".into(),
            archived_at: None,
            status: ThreadStatus::Idle,
            model: ModelRef {
                provider: "anthropic".into(),
                id: "claude".into(),
                thinking: None,
            },
            mode: ThreadMode::Default,
            active_turn_id: None,
            pending_request: None,
            queued: 0,
            last_message: None,
            latest_turn: None,
            latest_user_message_at: None,
            goal: None,
            triage: Triage {
                bumped_at: "2026-11-01T00:00:00.000Z".into(),
                ..Triage::default()
            },
        }
    }

    #[test]
    fn server_id_and_sequence_survive_reopen() {
        let dir = std::env::temp_dir().join(format!("j3-store-{}", random_hex(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("j3.sqlite");
        let (server_id, last) = {
            let mut store = Store::open(&path).unwrap();
            assert_eq!(store.high_water(), 0);
            let mut tx = store.transaction().unwrap();
            tx.next_sequence();
            tx.next_sequence();
            tx.upsert_project(&project("p1")).unwrap();
            let last = tx.commit().unwrap();
            (store.server_id().to_owned(), last)
        };
        assert_eq!(last, 2);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.server_id(), server_id);
        assert!(server_id.starts_with("env-"));
        assert_eq!(store.high_water(), 2);
        assert_eq!(store.projects().unwrap(), vec![project("p1")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_a_newer_schema() {
        let conn = Connection::open_in_memory().unwrap();
        let newer = MIGRATIONS.len() as u32 + 1;
        conn.pragma_update(None, "user_version", newer).unwrap();
        assert!(matches!(
            Store::init(conn),
            Err(Error::TooNew { found, .. }) if found == newer
        ));
    }

    #[test]
    fn a_dropped_transaction_rolls_back() {
        let mut store = Store::open_in_memory().unwrap();
        {
            let mut tx = store.transaction().unwrap();
            tx.next_sequence();
            tx.put_receipt("c1", &json!({"sequence": 1})).unwrap();
        }
        assert_eq!(store.high_water(), 0);
        assert_eq!(store.receipt("c1").unwrap(), None);
    }

    #[test]
    fn threads_and_records_round_trip() {
        let mut store = Store::open_in_memory().unwrap();
        let tx = store.transaction().unwrap();
        tx.upsert_thread("t1", "p", Some("/s.jsonl"), &json!(row("t1")))
            .unwrap();
        let turn = json!({"id": "turn_1", "threadId": "t1", "ordinal": 1, "status": "completed",
            "model": {"provider": "a", "id": "b"}, "requestedAt": "2026-11-01T00:00:00.000Z"});
        tx.upsert_record("t1", &Record::new("turn", "turn_1", 1, &turn))
            .unwrap();
        for ordinal in [2, 1] {
            let item = json!({"id": format!("item_{ordinal}"), "threadId": "t1", "turnId": "turn_1",
                "ordinal": ordinal, "status": "completed", "startedAt": "2026-11-01T00:00:00.000Z",
                "kind": "text", "text": "x"});
            tx.upsert_record(
                "t1",
                &Record::new("item", &format!("item_{ordinal}"), ordinal, &item),
            )
            .unwrap();
        }
        tx.upsert_record("t1", &Record::new("queue", "queue", 0, &json!([])))
            .unwrap();
        tx.commit().unwrap();
        let records = store.records("t1").unwrap();
        assert_eq!(records.turns.len(), 1);
        let ids: Vec<_> = records.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["item_1", "item_2"]);
        let index = store.thread("t1").unwrap().unwrap();
        assert_eq!(index.session_file.as_deref(), Some("/s.jsonl"));
        assert!(!index.replay && !index.deleted);
        let snapshot = store.snapshot("t1").unwrap().unwrap();
        assert_eq!(snapshot.items.len(), 2);

        let tx = store.transaction().unwrap();
        tx.delete_thread("t1").unwrap();
        tx.commit().unwrap();
        assert!(store.thread("t1").unwrap().unwrap().deleted);
        assert!(store.threads().unwrap().is_empty());
        assert!(store.snapshot("t1").unwrap().is_none());
        assert!(store.records("t1").unwrap().items.is_empty());
    }
}
