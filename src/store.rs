use crate::safety::ReviewSummary;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use std::collections::BTreeSet;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewStatus {
    pub reviewed_at: SystemTime,
    pub source: String,
    pub summary: ReviewSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerStatus {
    pub updated_at: SystemTime,
    pub next_clean_at: SystemTime,
    pub next_scan_at: SystemTime,
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
        if current < 1 {
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
        }
        if current < 2 {
            self.conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS review_status (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    reviewed_at INTEGER NOT NULL,
                    source TEXT NOT NULL,
                    total_projects INTEGER NOT NULL,
                    cleanable_projects INTEGER NOT NULL,
                    skipped_projects INTEGER NOT NULL,
                    cleanable_bytes INTEGER NOT NULL,
                    active_recent_write INTEGER NOT NULL,
                    active_process INTEGER NOT NULL,
                    managed_cache INTEGER NOT NULL,
                    container_storage INTEGER NOT NULL,
                    scan_error INTEGER NOT NULL,
                    no_target INTEGER NOT NULL,
                    target_read_error INTEGER NOT NULL
                );
                INSERT INTO schema_version (version) VALUES (2);
                ",
            )?;
        }
        if current < 3 {
            self.conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS scheduler_state (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    updated_at INTEGER NOT NULL,
                    next_clean_at INTEGER NOT NULL,
                    next_scan_at INTEGER NOT NULL
                );
                INSERT INTO schema_version (version) VALUES (3);
                ",
            )?;
        }
        if current < 4 {
            self.conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS linked_worktrees (
                    primary_path TEXT NOT NULL,
                    linked_path TEXT NOT NULL,
                    PRIMARY KEY (primary_path, linked_path)
                );
                CREATE INDEX IF NOT EXISTS idx_linked_worktrees_linked
                    ON linked_worktrees(linked_path);
                CREATE TABLE IF NOT EXISTS worktree_discovery_failures (
                    primary_path TEXT PRIMARY KEY,
                    failed_at INTEGER NOT NULL,
                    message TEXT NOT NULL
                );
                INSERT INTO schema_version (version) VALUES (4);
                ",
            )?;
        }
        if current < 5 {
            let tx = self.conn.unchecked_transaction()?;
            let has_canonical_primary_path = {
                let mut stmt = tx.prepare("PRAGMA table_info(worktree_discovery_failures)")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
                collect_rows(rows)?
                    .into_iter()
                    .any(|column| column == "canonical_primary_path")
            };
            if !has_canonical_primary_path {
                tx.execute(
                    "
                    ALTER TABLE worktree_discovery_failures
                    ADD COLUMN canonical_primary_path TEXT
                    ",
                    [],
                )?;
            }
            let legacy_primaries = {
                let mut stmt = tx.prepare(
                    "
                    SELECT primary_path
                    FROM worktree_discovery_failures
                    WHERE canonical_primary_path IS NULL
                    ORDER BY primary_path
                    ",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                collect_rows(rows)?
            };
            for primary in legacy_primaries {
                let Ok(canonical) = fs::canonicalize(Path::new(&primary)) else {
                    continue;
                };
                if path_to_string(&canonical)? == primary {
                    tx.execute(
                        "
                        UPDATE worktree_discovery_failures
                        SET canonical_primary_path=?1
                        WHERE primary_path=?1 AND canonical_primary_path IS NULL
                        ",
                        [&primary],
                    )?;
                }
            }
            tx.execute("INSERT INTO schema_version (version) VALUES (5)", [])?;
            tx.commit()?;
        }
        if current < 6 {
            let tx = self.conn.unchecked_transaction()?;
            let has_canonical_primary_path = {
                let mut stmt = tx.prepare("PRAGMA table_info(linked_worktrees)")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
                collect_rows(rows)?
                    .into_iter()
                    .any(|column| column == "canonical_primary_path")
            };
            if !has_canonical_primary_path {
                tx.execute(
                    "
                    ALTER TABLE linked_worktrees
                    ADD COLUMN canonical_primary_path TEXT
                    ",
                    [],
                )?;
            }
            let legacy_primaries = {
                let mut stmt = tx.prepare(
                    "
                    SELECT DISTINCT primary_path
                    FROM linked_worktrees
                    WHERE canonical_primary_path IS NULL
                    ORDER BY primary_path
                    ",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                collect_rows(rows)?
            };
            for primary in legacy_primaries {
                let Ok(canonical) = fs::canonicalize(Path::new(&primary)) else {
                    continue;
                };
                if path_to_string(&canonical)? == primary {
                    tx.execute(
                        "
                        UPDATE linked_worktrees
                        SET canonical_primary_path=?1
                        WHERE primary_path=?1 AND canonical_primary_path IS NULL
                        ",
                        [&primary],
                    )?;
                }
            }
            tx.execute("INSERT INTO schema_version (version) VALUES (6)", [])?;
            tx.commit()?;
        }
        if current < 7 {
            let tx = self.conn.unchecked_transaction()?;
            let has_errors = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type='table' AND name='errors'
                )",
                [],
                |row| row.get::<_, bool>(0),
            )?;
            if has_errors {
                tx.execute(
                    "
                    UPDATE errors
                    SET category='worktree_discovery'
                    WHERE category='scan'
                      AND path IS NOT NULL
                      AND EXISTS(
                          SELECT 1
                          FROM worktree_discovery_failures
                          WHERE (primary_path=errors.path
                                 OR canonical_primary_path=errors.path)
                            AND failed_at=errors.ts
                            AND message=errors.message
                      )
                    ",
                    [],
                )?;
            }
            tx.execute("INSERT INTO schema_version (version) VALUES (7)", [])?;
            tx.commit()?;
        }
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
        let path = path_to_string(path.as_ref())?;
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
        let path = path_to_string(path.as_ref())?;
        self.conn
            .execute("DELETE FROM projects WHERE path=?1", [&path])?;
        Ok(())
    }

    pub fn replace_project_path(&self, old_path: &Path, new_path: &Path) -> Result<()> {
        let old_path = path_to_string(old_path)?;
        let new_path = path_to_string(new_path)?;
        if old_path == new_path {
            return Ok(());
        }

        let tx = self.conn.unchecked_transaction()?;
        replace_project_path_in_transaction(&tx, &old_path, &new_path)?;
        tx.commit()?;
        Ok(())
    }

    pub fn replace_cached_project_path(&self, old_path: &Path, new_path: &Path) -> Result<()> {
        let old_path = path_to_string(old_path)?;
        let new_path = path_to_string(new_path)?;
        if old_path == new_path {
            return Ok(());
        }

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "
            INSERT INTO projects (path, discovered_at, last_seen_at, last_cleaned_at)
            SELECT ?2, discovered_at, last_seen_at, last_cleaned_at
            FROM projects
            WHERE path=?1
            ON CONFLICT(path) DO UPDATE SET
                discovered_at = MIN(projects.discovered_at, excluded.discovered_at),
                last_seen_at = MAX(projects.last_seen_at, excluded.last_seen_at),
                last_cleaned_at = CASE
                    WHEN projects.last_cleaned_at IS NULL THEN excluded.last_cleaned_at
                    WHEN excluded.last_cleaned_at IS NULL THEN projects.last_cleaned_at
                    ELSE MAX(projects.last_cleaned_at, excluded.last_cleaned_at)
                END
            ",
            params![old_path, new_path],
        )?;
        tx.execute("DELETE FROM projects WHERE path=?1", [&old_path])?;
        tx.commit()?;
        Ok(())
    }

    pub fn normalize_resolvable_project_aliases(&self) -> Result<()> {
        let frozen_identities = self.frozen_worktree_identities()?;
        let mut stmt = self.conn.prepare(
            "
            SELECT path FROM projects
            UNION
            SELECT primary_path FROM linked_worktrees
            UNION
            SELECT linked_path FROM linked_worktrees
            UNION
            SELECT primary_path FROM worktree_discovery_failures
            ORDER BY 1
            ",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let identities = collect_rows(rows)?;
        drop(stmt);

        let mut replacements = Vec::new();
        for identity in identities {
            if frozen_identities.contains(&identity) {
                continue;
            }
            let Ok(canonical) = fs::canonicalize(Path::new(&identity)) else {
                continue;
            };
            let canonical = path_to_string(&canonical)?;
            if identity != canonical {
                replacements.push((identity, canonical));
            }
        }
        if replacements.is_empty() {
            return Ok(());
        }

        let tx = self.conn.unchecked_transaction()?;
        for (alias, canonical) in replacements {
            replace_project_path_in_transaction(&tx, &alias, &canonical)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn is_active_worktree_discovery_identity(&self, path: &Path) -> Result<bool> {
        let path = path_to_string(path)?;
        Ok(self.frozen_worktree_identities()?.contains(&path))
    }

    pub fn replace_linked_worktrees(&self, primary: &Path, linked: &[PathBuf]) -> Result<()> {
        self.replace_linked_worktrees_with_exclusions(primary, linked, &[])
    }

    pub fn replace_linked_worktrees_with_exclusions(
        &self,
        primary: &Path,
        linked: &[PathBuf],
        excluded: &[PathBuf],
    ) -> Result<()> {
        self.replace_linked_worktrees_with_reconciliation(primary, linked, excluded, &[])
    }

    pub fn replace_linked_worktrees_with_reconciliation(
        &self,
        primary: &Path,
        linked: &[PathBuf],
        excluded: &[PathBuf],
        out_of_scope: &[PathBuf],
    ) -> Result<()> {
        let persisted_primary = path_to_string(primary)?;
        let canonical_primary = fs::canonicalize(primary)
            .ok()
            .map(|canonical| path_to_string(&canonical))
            .transpose()?;
        let primary = canonical_primary
            .as_deref()
            .unwrap_or(&persisted_primary)
            .to_owned();
        let linked: BTreeSet<_> = linked
            .iter()
            .map(|path| path_to_string(path))
            .collect::<Result<_>>()?;
        let excluded: BTreeSet<_> = excluded
            .iter()
            .map(|path| path_to_string(path))
            .collect::<Result<_>>()?;
        let out_of_scope: BTreeSet<_> = out_of_scope
            .iter()
            .map(|path| path_to_string(path))
            .collect::<Result<_>>()?;
        let tx = self.conn.unchecked_transaction()?;
        if canonical_primary.is_some() {
            normalize_failed_primary_aliases_for_success(&tx, &primary)?;
            rekey_trusted_linked_associations_for_success(&tx, &primary)?;
        }
        let has_untrusted_failure = tx.query_row(
            "
            SELECT EXISTS(
                SELECT 1
                FROM worktree_discovery_failures
                WHERE canonical_primary_path IS NULL
            )
            ",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if has_untrusted_failure {
            tx.commit()?;
            return Ok(());
        }
        let previous_linked = {
            let mut stmt = tx.prepare(
                "
                SELECT linked_path
                FROM linked_worktrees
                WHERE canonical_primary_path=?1
                ",
            )?;
            let rows = stmt.query_map([&primary], |row| row.get::<_, String>(0))?;
            collect_rows(rows)?.into_iter().collect::<BTreeSet<_>>()
        };
        tx.execute(
            "
            DELETE FROM linked_worktrees
            WHERE canonical_primary_path=?1
            ",
            [&primary],
        )?;
        for linked_path in &linked {
            tx.execute(
                "
                INSERT OR IGNORE INTO linked_worktrees (
                    primary_path,
                    linked_path,
                    canonical_primary_path
                )
                VALUES (?1, ?2, ?3)
                ",
                params![primary, linked_path, canonical_primary.as_deref()],
            )?;
        }
        for stale_path in previous_linked.difference(&linked) {
            tx.execute("DELETE FROM projects WHERE path=?1", [stale_path])?;
        }
        for excluded_path in excluded {
            tx.execute("DELETE FROM projects WHERE path=?1", [excluded_path])?;
        }
        for out_of_scope_path in out_of_scope {
            tx.execute("DELETE FROM projects WHERE path=?1", [out_of_scope_path])?;
        }
        tx.execute(
            "
            DELETE FROM worktree_discovery_failures
            WHERE primary_path=?1 AND canonical_primary_path=?1
            ",
            [&primary],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn mark_worktree_discovery_failed(
        &self,
        primary: &Path,
        now: SystemTime,
        message: &str,
    ) -> Result<()> {
        let primary_path = path_to_string(primary)?;
        let canonical_primary_path = fs::canonicalize(primary)
            .ok()
            .map(|canonical| path_to_string(&canonical))
            .transpose()?;
        self.conn.execute(
            "
            INSERT INTO worktree_discovery_failures (
                primary_path,
                failed_at,
                message,
                canonical_primary_path
            )
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(primary_path) DO UPDATE SET
                failed_at = excluded.failed_at,
                message = excluded.message
            ",
            params![
                primary_path,
                to_epoch(now)?,
                message,
                canonical_primary_path
            ],
        )?;
        Ok(())
    }

    pub fn blocked_worktree_discovery_paths(&self) -> Result<Vec<PathBuf>> {
        let active_identities = self.active_worktree_discovery_identities()?;
        let has_untrusted_failure = self.conn.query_row(
            "
            SELECT EXISTS(
                SELECT 1
                FROM worktree_discovery_failures
                WHERE canonical_primary_path IS NULL
            )
            ",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        let has_untrusted_association_during_failure = self.conn.query_row(
            "
            SELECT
                EXISTS(SELECT 1 FROM worktree_discovery_failures)
                AND EXISTS(
                    SELECT 1
                    FROM linked_worktrees
                    WHERE canonical_primary_path IS NULL
                )
            ",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        let blocked = self
            .trusted_worktree_discovery_paths()?
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();

        if !has_untrusted_failure
            && !has_untrusted_association_during_failure
            && blocked.iter().all(|path| {
                fs::canonicalize(path).is_ok_and(|canonical| canonical == path.as_path())
            })
        {
            return Ok(blocked);
        }

        let mut fail_closed = active_identities
            .into_iter()
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>();
        fail_closed.extend(blocked);
        fail_closed.extend(
            self.all_projects()?
                .into_iter()
                .map(|project| PathBuf::from(project.path)),
        );
        Ok(fail_closed.into_iter().collect())
    }

    fn trusted_worktree_discovery_paths(&self) -> Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare(
            "
            SELECT blocked_path
            FROM (
                SELECT canonical_primary_path AS blocked_path
                FROM worktree_discovery_failures
                WHERE canonical_primary_path IS NOT NULL
                UNION
                SELECT linked_path AS blocked_path
                FROM linked_worktrees
                INNER JOIN worktree_discovery_failures
                    ON linked_worktrees.canonical_primary_path
                        = worktree_discovery_failures.canonical_primary_path
                WHERE linked_worktrees.canonical_primary_path IS NOT NULL
                  AND worktree_discovery_failures.canonical_primary_path IS NOT NULL
            )
            ORDER BY blocked_path
            ",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(collect_rows(rows)?.into_iter().collect())
    }

    fn active_worktree_discovery_identities(&self) -> Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare(
            "
            SELECT blocked_path
            FROM (
                SELECT primary_path AS blocked_path
                FROM worktree_discovery_failures
                UNION
                SELECT linked_path AS blocked_path
                FROM linked_worktrees
                INNER JOIN worktree_discovery_failures
                    ON linked_worktrees.canonical_primary_path
                        = worktree_discovery_failures.canonical_primary_path
                WHERE linked_worktrees.canonical_primary_path IS NOT NULL
                  AND worktree_discovery_failures.canonical_primary_path IS NOT NULL
                UNION
                SELECT primary_path AS blocked_path
                FROM linked_worktrees
                WHERE canonical_primary_path IS NULL
                  AND EXISTS(SELECT 1 FROM worktree_discovery_failures)
                UNION
                SELECT linked_path AS blocked_path
                FROM linked_worktrees
                WHERE canonical_primary_path IS NULL
                  AND EXISTS(SELECT 1 FROM worktree_discovery_failures)
            )
            ORDER BY blocked_path
            ",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(collect_rows(rows)?.into_iter().collect())
    }

    fn frozen_worktree_identities(&self) -> Result<BTreeSet<String>> {
        let mut frozen = self.active_worktree_discovery_identities()?;
        let mut stmt = self.conn.prepare(
            "
            SELECT identity
            FROM (
                SELECT primary_path AS identity
                FROM linked_worktrees
                UNION
                SELECT linked_path AS identity
                FROM linked_worktrees
            )
            ORDER BY identity
            ",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        frozen.extend(collect_rows(rows)?);
        Ok(frozen)
    }

    pub fn mark_project_cleaned(&self, path: impl AsRef<Path>, when: SystemTime) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET last_cleaned_at=?1 WHERE path=?2",
            params![to_epoch(when)?, path_to_string(path.as_ref())?],
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

    pub fn project_count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
        Ok(count.max(0) as usize)
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
            SELECT DISTINCT path
            FROM errors
            WHERE ts >= ?1
              AND path IS NOT NULL
              AND (
                  category = 'scan'
                  OR (
                      category = 'worktree_discovery'
                      AND EXISTS(
                          SELECT 1
                          FROM worktree_discovery_failures
                          WHERE primary_path = errors.path
                             OR canonical_primary_path = errors.path
                      )
                  )
              )
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

    pub fn record_review_status(
        &self,
        reviewed_at: SystemTime,
        source: &str,
        summary: &ReviewSummary,
    ) -> Result<()> {
        self.conn.execute(
            "
            INSERT INTO review_status (
                id, reviewed_at, source, total_projects, cleanable_projects, skipped_projects,
                cleanable_bytes, active_recent_write, active_process, managed_cache,
                container_storage, scan_error, no_target, target_read_error
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(id) DO UPDATE SET
                reviewed_at = excluded.reviewed_at,
                source = excluded.source,
                total_projects = excluded.total_projects,
                cleanable_projects = excluded.cleanable_projects,
                skipped_projects = excluded.skipped_projects,
                cleanable_bytes = excluded.cleanable_bytes,
                active_recent_write = excluded.active_recent_write,
                active_process = excluded.active_process,
                managed_cache = excluded.managed_cache,
                container_storage = excluded.container_storage,
                scan_error = excluded.scan_error,
                no_target = excluded.no_target,
                target_read_error = excluded.target_read_error
            ",
            params![
                to_epoch(reviewed_at)?,
                source,
                summary.total_projects as i64,
                summary.cleanable_projects as i64,
                summary.skipped_projects as i64,
                i64::try_from(summary.cleanable_bytes).unwrap_or(i64::MAX),
                summary.active_recent_write as i64,
                summary.active_process as i64,
                summary.managed_cache as i64,
                summary.container_storage as i64,
                summary.scan_error as i64,
                summary.no_target as i64,
                summary.target_read_error as i64,
            ],
        )?;
        Ok(())
    }

    pub fn last_review_status(&self) -> Result<Option<ReviewStatus>> {
        self.conn
            .query_row(
                "
                SELECT reviewed_at, source, total_projects, cleanable_projects, skipped_projects,
                    cleanable_bytes, active_recent_write, active_process, managed_cache,
                    container_storage, scan_error, no_target, target_read_error
                FROM review_status
                WHERE id = 1
                ",
                [],
                |row| {
                    Ok(ReviewStatus {
                        reviewed_at: from_epoch(row.get(0)?),
                        source: row.get(1)?,
                        summary: ReviewSummary {
                            total_projects: to_usize(row.get(2)?),
                            cleanable_projects: to_usize(row.get(3)?),
                            skipped_projects: to_usize(row.get(4)?),
                            cleanable_bytes: to_u64(row.get(5)?),
                            active_recent_write: to_usize(row.get(6)?),
                            active_process: to_usize(row.get(7)?),
                            managed_cache: to_usize(row.get(8)?),
                            container_storage: to_usize(row.get(9)?),
                            scan_error: to_usize(row.get(10)?),
                            no_target: to_usize(row.get(11)?),
                            target_read_error: to_usize(row.get(12)?),
                        },
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn record_scheduler_status(
        &self,
        updated_at: SystemTime,
        next_clean_at: SystemTime,
        next_scan_at: SystemTime,
    ) -> Result<()> {
        self.conn.execute(
            "
            INSERT INTO scheduler_state (id, updated_at, next_clean_at, next_scan_at)
            VALUES (1, ?1, ?2, ?3)
            ON CONFLICT(id) DO UPDATE SET
                updated_at = excluded.updated_at,
                next_clean_at = excluded.next_clean_at,
                next_scan_at = excluded.next_scan_at
            ",
            params![
                to_epoch(updated_at)?,
                to_epoch(next_clean_at)?,
                to_epoch(next_scan_at)?,
            ],
        )?;
        Ok(())
    }

    pub fn scheduler_status(&self) -> Result<Option<SchedulerStatus>> {
        self.conn
            .query_row(
                "
                SELECT updated_at, next_clean_at, next_scan_at
                FROM scheduler_state
                WHERE id = 1
                ",
                [],
                |row| {
                    Ok(SchedulerStatus {
                        updated_at: from_epoch(row.get(0)?),
                        next_clean_at: from_epoch(row.get(1)?),
                        next_scan_at: from_epoch(row.get(2)?),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

fn replace_project_path_in_transaction(
    tx: &Transaction<'_>,
    old_path: &str,
    new_path: &str,
) -> Result<()> {
    tx.execute(
        "
        INSERT INTO projects (path, discovered_at, last_seen_at, last_cleaned_at)
        SELECT ?2, discovered_at, last_seen_at, last_cleaned_at
        FROM projects
        WHERE path=?1
        ON CONFLICT(path) DO UPDATE SET
            discovered_at = MIN(projects.discovered_at, excluded.discovered_at),
            last_seen_at = MAX(projects.last_seen_at, excluded.last_seen_at),
            last_cleaned_at = CASE
                WHEN projects.last_cleaned_at IS NULL THEN excluded.last_cleaned_at
                WHEN excluded.last_cleaned_at IS NULL THEN projects.last_cleaned_at
                ELSE MAX(projects.last_cleaned_at, excluded.last_cleaned_at)
            END
        ",
        params![old_path, new_path],
    )?;
    tx.execute(
        "
        INSERT INTO linked_worktrees (
            primary_path,
            linked_path,
            canonical_primary_path
        )
        SELECT
            CASE WHEN primary_path=?1 THEN ?2 ELSE primary_path END,
            CASE WHEN linked_path=?1 THEN ?2 ELSE linked_path END,
            canonical_primary_path
        FROM linked_worktrees
        WHERE (primary_path=?1 OR linked_path=?1)
          AND CASE WHEN primary_path=?1 THEN ?2 ELSE primary_path END
              <> CASE WHEN linked_path=?1 THEN ?2 ELSE linked_path END
        ON CONFLICT(primary_path, linked_path) DO UPDATE SET
            canonical_primary_path = CASE
                WHEN linked_worktrees.canonical_primary_path
                    = excluded.canonical_primary_path
                THEN linked_worktrees.canonical_primary_path
                ELSE NULL
            END
        ",
        params![old_path, new_path],
    )?;
    tx.execute(
        "DELETE FROM linked_worktrees WHERE primary_path=?1 OR linked_path=?1",
        [old_path],
    )?;
    tx.execute(
        "
        INSERT INTO worktree_discovery_failures (
            primary_path,
            failed_at,
            message,
            canonical_primary_path
        )
        SELECT ?2, failed_at, message, canonical_primary_path
        FROM worktree_discovery_failures
        WHERE primary_path=?1
        ON CONFLICT(primary_path) DO UPDATE SET
            failed_at = MAX(
                worktree_discovery_failures.failed_at,
                excluded.failed_at
            ),
            message = CASE
                WHEN excluded.failed_at >= worktree_discovery_failures.failed_at
                THEN excluded.message
                ELSE worktree_discovery_failures.message
            END,
            canonical_primary_path = CASE
                WHEN worktree_discovery_failures.canonical_primary_path
                    = excluded.canonical_primary_path
                THEN worktree_discovery_failures.canonical_primary_path
                ELSE NULL
            END
        ",
        params![old_path, new_path],
    )?;
    tx.execute(
        "DELETE FROM worktree_discovery_failures WHERE primary_path=?1",
        [old_path],
    )?;
    tx.execute("DELETE FROM projects WHERE path=?1", [old_path])?;
    Ok(())
}

fn normalize_failed_primary_aliases_for_success(
    tx: &Transaction<'_>,
    canonical_primary: &str,
) -> Result<()> {
    let aliases = {
        let mut stmt = tx.prepare(
            "
            SELECT primary_path
            FROM worktree_discovery_failures
            WHERE canonical_primary_path=?1 AND primary_path<>?1
            ORDER BY primary_path
            ",
        )?;
        let rows = stmt.query_map([canonical_primary], |row| row.get::<_, String>(0))?;
        collect_rows(rows)?
    };

    for alias in aliases {
        rekey_trusted_failed_primary_for_success(tx, &alias, canonical_primary)?;
    }
    Ok(())
}

fn rekey_trusted_failed_primary_for_success(
    tx: &Transaction<'_>,
    old_primary: &str,
    canonical_primary: &str,
) -> Result<()> {
    tx.execute(
        "
        INSERT OR IGNORE INTO linked_worktrees (
            primary_path,
            linked_path,
            canonical_primary_path
        )
        SELECT ?2, linked_path, canonical_primary_path
        FROM linked_worktrees
        WHERE primary_path=?1
          AND canonical_primary_path=?2
          AND linked_path<>?2
        ",
        params![old_primary, canonical_primary],
    )?;
    tx.execute(
        "
        DELETE FROM linked_worktrees
        WHERE primary_path=?1 AND canonical_primary_path=?2
        ",
        params![old_primary, canonical_primary],
    )?;
    tx.execute(
        "
        INSERT INTO worktree_discovery_failures (
            primary_path,
            failed_at,
            message,
            canonical_primary_path
        )
        SELECT ?2, failed_at, message, canonical_primary_path
        FROM worktree_discovery_failures
        WHERE primary_path=?1
        ON CONFLICT(primary_path) DO UPDATE SET
            failed_at = MAX(
                worktree_discovery_failures.failed_at,
                excluded.failed_at
            ),
            message = CASE
                WHEN excluded.failed_at >= worktree_discovery_failures.failed_at
                THEN excluded.message
                ELSE worktree_discovery_failures.message
            END,
            canonical_primary_path = CASE
                WHEN worktree_discovery_failures.canonical_primary_path
                    = excluded.canonical_primary_path
                THEN worktree_discovery_failures.canonical_primary_path
                ELSE NULL
            END
        ",
        params![old_primary, canonical_primary],
    )?;
    tx.execute(
        "DELETE FROM worktree_discovery_failures WHERE primary_path=?1",
        [old_primary],
    )?;
    tx.execute("DELETE FROM projects WHERE path=?1", [old_primary])?;
    Ok(())
}

fn rekey_trusted_linked_associations_for_success(
    tx: &Transaction<'_>,
    canonical_primary: &str,
) -> Result<()> {
    let aliases = {
        let mut stmt = tx.prepare(
            "
            SELECT DISTINCT primary_path
            FROM linked_worktrees
            WHERE canonical_primary_path=?1 AND primary_path<>?1
            ORDER BY primary_path
            ",
        )?;
        let rows = stmt.query_map([canonical_primary], |row| row.get::<_, String>(0))?;
        collect_rows(rows)?
    };
    for alias in aliases {
        tx.execute(
            "
            INSERT OR IGNORE INTO linked_worktrees (
                primary_path,
                linked_path,
                canonical_primary_path
            )
            SELECT ?2, linked_path, canonical_primary_path
            FROM linked_worktrees
            WHERE primary_path=?1
              AND canonical_primary_path=?2
              AND linked_path<>?2
            ",
            params![&alias, canonical_primary],
        )?;
        tx.execute(
            "
            DELETE FROM linked_worktrees
            WHERE primary_path=?1 AND canonical_primary_path=?2
            ",
            params![&alias, canonical_primary],
        )?;
        tx.execute("DELETE FROM projects WHERE path=?1", [&alias])?;
    }
    Ok(())
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

fn path_to_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn to_epoch(time: SystemTime) -> Result<i64> {
    Ok(time.duration_since(SystemTime::UNIX_EPOCH)?.as_secs() as i64)
}

fn from_epoch(secs: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64)
}

fn to_usize(value: i64) -> usize {
    value.max(0) as usize
}

fn to_u64(value: i64) -> u64 {
    value.max(0) as u64
}

#[allow(dead_code)]
fn _normalize(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().to_path_buf()
}
