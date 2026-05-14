use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub path: String,
    pub discovered_at: SystemTime,
    pub last_seen_at: SystemTime,
    pub last_cleaned_at: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: i64,
    pub started_at: SystemTime,
    pub finished_at: Option<SystemTime>,
    pub projects_cleaned: i64,
    pub bytes_recovered: i64,
    pub errors_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanEvent {
    pub id: i64,
    pub run_id: i64,
    pub ts: SystemTime,
    pub path: String,
    pub bytes_before: i64,
    pub bytes_after: i64,
    pub duration_ms: i64,
    pub exit_code: i32,
    pub stderr_excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorRecord {
    pub id: i64,
    pub ts: SystemTime,
    pub category: String,
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectBytes {
    pub path: String,
    pub bytes: i64,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { conn })
    }

    pub fn ping(&self) -> Result<()> {
        self.conn.query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn migrate(&self) -> Result<()> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)",
            [],
        )?;
        let current: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )?;
        if current >= 1 {
            return Ok(());
        }
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS projects (
                path TEXT PRIMARY KEY,
                discovered_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                last_cleaned_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                projects_cleaned INTEGER NOT NULL DEFAULT 0,
                bytes_recovered INTEGER NOT NULL DEFAULT 0,
                errors_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS clean_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL REFERENCES runs(id),
                ts INTEGER NOT NULL,
                path TEXT NOT NULL,
                bytes_before INTEGER NOT NULL,
                bytes_after INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                exit_code INTEGER NOT NULL DEFAULT 0,
                stderr_excerpt TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS errors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                category TEXT NOT NULL,
                path TEXT,
                message TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_clean_events_ts ON clean_events(ts);
            CREATE INDEX IF NOT EXISTS idx_errors_ts ON errors(ts);
            CREATE INDEX IF NOT EXISTS idx_runs_started_at ON runs(started_at);
            INSERT INTO schema_version (version) VALUES (1);
            ",
        )?;
        Ok(())
    }

    pub fn table_exists(&self, table: &str) -> Result<bool> {
        let exists = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        Ok(exists)
    }

    pub fn upsert_project(&self, path: impl AsRef<Path>, now: SystemTime) -> Result<()> {
        let path = path_to_string(path.as_ref());
        let now = to_epoch(now)?;
        self.conn.execute(
            "
            INSERT INTO projects (path, discovered_at, last_seen_at)
            VALUES (?1, ?2, ?2)
            ON CONFLICT(path) DO UPDATE SET last_seen_at = excluded.last_seen_at
            ",
            params![path, now],
        )?;
        Ok(())
    }

    pub fn remove_project(&self, path: impl AsRef<Path>) -> Result<()> {
        self.conn.execute(
            "DELETE FROM projects WHERE path=?1",
            [path_to_string(path.as_ref())],
        )?;
        Ok(())
    }

    pub fn mark_project_cleaned(&self, path: impl AsRef<Path>, when: SystemTime) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET last_cleaned_at=?1 WHERE path=?2",
            params![to_epoch(when)?, path_to_string(path.as_ref())],
        )?;
        Ok(())
    }

    pub fn all_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, discovered_at, last_seen_at, last_cleaned_at FROM projects ORDER BY path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                path: row.get(0)?,
                discovered_at: from_epoch(row.get(1)?),
                last_seen_at: from_epoch(row.get(2)?),
                last_cleaned_at: row.get::<_, Option<i64>>(3)?.map(from_epoch),
            })
        })?;
        collect_rows(rows)
    }

    pub fn start_run(&self, started_at: SystemTime) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (started_at) VALUES (?1)",
            [to_epoch(started_at)?],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(
        &self,
        id: i64,
        finished_at: SystemTime,
        projects_cleaned: i64,
        bytes_recovered: i64,
        errors_count: i64,
    ) -> Result<()> {
        self.conn.execute(
            "
            UPDATE runs
            SET finished_at=?1, projects_cleaned=?2, bytes_recovered=?3, errors_count=?4
            WHERE id=?5
            ",
            params![
                to_epoch(finished_at)?,
                projects_cleaned,
                bytes_recovered,
                errors_count,
                id
            ],
        )?;
        Ok(())
    }

    pub fn last_run(&self) -> Result<Run> {
        self.conn
            .query_row(
                "
                SELECT id, started_at, finished_at, projects_cleaned, bytes_recovered, errors_count
                FROM runs ORDER BY started_at DESC, id DESC LIMIT 1
                ",
                [],
                run_from_row,
            )
            .context("no runs recorded")
    }

    pub fn record_clean_event(&self, event: &CleanEvent) -> Result<()> {
        self.conn.execute(
            "
            INSERT INTO clean_events
                (run_id, ts, path, bytes_before, bytes_after, duration_ms, exit_code, stderr_excerpt)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            params![
                event.run_id,
                to_epoch(event.ts)?,
                event.path,
                event.bytes_before,
                event.bytes_after,
                event.duration_ms,
                event.exit_code,
                event.stderr_excerpt
            ],
        )?;
        Ok(())
    }

    pub fn clean_events_since(&self, since: SystemTime) -> Result<Vec<CleanEvent>> {
        let mut stmt = self.conn.prepare(
            "
            SELECT id, run_id, ts, path, bytes_before, bytes_after, duration_ms, exit_code, stderr_excerpt
            FROM clean_events WHERE ts >= ?1 ORDER BY ts
            ",
        )?;
        let rows = stmt.query_map([to_epoch(since)?], |row| {
            Ok(CleanEvent {
                id: row.get(0)?,
                run_id: row.get(1)?,
                ts: from_epoch(row.get(2)?),
                path: row.get(3)?,
                bytes_before: row.get(4)?,
                bytes_after: row.get(5)?,
                duration_ms: row.get(6)?,
                exit_code: row.get(7)?,
                stderr_excerpt: row.get(8)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn record_error(&self, error: &ErrorRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO errors (ts, category, path, message) VALUES (?1, ?2, ?3, ?4)",
            params![
                to_epoch(error.ts)?,
                error.category,
                error.path,
                error.message
            ],
        )?;
        Ok(())
    }

    pub fn errors_since(&self, since: SystemTime) -> Result<Vec<ErrorRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, category, path, message FROM errors WHERE ts >= ?1 ORDER BY ts",
        )?;
        let rows = stmt.query_map([to_epoch(since)?], |row| {
            Ok(ErrorRecord {
                id: row.get(0)?,
                ts: from_epoch(row.get(1)?),
                category: row.get(2)?,
                path: row.get(3)?,
                message: row.get(4)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn scan_error_paths_since(&self, since: SystemTime) -> Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare(
            "
            SELECT path FROM errors
            WHERE ts >= ?1 AND category = 'scan' AND path IS NOT NULL
            ORDER BY path
            ",
        )?;
        let rows = stmt.query_map([to_epoch(since)?], |row| {
            let path: String = row.get(0)?;
            Ok(PathBuf::from(path))
        })?;
        collect_rows(rows)
    }

    pub fn total_bytes_recovered(&self, since: SystemTime) -> Result<i64> {
        let total = self.conn.query_row(
            "
            SELECT COALESCE(SUM(bytes_before - bytes_after), 0)
            FROM clean_events WHERE ts >= ?1
            ",
            [to_epoch(since)?],
            |row| row.get(0),
        )?;
        Ok(total)
    }

    pub fn top_projects_by_bytes(&self, since: SystemTime, n: usize) -> Result<Vec<ProjectBytes>> {
        let mut stmt = self.conn.prepare(
            "
            SELECT path, SUM(bytes_before - bytes_after) AS recovered
            FROM clean_events
            WHERE ts >= ?1
            GROUP BY path
            ORDER BY recovered DESC
            LIMIT ?2
            ",
        )?;
        let rows = stmt.query_map(params![to_epoch(since)?, n as i64], |row| {
            Ok(ProjectBytes {
                path: row.get(0)?,
                bytes: row.get(1)?,
            })
        })?;
        collect_rows(rows)
    }
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: row.get(0)?,
        started_at: from_epoch(row.get(1)?),
        finished_at: row.get::<_, Option<i64>>(2)?.map(from_epoch),
        projects_cleaned: row.get(3)?,
        bytes_recovered: row.get(4)?,
        errors_count: row.get(5)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn to_epoch(time: SystemTime) -> Result<i64> {
    Ok(time.duration_since(SystemTime::UNIX_EPOCH)?.as_secs() as i64)
}

fn from_epoch(secs: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64)
}

#[allow(dead_code)]
fn _normalize(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().to_path_buf()
}
