//! SQLite persistence: environment identity, the global sequence high-water
//! mark, projects, the thread index, every thread projection entity, and
//! command receipts for idempotent dispatch.
//!
//! The store is synchronous; [`crate::hub::Hub`] owns it behind a mutex and
//! runs each commit as one transaction.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

const SCHEMA: &str = "
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
";

const KEY_ENVIRONMENT: &str = "environment_id";
const KEY_SEQUENCE: &str = "sequence";

/// One row of the thread index.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadRow {
    pub id: String,
    pub project_id: String,
    /// omp session transcript, once omp has started for this thread.
    pub session_file: Option<String>,
    pub archived: bool,
    pub deleted: bool,
    /// Current `OrchestrationV2ThreadShell` JSON.
    pub shell: Value,
}

/// One projection entity of a thread.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityRow {
    pub kind: String,
    pub id: String,
    pub ordinal: i64,
    pub json: Value,
}

pub struct Store {
    conn: Connection,
    environment_id: String,
    high_water: u64,
}

impl Store {
    /// Opens (creating if needed) the database at `path` in WAL mode.
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

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        let environment_id = match meta(&conn, KEY_ENVIRONMENT)? {
            Some(id) => id,
            None => {
                let id = format!("env-{}", random_hex(16));
                set_meta(&conn, KEY_ENVIRONMENT, &id)?;
                id
            }
        };
        let high_water = meta(&conn, KEY_SEQUENCE)?
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        Ok(Self {
            conn,
            environment_id,
            high_water,
        })
    }

    /// Generated on first open, then stable for the life of the database.
    pub fn environment_id(&self) -> &str {
        &self.environment_id
    }

    /// Highest sequence assigned so far (0 before any commit).
    pub fn high_water(&self) -> u64 {
        self.high_water
    }

    pub fn projects(&self) -> Result<Vec<Value>> {
        let mut statement = self
            .conn
            .prepare_cached("SELECT json FROM projects ORDER BY rowid")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|json| Ok(serde_json::from_str(&json?)?)).collect()
    }

    pub fn project(&self, id: &str) -> Result<Option<Value>> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM projects WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(json.map(|json| serde_json::from_str(&json)).transpose()?)
    }

    /// Every thread that is not deleted, in creation order.
    pub fn threads(&self) -> Result<Vec<ThreadRow>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT id, project_id, session_file, archived, deleted, shell
             FROM threads WHERE deleted = 0 ORDER BY rowid",
        )?;
        let rows = statement.query_map([], thread_row)?;
        rows.map(|row| row?.decode()).collect()
    }

    pub fn thread(&self, id: &str) -> Result<Option<ThreadRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, project_id, session_file, archived, deleted, shell
                 FROM threads WHERE id = ?1",
                [id],
                thread_row,
            )
            .optional()?;
        row.map(RawThreadRow::decode).transpose()
    }

    /// All entities of one thread, ordered by kind, ordinal, then id.
    pub fn entities(&self, thread_id: &str) -> Result<Vec<EntityRow>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT kind, id, ordinal, json FROM entities
             WHERE thread_id = ?1 ORDER BY kind, ordinal, id",
        )?;
        let rows = statement.query_map([thread_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (kind, id, ordinal, json) = row?;
            Ok(EntityRow {
                kind,
                id,
                ordinal,
                json: serde_json::from_str(&json)?,
            })
        })
        .collect()
    }

    /// One entity of one thread.
    pub fn entity(&self, thread_id: &str, kind: &str, id: &str) -> Result<Option<Value>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM entities WHERE thread_id = ?1 AND kind = ?2 AND id = ?3",
                params![thread_id, kind, id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(json.map(|json| serde_json::from_str(&json)).transpose()?)
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
        Ok(json.map(|json| serde_json::from_str(&json)).transpose()?)
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

    /// Last sequence reserved in this transaction, or the stored high water.
    pub fn last_sequence(&self) -> u64 {
        self.next_high_water
    }

    pub fn upsert_entity(&self, thread_id: &str, row: &EntityRow) -> Result<()> {
        self.tx
            .prepare_cached(
                "INSERT INTO entities (thread_id, kind, id, ordinal, json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (thread_id, kind, id)
                 DO UPDATE SET ordinal = excluded.ordinal, json = excluded.json",
            )?
            .execute(params![
                thread_id,
                row.kind,
                row.id,
                row.ordinal,
                serde_json::to_string(&row.json)?
            ])?;
        Ok(())
    }

    pub fn upsert_thread(&self, row: &ThreadRow) -> Result<()> {
        self.tx
            .prepare_cached(
                "INSERT INTO threads (id, project_id, session_file, archived, deleted, shell)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO UPDATE SET project_id = excluded.project_id,
                   session_file = excluded.session_file, archived = excluded.archived,
                   deleted = excluded.deleted, shell = excluded.shell",
            )?
            .execute(params![
                row.id,
                row.project_id,
                row.session_file,
                row.archived,
                row.deleted,
                serde_json::to_string(&row.shell)?
            ])?;
        Ok(())
    }

    /// Drops a thread's entities and marks its index row deleted (the row
    /// stays so a later lookup reports "deleted", not "unknown").
    pub fn delete_thread(&self, thread_id: &str) -> Result<()> {
        self.tx
            .execute("DELETE FROM entities WHERE thread_id = ?1", [thread_id])?;
        self.tx
            .execute("UPDATE threads SET deleted = 1 WHERE id = ?1", [thread_id])?;
        Ok(())
    }

    pub fn upsert_project(&self, id: &str, json: &Value) -> Result<()> {
        self.tx
            .prepare_cached(
                "INSERT INTO projects (id, json) VALUES (?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET json = excluded.json",
            )?
            .execute(params![id, serde_json::to_string(json)?])?;
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
            params![command_id, serde_json::to_string(result)?],
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

struct RawThreadRow {
    id: String,
    project_id: String,
    session_file: Option<String>,
    archived: bool,
    deleted: bool,
    shell: String,
}

impl RawThreadRow {
    fn decode(self) -> Result<ThreadRow> {
        Ok(ThreadRow {
            id: self.id,
            project_id: self.project_id,
            session_file: self.session_file,
            archived: self.archived,
            deleted: self.deleted,
            shell: serde_json::from_str(&self.shell)?,
        })
    }
}

fn thread_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawThreadRow> {
    Ok(RawThreadRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        session_file: row.get(2)?,
        archived: row.get(3)?,
        deleted: row.get(4)?,
        shell: row.get(5)?,
    })
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
    use super::*;
    use serde_json::json;

    #[test]
    fn engine_store_environment_id_and_sequence_survive_reopen() {
        let dir = std::env::temp_dir().join(format!("j3-store-{}", random_hex(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("j3.sqlite");
        let (environment_id, last) = {
            let mut store = Store::open(&path).unwrap();
            assert_eq!(store.high_water(), 0);
            let mut tx = store.transaction().unwrap();
            tx.next_sequence();
            tx.next_sequence();
            tx.upsert_project("p1", &json!({"id": "p1"})).unwrap();
            let last = tx.commit().unwrap();
            (store.environment_id().to_owned(), last)
        };
        assert_eq!(last, 2);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.environment_id(), environment_id);
        assert!(environment_id.starts_with("env-"));
        assert_eq!(store.high_water(), 2);
        assert_eq!(store.projects().unwrap(), vec![json!({"id": "p1"})]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn engine_store_dropped_transaction_rolls_back_sequence() {
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
    fn engine_store_entities_round_trip_in_order() {
        let mut store = Store::open_in_memory().unwrap();
        let tx = store.transaction().unwrap();
        for (id, ordinal) in [("b", 2), ("a", 1), ("c", 2)] {
            tx.upsert_entity(
                "t1",
                &EntityRow {
                    kind: "turnItem".into(),
                    id: id.into(),
                    ordinal,
                    json: json!({"id": id}),
                },
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let ids: Vec<_> = store
            .entities("t1")
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }
}
