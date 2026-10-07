//! SQLite storage. Records are written in transactions together with the active state,
//! with WAL journaling and `synchronous=FULL` so a committed record survives power loss.

use std::path::Path;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::*;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0}")]
    Invalid(String),
}

const SCHEMA_VERSION: i32 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tags (
    id        TEXT PRIMARY KEY,
    name      TEXT NOT NULL,
    color     TEXT NOT NULL,
    archived  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS sessions (
    id                TEXT PRIMARY KEY,
    tag_id            TEXT REFERENCES tags(id),
    mode              TEXT NOT NULL,
    target_pomodoros  INTEGER NOT NULL,
    started_at        TEXT NOT NULL,
    ended_at          TEXT,
    status            TEXT NOT NULL,
    local_date        TEXT,
    goal_sessions     INTEGER
);
CREATE INDEX IF NOT EXISTS sessions_local_date ON sessions(local_date);
CREATE TABLE IF NOT EXISTS pomodoros (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL REFERENCES sessions(id),
    started_at   TEXT NOT NULL,
    ended_at     TEXT NOT NULL,
    planned_sec  INTEGER NOT NULL,
    elapsed_sec  REAL NOT NULL,
    status       TEXT NOT NULL,
    local_date   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS pomodoros_session ON pomodoros(session_id);
CREATE INDEX IF NOT EXISTS pomodoros_local_date ON pomodoros(local_date);
CREATE TABLE IF NOT EXISTS goal_history (
    effective_from         TEXT PRIMARY KEY,
    goal_sessions          INTEGER NOT NULL,
    pomodoros_per_session  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS active_state (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    session_id           TEXT,
    pomodoro_id          TEXT,
    pomodoro_started_at  TEXT,
    phase                TEXT,
    run_state            TEXT NOT NULL,
    phase_planned_sec    INTEGER NOT NULL,
    phase_elapsed_sec    REAL NOT NULL,
    checkpointed_at      TEXT,
    clean_exit           INTEGER NOT NULL
);
INSERT OR IGNORE INTO active_state VALUES (1, NULL, NULL, NULL, NULL, 'waiting', 0, 0, NULL, 1);
"#;

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Invalid(e.to_string()))?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db> {
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch(
            "PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;
             PRAGMA wal_autocheckpoint = 1000;",
        )?;
        // Reading user_version also forces SQLite to validate the file header.
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error::Invalid(format!(
                "database schema version {version} is newer than this app supports"
            )));
        }
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        Ok(Db { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Runs `f` in one transaction: all of its writes land, or none do.
    pub fn tx<T>(&mut self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ---- tags --------------------------------------------------------------

    pub fn tags(&self) -> Result<Vec<Tag>> {
        tags(&self.conn)
    }

    pub fn tag(&self, id: &str) -> Result<Option<Tag>> {
        Ok(self
            .conn
            .query_row("SELECT id, name, color, archived FROM tags WHERE id = ?", [id], tag_from_row)
            .optional()?)
    }

    /// Creates a tag after validating the name (1–32 chars, unique ignoring case).
    pub fn create_tag(&mut self, name: &str, color: TagColor) -> Result<Tag> {
        let name = validate_tag_name(name)?;
        self.ensure_name_free(&name, None)?;
        let tag = Tag { id: new_id(), name, color, archived: false };
        insert_tag(&self.conn, &tag)?;
        Ok(tag)
    }

    pub fn rename_tag(&mut self, id: &str, name: &str) -> Result<()> {
        let name = validate_tag_name(name)?;
        self.ensure_name_free(&name, Some(id))?;
        self.conn.execute("UPDATE tags SET name = ? WHERE id = ?", params![name, id])?;
        Ok(())
    }

    pub fn recolor_tag(&mut self, id: &str, color: TagColor) -> Result<()> {
        self.conn.execute("UPDATE tags SET color = ? WHERE id = ?", params![color.as_str(), id])?;
        Ok(())
    }

    pub fn set_tag_archived(&mut self, id: &str, archived: bool) -> Result<()> {
        self.conn.execute("UPDATE tags SET archived = ? WHERE id = ?", params![archived, id])?;
        Ok(())
    }

    fn ensure_name_free(&self, name: &str, except: Option<&str>) -> Result<()> {
        let lower = name.to_lowercase();
        let clash = self
            .tags()?
            .into_iter()
            .any(|t| Some(t.id.as_str()) != except && t.name.to_lowercase() == lower);
        if clash {
            return Err(Error::Invalid(format!("A tag named “{name}” already exists")));
        }
        Ok(())
    }

    // ---- sessions & pomodoros ---------------------------------------------

    pub fn sessions(&self) -> Result<Vec<Session>> {
        let mut st = self.conn.prepare(&format!("SELECT {SESSION_COLS} FROM sessions ORDER BY started_at"))?;
        let rows = st.query_map([], session_from_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        session(&self.conn, id)
    }

    pub fn pomodoros(&self) -> Result<Vec<Pomodoro>> {
        let mut st = self.conn.prepare(&format!("SELECT {POMODORO_COLS} FROM pomodoros ORDER BY ended_at"))?;
        let rows = st.query_map([], pomodoro_from_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn session_pomodoros(&self, session_id: &str) -> Result<Vec<Pomodoro>> {
        session_pomodoros(&self.conn, session_id)
    }

    pub fn set_session_tag(&mut self, session_id: &str, tag_id: Option<&str>) -> Result<()> {
        self.conn.execute("UPDATE sessions SET tag_id = ? WHERE id = ?", params![tag_id, session_id])?;
        Ok(())
    }

    pub fn completed_sessions_on(&self, date: NaiveDate) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE status = 'completed' AND local_date = ?",
            [date.to_string()],
            |r| r.get(0),
        )?)
    }

    pub fn completed_pomodoros_on(&self, date: NaiveDate) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM pomodoros WHERE status = 'completed' AND local_date = ?",
            [date.to_string()],
            |r| r.get(0),
        )?)
    }

    // ---- goals -------------------------------------------------------------

    pub fn goal_history(&self) -> Result<Vec<GoalEntry>> {
        goal_history(&self.conn)
    }

    /// Records the goal in force from `from` onward (replacing an entry for that same day).
    pub fn set_goal(&mut self, entry: &GoalEntry) -> Result<()> {
        upsert_goal(&self.conn, entry)
    }

    // ---- active state ------------------------------------------------------

    pub fn active_state(&self) -> Result<ActiveState> {
        Ok(self.conn.query_row(
            "SELECT session_id, pomodoro_id, pomodoro_started_at, phase, run_state,
                    phase_planned_sec, phase_elapsed_sec, checkpointed_at, clean_exit
             FROM active_state WHERE id = 1",
            [],
            |r| {
                Ok(ActiveState {
                    session_id: r.get(0)?,
                    pomodoro_id: r.get(1)?,
                    pomodoro_started_at: r.get(2)?,
                    phase: r.get::<_, Option<String>>(3)?.and_then(|s| PhaseKind::parse(&s)),
                    run_state: RunState::parse(&r.get::<_, String>(4)?).unwrap_or(RunState::Waiting),
                    phase_planned_sec: r.get(5)?,
                    phase_elapsed_sec: r.get(6)?,
                    checkpointed_at: r.get(7)?,
                    clean_exit: r.get(8)?,
                })
            },
        )?)
    }

    pub fn save_active_state(&self, s: &ActiveState) -> Result<()> {
        save_active_state(&self.conn, s)
    }

    pub fn set_clean_exit(&self, clean: bool) -> Result<()> {
        self.conn.execute("UPDATE active_state SET clean_exit = ? WHERE id = 1", [clean])?;
        Ok(())
    }
}

// ---- free functions usable inside a transaction ----------------------------

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn validate_tag_name(name: &str) -> Result<String> {
    let name = name.trim();
    let len = name.chars().count();
    if !(1..=32).contains(&len) {
        return Err(Error::Invalid("Tag names must be 1 to 32 characters long".into()));
    }
    Ok(name.to_owned())
}

const SESSION_COLS: &str =
    "id, tag_id, mode, target_pomodoros, started_at, ended_at, status, local_date, goal_sessions";
const POMODORO_COLS: &str =
    "id, session_id, started_at, ended_at, planned_sec, elapsed_sec, status, local_date";

fn bad_value(col: usize, v: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        col,
        rusqlite::types::Type::Text,
        format!("unexpected value {v:?}").into(),
    )
}

fn parse_col<T>(r: &Row, col: usize, f: impl Fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    let s: String = r.get(col)?;
    f(&s).ok_or_else(|| bad_value(col, &s))
}

fn date_col(r: &Row, col: usize) -> rusqlite::Result<Option<NaiveDate>> {
    match r.get::<_, Option<String>>(col)? {
        None => Ok(None),
        Some(s) => s.parse().map(Some).map_err(|_| bad_value(col, &s)),
    }
}

fn tag_from_row(r: &Row) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: r.get(0)?,
        name: r.get(1)?,
        color: parse_col(r, 2, TagColor::parse)?,
        archived: r.get(3)?,
    })
}

fn session_from_row(r: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        tag_id: r.get(1)?,
        mode: parse_col(r, 2, Mode::parse)?,
        target_pomodoros: r.get(3)?,
        started_at: r.get(4)?,
        ended_at: r.get(5)?,
        status: parse_col(r, 6, SessionStatus::parse)?,
        local_date: date_col(r, 7)?,
        goal_sessions: r.get(8)?,
    })
}

fn pomodoro_from_row(r: &Row) -> rusqlite::Result<Pomodoro> {
    Ok(Pomodoro {
        id: r.get(0)?,
        session_id: r.get(1)?,
        started_at: r.get(2)?,
        ended_at: r.get(3)?,
        planned_sec: r.get(4)?,
        elapsed_sec: r.get(5)?,
        status: parse_col(r, 6, PomodoroStatus::parse)?,
        local_date: date_col(r, 7)?.ok_or_else(|| bad_value(7, "NULL"))?,
    })
}

pub fn tags(conn: &Connection) -> Result<Vec<Tag>> {
    let mut st = conn.prepare("SELECT id, name, color, archived FROM tags ORDER BY name COLLATE NOCASE")?;
    let rows = st.query_map([], tag_from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn insert_tag(conn: &Connection, t: &Tag) -> Result<()> {
    conn.execute(
        "INSERT INTO tags (id, name, color, archived) VALUES (?, ?, ?, ?)",
        params![t.id, t.name, t.color.as_str(), t.archived],
    )?;
    Ok(())
}

pub fn session(conn: &Connection, id: &str) -> Result<Option<Session>> {
    Ok(conn
        .query_row(&format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?"), [id], session_from_row)
        .optional()?)
}

pub fn insert_session(conn: &Connection, s: &Session) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO sessions ({SESSION_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"),
        params![
            s.id,
            s.tag_id,
            s.mode.as_str(),
            s.target_pomodoros,
            s.started_at,
            s.ended_at,
            s.status.as_str(),
            s.local_date.map(|d| d.to_string()),
            s.goal_sessions,
        ],
    )?;
    Ok(())
}

pub fn finish_session(
    conn: &Connection,
    id: &str,
    status: SessionStatus,
    ended_at: &str,
    local_date: NaiveDate,
    goal_sessions: u32,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET status = ?, ended_at = ?, local_date = ?, goal_sessions = ? WHERE id = ?",
        params![status.as_str(), ended_at, local_date.to_string(), goal_sessions, id],
    )?;
    Ok(())
}

pub fn delete_session(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE id = ?", [id])?;
    Ok(())
}

pub fn session_pomodoros(conn: &Connection, session_id: &str) -> Result<Vec<Pomodoro>> {
    let mut st = conn.prepare(&format!(
        "SELECT {POMODORO_COLS} FROM pomodoros WHERE session_id = ? ORDER BY ended_at"
    ))?;
    let rows = st.query_map([session_id], pomodoro_from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn insert_pomodoro(conn: &Connection, p: &Pomodoro) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO pomodoros ({POMODORO_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"),
        params![
            p.id,
            p.session_id,
            p.started_at,
            p.ended_at,
            p.planned_sec,
            p.elapsed_sec,
            p.status.as_str(),
            p.local_date.to_string(),
        ],
    )?;
    Ok(())
}

pub fn goal_history(conn: &Connection) -> Result<Vec<GoalEntry>> {
    let mut st = conn.prepare(
        "SELECT effective_from, goal_sessions, pomodoros_per_session FROM goal_history ORDER BY effective_from",
    )?;
    let rows = st.query_map([], |r| {
        Ok(GoalEntry {
            effective_from: date_col(r, 0)?.ok_or_else(|| bad_value(0, "NULL"))?,
            goal_sessions: r.get(1)?,
            pomodoros_per_session: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn upsert_goal(conn: &Connection, g: &GoalEntry) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO goal_history (effective_from, goal_sessions, pomodoros_per_session)
         VALUES (?, ?, ?)",
        params![g.effective_from.to_string(), g.goal_sessions, g.pomodoros_per_session],
    )?;
    Ok(())
}

pub fn save_active_state(conn: &Connection, s: &ActiveState) -> Result<()> {
    conn.execute(
        "UPDATE active_state SET session_id = ?, pomodoro_id = ?, pomodoro_started_at = ?, phase = ?,
             run_state = ?, phase_planned_sec = ?, phase_elapsed_sec = ?, checkpointed_at = ?,
             clean_exit = ?
         WHERE id = 1",
        params![
            s.session_id,
            s.pomodoro_id,
            s.pomodoro_started_at,
            s.phase.map(PhaseKind::as_str),
            s.run_state.as_str(),
            s.phase_planned_sec,
            s.phase_elapsed_sec,
            s.checkpointed_at,
            s.clean_exit,
        ],
    )?;
    Ok(())
}

/// The goal in force on `date`: the latest entry starting on or before it, else the earliest one.
pub fn goal_for(history: &[GoalEntry], date: NaiveDate) -> Option<&GoalEntry> {
    history
        .iter()
        .rev()
        .find(|g| g.effective_from <= date)
        .or_else(|| history.first())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_names_unique_ignoring_case() {
        let mut db = Db::open_in_memory().unwrap();
        db.create_tag("Chemistry", TagColor::Blue).unwrap();
        assert!(db.create_tag("chemistry", TagColor::Red).is_err());
        assert!(db.create_tag("", TagColor::Red).is_err());
        assert!(db.create_tag(&"x".repeat(33), TagColor::Red).is_err());
        let m = db.create_tag("Mathematics", TagColor::Green).unwrap();
        assert!(db.rename_tag(&m.id, "CHEMISTRY").is_err());
        db.rename_tag(&m.id, "Maths").unwrap();
        db.set_tag_archived(&m.id, true).unwrap();
        assert!(db.tag(&m.id).unwrap().unwrap().archived);
    }

    #[test]
    fn fresh_database_is_idle_with_clean_exit() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.active_state().unwrap(), ActiveState::default());
    }
}
