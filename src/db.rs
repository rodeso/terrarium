//! Server-only: the SQLite database where the workspace keeps data that must
//! survive restarts (notes, for now).
//!
//! The file lives at `data/workspace.db` (override with WORKSPACE_DB) and is
//! created, with its tables, on first run.

use crate::notes::{Note, NoteSummary, snippet};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_PATH: &str = "data/workspace.db";

/// Each step upgrades the schema by one version; never edit a step once
/// released, only append new ones.
const MIGRATIONS: &[&str] = &["CREATE TABLE notes (
        id         INTEGER PRIMARY KEY,
        title      TEXT    NOT NULL DEFAULT '',
        body       TEXT    NOT NULL DEFAULT '',
        pinned     INTEGER NOT NULL DEFAULT 0,
        created_ms INTEGER NOT NULL,
        updated_ms INTEGER NOT NULL
    );
    CREATE INDEX notes_order ON notes (pinned DESC, updated_ms DESC);"];

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub fn open_from_env() -> Result<Self, String> {
        let path = std::env::var("WORKSPACE_DB").ok().filter(|p| !p.trim().is_empty()).unwrap_or(DEFAULT_PATH.into());
        if let Some(dir) = Path::new(&path).parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
        }
        let conn = Connection::open(&path).map_err(|e| format!("Can't open database {path}: {e}"))?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(|e| e.to_string())?;
        migrate(&conn)?;
        println!("Database: {path}");
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    /// Runs `f` with the connection on a blocking-friendly thread.
    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        let inner = self.0.clone();
        actix_web::web::block(move || {
            let conn = inner.lock().map_err(|_| "The database lock was poisoned".to_string())?;
            f(&conn).map_err(|e| format!("Database error: {e}"))
        })
        .await
        .map_err(|e| format!("Database task failed: {e}"))?
    }

    // ---------------- Notes ----------------

    /// Pinned first, then most recently edited. `query` filters by title or
    /// body (case-insensitive for ASCII letters).
    pub async fn list_notes(&self, query: String) -> Result<Vec<NoteSummary>, String> {
        self.run(move |c| {
            let pattern = format!("%{}%", escape_like(query.trim()));
            let mut stmt = c.prepare(
                "SELECT id, title, substr(body, 1, 400), pinned, updated_ms FROM notes
                 WHERE ?1 = '%%' OR title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\'
                 ORDER BY pinned DESC, updated_ms DESC",
            )?;
            let rows = stmt.query_map(params![pattern], |r| {
                let body: String = r.get(2)?;
                Ok(NoteSummary {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    snippet: snippet(&body),
                    pinned: r.get::<_, i64>(3)? != 0,
                    updated_ms: r.get(4)?,
                })
            })?;
            rows.collect()
        })
        .await
    }

    pub async fn get_note(&self, id: i64) -> Result<Option<Note>, String> {
        self.run(move |c| {
            c.query_row(
                "SELECT id, title, body, pinned, created_ms, updated_ms FROM notes WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Note {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        body: r.get(2)?,
                        pinned: r.get::<_, i64>(3)? != 0,
                        created_ms: r.get(4)?,
                        updated_ms: r.get(5)?,
                    })
                },
            )
            .optional()
        })
        .await
    }

    /// Every note in full, most recently edited first (for the model's search).
    pub async fn all_notes(&self) -> Result<Vec<Note>, String> {
        self.run(|c| {
            let mut stmt =
                c.prepare("SELECT id, title, body, pinned, created_ms, updated_ms FROM notes ORDER BY updated_ms DESC")?;
            let rows = stmt.query_map([], |r| {
                Ok(Note {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    body: r.get(2)?,
                    pinned: r.get::<_, i64>(3)? != 0,
                    created_ms: r.get(4)?,
                    updated_ms: r.get(5)?,
                })
            })?;
            rows.collect()
        })
        .await
    }

    pub async fn create_note(&self) -> Result<Note, String> {
        self.run(|c| {
            let now = now_ms();
            c.execute("INSERT INTO notes (created_ms, updated_ms) VALUES (?1, ?1)", params![now])?;
            Ok(Note {
                id: c.last_insert_rowid(),
                title: String::new(),
                body: String::new(),
                pinned: false,
                created_ms: now,
                updated_ms: now,
            })
        })
        .await
    }

    /// Returns the new `updated_ms`, or None if the note no longer exists.
    pub async fn save_note(&self, id: i64, title: String, body: String) -> Result<Option<i64>, String> {
        self.run(move |c| {
            let now = now_ms();
            let changed = c.execute(
                "UPDATE notes SET title = ?2, body = ?3, updated_ms = ?4 WHERE id = ?1",
                params![id, title, body, now],
            )?;
            Ok((changed > 0).then_some(now))
        })
        .await
    }

    pub async fn set_pinned(&self, id: i64, pinned: bool) -> Result<(), String> {
        self.run(move |c| c.execute("UPDATE notes SET pinned = ?2 WHERE id = ?1", params![id, pinned as i64]).map(|_| ()))
            .await
    }

    pub async fn delete_note(&self, id: i64) -> Result<(), String> {
        self.run(move |c| c.execute("DELETE FROM notes WHERE id = ?1", params![id]).map(|_| ())).await
    }
}

fn migrate(conn: &Connection) -> Result<(), String> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(|e| e.to_string())?;
    for (i, step) in MIGRATIONS.iter().enumerate().skip(version.max(0) as usize) {
        let next = i as i64 + 1;
        conn.execute_batch(&format!("BEGIN; {step}; PRAGMA user_version = {next}; COMMIT;"))
            .map_err(|e| format!("Database migration {next} failed: {e}"))?;
    }
    Ok(())
}

/// Escapes LIKE wildcards so searching for "50%" finds the literal text.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
