//! Server-only: the SQLite database where the app keeps data that must
//! survive restarts (notes, for now).
//!
//! The file lives at `data/terrarium.db` (override with TERRARIUM_DB) and is
//! created, with its tables, on first run.

use crate::calendar::{Event, EventInput};
use crate::news::{Article, ArticleSummary, FeedInfo};
use crate::notes::{Note, NoteSummary, snippet};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_PATH: &str = "data/terrarium.db";

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
    CREATE INDEX notes_order ON notes (pinned DESC, updated_ms DESC);",
    "CREATE TABLE feeds (
        id          INTEGER PRIMARY KEY,
        url         TEXT    NOT NULL UNIQUE,
        title       TEXT    NOT NULL DEFAULT '',
        site_url    TEXT,
        added_ms    INTEGER NOT NULL,
        fetched_ms  INTEGER,
        last_error  TEXT
    );
    CREATE TABLE articles (
        id           INTEGER PRIMARY KEY,
        feed_id      INTEGER NOT NULL REFERENCES feeds (id) ON DELETE CASCADE,
        guid         TEXT    NOT NULL,
        url          TEXT,
        title        TEXT    NOT NULL,
        snippet      TEXT    NOT NULL DEFAULT '',
        content_html TEXT    NOT NULL DEFAULT '',
        author       TEXT,
        published_ms INTEGER NOT NULL,
        read         INTEGER NOT NULL DEFAULT 0,
        UNIQUE (feed_id, guid)
    );
    CREATE INDEX articles_by_date ON articles (published_ms DESC);
    CREATE INDEX articles_by_feed ON articles (feed_id, published_ms DESC);",
    // Dates and times are plain local values ("2026-11-14", "15:30"), like a
    // paper diary; a NULL start_time means an all-day event.
    "CREATE TABLE events (
        id          INTEGER PRIMARY KEY,
        title       TEXT    NOT NULL,
        date        TEXT    NOT NULL,
        start_time  TEXT,
        end_time    TEXT,
        location    TEXT    NOT NULL DEFAULT '',
        notes       TEXT    NOT NULL DEFAULT '',
        created_ms  INTEGER NOT NULL,
        updated_ms  INTEGER NOT NULL
    );
    CREATE INDEX events_by_date ON events (date, start_time);"];

/// Oldest articles beyond this many per feed are dropped after a refresh.
const MAX_ARTICLES_PER_FEED: i64 = 300;

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub fn open_from_env() -> Result<Self, String> {
        let path = std::env::var("TERRARIUM_DB").ok().filter(|p| !p.trim().is_empty()).unwrap_or(DEFAULT_PATH.into());
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

/// An article as parsed from a feed, before it has an id.
pub struct NewArticle {
    pub guid: String,
    pub url: Option<String>,
    pub title: String,
    pub snippet: String,
    pub content_html: String,
    pub author: Option<String>,
    pub published_ms: i64,
}

impl Db {
    // ---------------- News ----------------

    pub async fn feeds(&self) -> Result<Vec<FeedInfo>, String> {
        self.run(|c| {
            let mut stmt = c.prepare(
                "SELECT f.id, f.title, f.url, f.site_url, f.last_error, f.fetched_ms,
                        (SELECT count(*) FROM articles a WHERE a.feed_id = f.id AND a.read = 0)
                 FROM feeds f ORDER BY lower(f.title), f.id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(FeedInfo {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    url: r.get(2)?,
                    site_url: r.get(3)?,
                    last_error: r.get(4)?,
                    fetched_ms: r.get(5)?,
                    unread: r.get(6)?,
                })
            })?;
            rows.collect()
        })
        .await
    }

    /// Newest first, at most `limit`.
    pub async fn articles(&self, feed: Option<i64>, unread_only: bool, limit: i64) -> Result<Vec<ArticleSummary>, String> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT a.id, a.feed_id, f.title, a.title, a.snippet, a.published_ms, a.read
                 FROM articles a JOIN feeds f ON f.id = a.feed_id
                 WHERE (?1 IS NULL OR a.feed_id = ?1) AND (?2 = 0 OR a.read = 0)
                 ORDER BY a.published_ms DESC, a.id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![feed, unread_only as i64, limit], |r| {
                Ok(ArticleSummary {
                    id: r.get(0)?,
                    feed_id: r.get(1)?,
                    feed_title: r.get(2)?,
                    title: r.get(3)?,
                    snippet: r.get(4)?,
                    published_ms: r.get(5)?,
                    read: r.get::<_, i64>(6)? != 0,
                })
            })?;
            rows.collect()
        })
        .await
    }

    /// Fetches an article, marking it read if `mark_read` (the reader does;
    /// the model reading it for an answer doesn't).
    pub async fn open_article(&self, id: i64, mark_read: bool) -> Result<Option<Article>, String> {
        self.run(move |c| {
            if mark_read {
                c.execute("UPDATE articles SET read = 1 WHERE id = ?1", params![id])?;
            }
            c.query_row(
                "SELECT a.id, a.feed_id, f.title, a.title, a.url, a.author, a.content_html, a.snippet, a.published_ms, a.read
                 FROM articles a JOIN feeds f ON f.id = a.feed_id WHERE a.id = ?1",
                params![id],
                |r| {
                    Ok(Article {
                        id: r.get(0)?,
                        feed_id: r.get(1)?,
                        feed_title: r.get(2)?,
                        title: r.get(3)?,
                        url: r.get(4)?,
                        author: r.get(5)?,
                        content_html: r.get(6)?,
                        snippet: r.get(7)?,
                        published_ms: r.get(8)?,
                        read: r.get::<_, i64>(9)? != 0,
                    })
                },
            )
            .optional()
        })
        .await
    }

    pub async fn set_article_read(&self, id: i64, read: bool) -> Result<(), String> {
        self.run(move |c| c.execute("UPDATE articles SET read = ?2 WHERE id = ?1", params![id, read as i64]).map(|_| ()))
            .await
    }

    pub async fn mark_all_read(&self, feed: Option<i64>) -> Result<(), String> {
        self.run(move |c| {
            c.execute("UPDATE articles SET read = 1 WHERE read = 0 AND (?1 IS NULL OR feed_id = ?1)", params![feed])
                .map(|_| ())
        })
        .await
    }

    /// Adds a feed, or returns the existing one with the same URL.
    pub async fn upsert_feed(&self, url: String, title: String, site_url: Option<String>) -> Result<i64, String> {
        self.run(move |c| {
            c.execute(
                "INSERT INTO feeds (url, title, site_url, added_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (url) DO UPDATE SET title = excluded.title, site_url = excluded.site_url",
                params![url, title, site_url, now_ms()],
            )?;
            c.query_row("SELECT id FROM feeds WHERE url = ?1", params![url], |r| r.get(0))
        })
        .await
    }

    pub async fn remove_feed(&self, id: i64) -> Result<(), String> {
        self.run(move |c| c.execute("DELETE FROM feeds WHERE id = ?1", params![id]).map(|_| ())).await
    }

    /// Stores a fetch result: new articles are added (existing ones are left
    /// as they are, keeping their read state), the feed's status is updated,
    /// and old articles beyond the per-feed limit are dropped. Returns how
    /// many articles were new.
    pub async fn store_fetch(
        &self,
        feed_id: i64,
        title: Option<String>,
        articles: Vec<NewArticle>,
        error: Option<String>,
    ) -> Result<usize, String> {
        self.run(move |c| {
            let tx = c.unchecked_transaction()?;
            let mut added = 0;
            {
                let mut insert = tx.prepare(
                    "INSERT OR IGNORE INTO articles
                        (feed_id, guid, url, title, snippet, content_html, author, published_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?;
                for a in &articles {
                    added += insert.execute(params![
                        feed_id, a.guid, a.url, a.title, a.snippet, a.content_html, a.author, a.published_ms
                    ])?;
                }
            }
            tx.execute(
                "UPDATE feeds SET fetched_ms = ?2, last_error = ?3, title = coalesce(nullif(?4, ''), title) WHERE id = ?1",
                params![feed_id, now_ms(), error, title],
            )?;
            tx.execute(
                "DELETE FROM articles WHERE feed_id = ?1 AND id NOT IN
                    (SELECT id FROM articles WHERE feed_id = ?1 ORDER BY published_ms DESC LIMIT ?2)",
                params![feed_id, MAX_ARTICLES_PER_FEED],
            )?;
            tx.commit()?;
            Ok(added)
        })
        .await
    }
}

impl Db {
    // ---------------- Calendar ----------------

    /// Events from `from` to `to` inclusive ("YYYY-MM-DD"), by date, all-day
    /// events first, then by start time.
    pub async fn events_between(&self, from: String, to: String) -> Result<Vec<Event>, String> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, title, date, start_time, end_time, location, notes FROM events
                 WHERE date BETWEEN ?1 AND ?2 ORDER BY date, start_time IS NOT NULL, start_time, id",
            )?;
            let rows = stmt.query_map(params![from, to], event_from_row)?;
            rows.collect()
        })
        .await
    }

    /// Inserts (no id) or updates an already validated event. Returns None
    /// if the event to update no longer exists.
    pub async fn save_event(&self, e: EventInput) -> Result<Option<Event>, String> {
        self.run(move |c| {
            let now = now_ms();
            let id = match e.id {
                Some(id) => {
                    let changed = c.execute(
                        "UPDATE events SET title = ?2, date = ?3, start_time = ?4, end_time = ?5,
                            location = ?6, notes = ?7, updated_ms = ?8 WHERE id = ?1",
                        params![id, e.title, e.date, e.start_time, e.end_time, e.location, e.notes, now],
                    )?;
                    if changed == 0 {
                        return Ok(None);
                    }
                    id
                }
                None => {
                    c.execute(
                        "INSERT INTO events (title, date, start_time, end_time, location, notes, created_ms, updated_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                        params![e.title, e.date, e.start_time, e.end_time, e.location, e.notes, now],
                    )?;
                    c.last_insert_rowid()
                }
            };
            c.query_row(
                "SELECT id, title, date, start_time, end_time, location, notes FROM events WHERE id = ?1",
                params![id],
                event_from_row,
            )
            .optional()
        })
        .await
    }

    pub async fn delete_event(&self, id: i64) -> Result<(), String> {
        self.run(move |c| c.execute("DELETE FROM events WHERE id = ?1", params![id]).map(|_| ())).await
    }
}

fn event_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: r.get(0)?,
        title: r.get(1)?,
        date: r.get(2)?,
        start_time: r.get(3)?,
        end_time: r.get(4)?,
        location: r.get(5)?,
        notes: r.get(6)?,
    })
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
