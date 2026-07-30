use crate::identity::{FilesystemIdentity, MountIdentity, ReviewedIdentity};
use crate::safety::{CleanDecision, ProjectClass, ProjectReview, ReviewSummary, SkipReason};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const REVIEW_PLAN_TTL: Duration = Duration::from_secs(30 * 60);
pub const REVIEW_PLAN_RETENTION: usize = 20;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryGeneration {
    pub id: i64,
    pub created_at: SystemTime,
    pub policy_hash: String,
    pub boot_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryOriginKind {
    ScanRoot,
    ExplicitProject,
}

impl DiscoveryOriginKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ScanRoot => "scan_root",
            Self::ExplicitProject => "explicit_project",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryOriginRecord {
    pub id: i64,
    pub generation_id: i64,
    pub kind: DiscoveryOriginKind,
    pub configured_path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub completed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectObservation {
    pub generation_id: i64,
    pub origin_id: i64,
    pub project_path: PathBuf,
    pub project_identity: FilesystemIdentity,
    pub target_identity: Option<FilesystemIdentity>,
    pub boot_session_id: Option<String>,
    pub observed_at: SystemTime,
    pub authorized: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationReconciliation {
    pub project_path: PathBuf,
    pub project_identity: FilesystemIdentity,
    pub target_identity: Option<FilesystemIdentity>,
    pub observed_at: SystemTime,
    pub authorized: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginReconciliation {
    pub kind: DiscoveryOriginKind,
    pub configured_path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub completed: bool,
    pub error: Option<String>,
    pub observations: Vec<ObservationReconciliation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationReconciliation {
    pub policy_hash: String,
    pub boot_session_id: Option<String>,
    pub origins: Vec<OriginReconciliation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeReconciliation {
    Success {
        primary: PathBuf,
        linked: Vec<PathBuf>,
        excluded: Vec<PathBuf>,
        out_of_scope: Vec<PathBuf>,
    },
    Failure {
        primary: PathBuf,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPublication {
    pub generation: GenerationReconciliation,
    pub worktrees: Vec<WorktreeReconciliation>,
    pub diagnostics: Vec<ErrorRecord>,
}

struct PreparedWorktreeSuccess {
    primary: String,
    canonical_primary: Option<String>,
    linked: BTreeSet<String>,
    excluded: BTreeSet<String>,
    out_of_scope: BTreeSet<String>,
}

struct PreparedWorktreeFailure {
    primary: String,
    canonical_primary: Option<String>,
    failed_at: i64,
    message: String,
}

enum PreparedWorktreeReconciliation {
    Success(PreparedWorktreeSuccess),
    Failure(PreparedWorktreeFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPlanTarget {
    pub ordinal: usize,
    pub review: ProjectReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPlan {
    pub id: i64,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
    pub policy_hash: String,
    pub generation_id: i64,
    pub coverage_incomplete: bool,
    pub candidate_bytes: i64,
    pub targets: Vec<ReviewPlanTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanLoadError {
    Missing,
    Expired,
    PolicyMismatch,
    GenerationMismatch,
    Storage(String),
}

impl fmt::Display for PlanLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("review plan does not exist"),
            Self::Expired => formatter.write_str("review plan has expired"),
            Self::PolicyMismatch => {
                formatter.write_str("review plan policy does not match current policy")
            }
            Self::GenerationMismatch => formatter
                .write_str("review plan generation is not the current discovery generation"),
            Self::Storage(message) => write!(formatter, "review plan storage error: {message}"),
        }
    }
}

impl Error for PlanLoadError {}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let store = Self { conn };
        store.prune_review_plans(SystemTime::now(), None)?;
        Ok(store)
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
        if current < 8 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(
                "
                UPDATE runs
                SET projects_cleaned = (
                        SELECT COUNT(*)
                        FROM clean_events
                        WHERE run_id = runs.id
                          AND exit_code = 0
                    ),
                    bytes_recovered = COALESCE((
                        SELECT SUM(MAX(bytes_before - bytes_after, 0))
                        FROM clean_events
                        WHERE run_id = runs.id
                          AND exit_code = 0
                    ), 0),
                    errors_count = MAX(
                        errors_count,
                        (
                            SELECT COUNT(*)
                            FROM clean_events
                            WHERE run_id = runs.id
                              AND exit_code <> 0
                        )
                    );

                UPDATE projects
                SET last_cleaned_at = (
                    SELECT MAX(ts)
                    FROM clean_events
                    WHERE path = projects.path
                      AND exit_code = 0
                );

                INSERT INTO errors (ts, category, path, message)
                SELECT
                    clean_events.ts,
                    'clean',
                    clean_events.path,
                    'cargo clean exited ' || clean_events.exit_code
                        || CASE
                            WHEN clean_events.stderr_excerpt = '' THEN ''
                            ELSE ': ' || clean_events.stderr_excerpt
                        END
                FROM clean_events
                WHERE clean_events.exit_code <> 0
                  AND NOT EXISTS(
                      SELECT 1
                      FROM errors
                      WHERE errors.ts = clean_events.ts
                        AND errors.category = 'clean'
                        AND errors.path = clean_events.path
                        AND errors.message =
                            'cargo clean exited ' || clean_events.exit_code
                            || CASE
                                WHEN clean_events.stderr_excerpt = '' THEN ''
                                ELSE ': ' || clean_events.stderr_excerpt
                            END
                  );

                INSERT INTO schema_version (version) VALUES (8);
                ",
            )?;
            tx.commit()?;
        }
        if current < 9 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS scheduler_state (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    updated_at INTEGER NOT NULL,
                    next_clean_at INTEGER NOT NULL,
                    next_scan_at INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS discovery_generations (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at INTEGER NOT NULL,
                    policy_hash TEXT NOT NULL,
                    boot_session_id TEXT,
                    authority_valid INTEGER NOT NULL DEFAULT 1
                        CHECK(authority_valid IN (0, 1))
                );
                CREATE INDEX IF NOT EXISTS idx_discovery_generations_policy_created
                    ON discovery_generations(policy_hash, created_at DESC);

                CREATE TABLE IF NOT EXISTS discovery_origins (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    generation_id INTEGER NOT NULL
                        REFERENCES discovery_generations(id) ON DELETE CASCADE,
                    kind TEXT NOT NULL CHECK(kind IN ('scan_root', 'explicit_project')),
                    configured_path TEXT NOT NULL,
                    canonical_path TEXT,
                    completed INTEGER NOT NULL CHECK(completed IN (0, 1)),
                    error TEXT
                );

                CREATE TABLE IF NOT EXISTS project_observations (
                    generation_id INTEGER NOT NULL
                        REFERENCES discovery_generations(id) ON DELETE CASCADE,
                    origin_id INTEGER NOT NULL
                        REFERENCES discovery_origins(id) ON DELETE CASCADE,
                    project_path TEXT NOT NULL,
                    project_device INTEGER NOT NULL,
                    project_inode INTEGER NOT NULL,
                    target_device INTEGER,
                    target_inode INTEGER,
                    observed_at INTEGER NOT NULL,
                    authorized INTEGER NOT NULL CHECK(authorized IN (0, 1)),
                    blocked_reason TEXT,
                    boot_session_id TEXT,
                    PRIMARY KEY(generation_id, origin_id, project_path)
                );
                CREATE INDEX IF NOT EXISTS idx_project_observations_authorized
                    ON project_observations(generation_id, authorized, project_path);
                ",
            )?;
            let has_last_forced_scan_at = {
                let mut statement = tx.prepare("PRAGMA table_info(scheduler_state)")?;
                let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
                collect_rows(columns)?
                    .into_iter()
                    .any(|column| column == "last_forced_scan_at")
            };
            if !has_last_forced_scan_at {
                tx.execute(
                    "ALTER TABLE scheduler_state ADD COLUMN last_forced_scan_at INTEGER",
                    [],
                )?;
            }
            tx.execute("INSERT INTO schema_version(version) VALUES (9)", [])?;
            tx.commit()?;
        }
        let has_generation_authority_valid = {
            let mut statement = self
                .conn
                .prepare("PRAGMA table_info(discovery_generations)")?;
            let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
            collect_rows(columns)?
                .into_iter()
                .any(|column| column == "authority_valid")
        };
        if current < 10 || !has_generation_authority_valid {
            let tx = self.conn.unchecked_transaction()?;
            if !has_generation_authority_valid {
                tx.execute(
                    "
                    ALTER TABLE discovery_generations
                    ADD COLUMN authority_valid INTEGER NOT NULL DEFAULT 1
                        CHECK(authority_valid IN (0, 1))
                    ",
                    [],
                )?;
            }
            let has_boot_session_id = {
                let mut statement = tx.prepare("PRAGMA table_info(project_observations)")?;
                let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
                collect_rows(columns)?
                    .into_iter()
                    .any(|column| column == "boot_session_id")
            };
            if !has_boot_session_id {
                tx.execute(
                    "ALTER TABLE project_observations ADD COLUMN boot_session_id TEXT",
                    [],
                )?;
            }
            tx.execute(
                "
                UPDATE project_observations
                SET boot_session_id = NULL,
                    blocked_reason = CASE
                        WHEN authorized = 1 THEN COALESCE(
                            blocked_reason,
                            'migration requires fresh discovery'
                        )
                        ELSE blocked_reason
                    END,
                    authorized = 0
                ",
                [],
            )?;
            tx.execute("UPDATE discovery_generations SET authority_valid = 0", [])?;
            if current < 10 {
                tx.execute("INSERT INTO schema_version(version) VALUES (10)", [])?;
            }
            tx.commit()?;
        }
        let has_scan_retry_at = {
            let mut statement = self.conn.prepare("PRAGMA table_info(scheduler_state)")?;
            let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
            collect_rows(columns)?
                .into_iter()
                .any(|column| column == "scan_retry_at")
        };
        if current < 11 || !has_scan_retry_at {
            let tx = self.conn.unchecked_transaction()?;
            if !has_scan_retry_at {
                tx.execute(
                    "ALTER TABLE scheduler_state ADD COLUMN scan_retry_at INTEGER",
                    [],
                )?;
            }
            if current < 11 {
                tx.execute("INSERT INTO schema_version(version) VALUES (11)", [])?;
            }
            tx.commit()?;
        }
        if current < 12 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS review_plans (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL,
                    policy_hash TEXT NOT NULL,
                    generation_id INTEGER NOT NULL
                        REFERENCES discovery_generations(id) ON DELETE CASCADE,
                    coverage_incomplete INTEGER NOT NULL
                        CHECK(coverage_incomplete IN (0, 1)),
                    candidate_bytes INTEGER NOT NULL CHECK(candidate_bytes >= 0)
                );

                CREATE TABLE IF NOT EXISTS review_plan_targets (
                    plan_id INTEGER NOT NULL
                        REFERENCES review_plans(id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                    project_path TEXT NOT NULL,
                    canonical_project_path TEXT,
                    project_class TEXT NOT NULL
                        CHECK(project_class IN (
                            'workspace',
                            'managed_cache',
                            'container_storage'
                        )),
                    target_path TEXT NOT NULL,
                    project_device INTEGER CHECK(project_device >= 0),
                    project_inode INTEGER CHECK(project_inode >= 0),
                    target_device INTEGER CHECK(target_device >= 0),
                    target_inode INTEGER CHECK(target_inode >= 0),
                    review_boot_session_id TEXT,
                    reviewed_bytes INTEGER NOT NULL CHECK(reviewed_bytes >= 0),
                    decision TEXT NOT NULL
                        CHECK(decision IN ('cleanable', 'skipped')),
                    skip_reason TEXT,
                    skip_newest_age_secs INTEGER
                        CHECK(skip_newest_age_secs >= 0),
                    CHECK(
                        (
                            project_device IS NULL
                            AND project_inode IS NULL
                            AND target_device IS NULL
                            AND target_inode IS NULL
                            AND review_boot_session_id IS NULL
                        )
                        OR (
                            project_device IS NOT NULL
                            AND project_inode IS NOT NULL
                            AND target_device IS NOT NULL
                            AND target_inode IS NOT NULL
                        )
                    ),
                    CHECK(
                        (decision = 'cleanable' AND skip_reason IS NULL)
                        OR (decision = 'skipped' AND skip_reason IS NOT NULL)
                    ),
                    CHECK(
                        (skip_reason = 'active_recent_write'
                            AND skip_newest_age_secs IS NOT NULL)
                        OR (skip_reason <> 'active_recent_write'
                            AND skip_newest_age_secs IS NULL)
                        OR skip_reason IS NULL
                    ),
                    PRIMARY KEY(plan_id, ordinal)
                );

                CREATE INDEX IF NOT EXISTS idx_review_plans_expires
                    ON review_plans(expires_at);
                INSERT INTO schema_version(version) VALUES (12);
                ",
            )?;
            tx.commit()?;
        }
        let has_project_mount_identity =
            connection_column_exists(&self.conn, "project_observations", "project_mount_id")?;
        let has_target_mount_identity =
            connection_column_exists(&self.conn, "project_observations", "target_mount_id")?;
        let has_review_project_mount_identity =
            connection_column_exists(&self.conn, "review_plan_targets", "project_mount_id")?;
        let has_review_target_mount_identity =
            connection_column_exists(&self.conn, "review_plan_targets", "target_mount_id")?;
        if current < 13
            || !has_project_mount_identity
            || !has_target_mount_identity
            || !has_review_project_mount_identity
            || !has_review_target_mount_identity
        {
            let tx = self.conn.unchecked_transaction()?;
            if !has_project_mount_identity {
                tx.execute(
                    "ALTER TABLE project_observations ADD COLUMN project_mount_id TEXT",
                    [],
                )?;
            }
            if !has_target_mount_identity {
                tx.execute(
                    "ALTER TABLE project_observations ADD COLUMN target_mount_id TEXT",
                    [],
                )?;
            }
            if !has_review_project_mount_identity {
                tx.execute(
                    "ALTER TABLE review_plan_targets ADD COLUMN project_mount_id TEXT",
                    [],
                )?;
            }
            if !has_review_target_mount_identity {
                tx.execute(
                    "ALTER TABLE review_plan_targets ADD COLUMN target_mount_id TEXT",
                    [],
                )?;
            }
            tx.execute(
                "
                UPDATE project_observations
                SET authorized = 0,
                    blocked_reason = CASE
                        WHEN authorized = 1 THEN COALESCE(
                            blocked_reason,
                            'migration requires fresh mount identity discovery'
                        )
                        ELSE blocked_reason
                    END
                ",
                [],
            )?;
            tx.execute("UPDATE discovery_generations SET authority_valid = 0", [])?;
            tx.execute(
                "
                CREATE UNIQUE INDEX IF NOT EXISTS
                    idx_discovery_generations_single_valid
                ON discovery_generations(authority_valid)
                WHERE authority_valid = 1
                ",
                [],
            )?;
            if current < 13 {
                tx.execute("INSERT INTO schema_version(version) VALUES (13)", [])?;
            }
            tx.commit()?;
        }
        self.prune_review_plans(SystemTime::now(), None)?;
        Ok(())
    }

    pub fn table_exists(&self, table: &str) -> Result<bool> {
        connection_table_exists(&self.conn, table)
    }

    pub fn create_review_plan(
        &self,
        created_at: SystemTime,
        policy_hash: &str,
        generation_id: i64,
        coverage_incomplete: bool,
        candidate_bytes: i64,
        reviews: &[ProjectReview],
    ) -> Result<ReviewPlan> {
        let created_at_epoch = to_epoch(created_at)?;
        let created_at = from_epoch(created_at_epoch);
        let expires_at = created_at
            .checked_add(REVIEW_PLAN_TTL)
            .context("review plan expiry exceeds system time range")?;
        let expires_at_epoch = to_epoch(expires_at)?;
        let tx = self.conn.unchecked_transaction()?;

        if !generation_is_current(&tx, policy_hash, generation_id)? {
            bail!(
                "generation {generation_id} is not the current valid generation for policy {policy_hash}"
            );
        }

        prune_review_plans_in_transaction(
            &tx,
            created_at_epoch,
            Some((policy_hash, generation_id)),
        )?;
        tx.execute(
            "
            INSERT INTO review_plans (
                created_at,
                expires_at,
                policy_hash,
                generation_id,
                coverage_incomplete,
                candidate_bytes
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                created_at_epoch,
                expires_at_epoch,
                policy_hash,
                generation_id,
                coverage_incomplete,
                candidate_bytes,
            ],
        )?;
        let plan_id = tx.last_insert_rowid();
        let mut targets = Vec::with_capacity(reviews.len());
        for (ordinal, review) in reviews.iter().enumerate() {
            insert_review_plan_target(&tx, plan_id, ordinal, review)?;
            targets.push(ReviewPlanTarget {
                ordinal,
                review: review.clone(),
            });
        }
        prune_review_plan_retention_in_transaction(&tx)?;
        tx.commit()?;

        Ok(ReviewPlan {
            id: plan_id,
            created_at,
            expires_at,
            policy_hash: policy_hash.to_string(),
            generation_id,
            coverage_incomplete,
            candidate_bytes,
            targets,
        })
    }

    pub fn load_review_plan(
        &self,
        id: i64,
        now: SystemTime,
        current_policy_hash: &str,
        current_generation_id: i64,
    ) -> std::result::Result<ReviewPlan, PlanLoadError> {
        let now_epoch = to_epoch(now).map_err(plan_storage_error)?;
        let plan = load_review_plan_from_connection(&self.conn, id).map_err(plan_storage_error)?;
        let result = match plan {
            None => Err(PlanLoadError::Missing),
            Some(plan) if plan.expires_at <= now => Err(PlanLoadError::Expired),
            Some(plan) if plan.policy_hash != current_policy_hash => {
                Err(PlanLoadError::PolicyMismatch)
            }
            Some(plan)
                if plan.generation_id != current_generation_id
                    || !generation_is_current(
                        &self.conn,
                        current_policy_hash,
                        current_generation_id,
                    )
                    .map_err(plan_storage_error)? =>
            {
                Err(PlanLoadError::GenerationMismatch)
            }
            Some(plan) => Ok(plan),
        };

        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(plan_storage_error)?;
        prune_review_plans_in_transaction(
            &tx,
            now_epoch,
            Some((current_policy_hash, current_generation_id)),
        )
        .map_err(plan_storage_error)?;
        tx.commit().map_err(plan_storage_error)?;
        result
    }

    pub fn prune_review_plans(
        &self,
        now: SystemTime,
        current_authority: Option<(&str, i64)>,
    ) -> Result<usize> {
        if !connection_table_exists(&self.conn, "review_plans")?
            || !connection_table_exists(&self.conn, "discovery_generations")?
            || !connection_column_exists(&self.conn, "discovery_generations", "authority_valid")?
        {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let pruned = prune_review_plans_in_transaction(&tx, to_epoch(now)?, current_authority)?;
        tx.commit()?;
        Ok(pruned)
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

    pub fn reconcile_excluded_discovery_state<F>(&self, mut is_excluded: F) -> Result<()>
    where
        F: FnMut(&Path) -> bool,
    {
        let projects = {
            let mut stmt = self.conn.prepare("SELECT path FROM projects")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            collect_rows(rows)?
        };
        let linked = {
            let mut stmt = self.conn.prepare(
                "SELECT primary_path, linked_path, canonical_primary_path
                 FROM linked_worktrees",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;
            collect_rows(rows)?
        };
        let failures = {
            let mut stmt = self.conn.prepare(
                "SELECT primary_path, canonical_primary_path
                 FROM worktree_discovery_failures",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            collect_rows(rows)?
        };

        let mut remove_projects = Vec::new();
        for path in projects {
            if should_remove_cached_path(Path::new(&path), &mut is_excluded)? {
                remove_projects.push(path);
            }
        }

        let mut remove_linked = Vec::new();
        for (primary, linked, canonical_primary) in linked {
            let remove = should_remove_durable_identity(Path::new(&primary), &mut is_excluded)?
                || should_remove_durable_identity(Path::new(&linked), &mut is_excluded)?
                || match canonical_primary.as_deref() {
                    Some(path) => {
                        should_remove_durable_identity(Path::new(path), &mut is_excluded)?
                    }
                    None => false,
                };
            if remove {
                remove_linked.push((primary, linked));
            }
        }

        let mut remove_failures = Vec::new();
        for (primary, canonical_primary) in failures {
            let remove = should_remove_durable_identity(Path::new(&primary), &mut is_excluded)?
                || match canonical_primary.as_deref() {
                    Some(path) => {
                        should_remove_durable_identity(Path::new(path), &mut is_excluded)?
                    }
                    None => false,
                };
            if remove {
                remove_failures.push(primary);
            }
        }

        let tx = self.conn.unchecked_transaction()?;
        for path in remove_projects {
            tx.execute("DELETE FROM projects WHERE path=?1", [&path])?;
        }
        for (primary, linked) in remove_linked {
            tx.execute(
                "DELETE FROM linked_worktrees
                 WHERE primary_path=?1 AND linked_path=?2",
                params![primary, linked],
            )?;
        }
        for primary in remove_failures {
            tx.execute(
                "DELETE FROM worktree_discovery_failures WHERE primary_path=?1",
                [&primary],
            )?;
        }
        tx.commit()?;
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
        let replacements = self.resolvable_project_alias_replacements()?;
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

    fn resolvable_project_alias_replacements(&self) -> Result<Vec<(String, String)>> {
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
        Ok(replacements)
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
        let prepared = prepare_worktree_success(primary, linked, excluded, out_of_scope)?;
        let tx = self.conn.unchecked_transaction()?;
        replace_linked_worktrees_in_transaction(&tx, &prepared)?;
        tx.commit()?;
        Ok(())
    }

    pub fn mark_worktree_discovery_failed(
        &self,
        primary: &Path,
        now: SystemTime,
        message: &str,
    ) -> Result<()> {
        let prepared = prepare_worktree_failure(primary, now, message)?;
        let tx = self.conn.unchecked_transaction()?;
        mark_worktree_discovery_failed_in_transaction(&tx, &prepared)?;
        tx.commit()?;
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

    pub fn reconcile_generation(
        &self,
        created_at: SystemTime,
        reconciliation: &GenerationReconciliation,
    ) -> Result<DiscoveryGeneration> {
        let created_at_epoch = to_epoch(created_at)?;
        let tx = self.conn.unchecked_transaction()?;
        let generation =
            reconcile_generation_in_transaction(&tx, created_at_epoch, created_at, reconciliation)?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn publish_scan(
        &self,
        created_at: SystemTime,
        publication: &ScanPublication,
    ) -> Result<DiscoveryGeneration> {
        let created_at_epoch = to_epoch(created_at)?;
        let alias_replacements = self.resolvable_project_alias_replacements()?;
        let prepared_worktrees = publication
            .worktrees
            .iter()
            .map(|worktree| prepare_worktree_reconciliation(worktree, created_at))
            .collect::<Result<Vec<_>>>()?;
        let prepared_diagnostics = publication
            .diagnostics
            .iter()
            .map(|diagnostic| Ok((to_epoch(diagnostic.ts)?, diagnostic)))
            .collect::<Result<Vec<_>>>()?;

        let tx = self.conn.unchecked_transaction()?;
        for (alias, canonical) in alias_replacements {
            replace_project_path_in_transaction(&tx, &alias, &canonical)?;
        }
        for worktree in &prepared_worktrees {
            match worktree {
                PreparedWorktreeReconciliation::Success(success) => {
                    replace_linked_worktrees_in_transaction(&tx, success)?;
                }
                PreparedWorktreeReconciliation::Failure(failure) => {
                    mark_worktree_discovery_failed_in_transaction(&tx, failure)?;
                }
            }
        }
        let generation = reconcile_generation_in_transaction(
            &tx,
            created_at_epoch,
            created_at,
            &publication.generation,
        )?;
        for (timestamp, diagnostic) in prepared_diagnostics {
            record_error_in_transaction(&tx, timestamp, diagnostic)?;
        }
        tx.commit()?;
        Ok(generation)
    }

    pub fn current_generation(&self, policy_hash: &str) -> Result<Option<DiscoveryGeneration>> {
        self.conn
            .query_row(
                "
                SELECT id, created_at, policy_hash, boot_session_id
                FROM discovery_generations
                WHERE authority_valid = 1
                  AND policy_hash = ?1
                  AND id = (
                      SELECT id
                      FROM discovery_generations
                      ORDER BY id DESC
                      LIMIT 1
                  )
                ",
                [policy_hash],
                |row| {
                    Ok(DiscoveryGeneration {
                        id: row.get(0)?,
                        created_at: from_epoch(row.get(1)?),
                        policy_hash: row.get(2)?,
                        boot_session_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn has_matching_generation(&self, policy_hash: &str) -> Result<bool> {
        Ok(self.current_generation(policy_hash)?.is_some())
    }

    pub fn current_generation_coverage_incomplete(&self, policy_hash: &str) -> Result<bool> {
        let incomplete = self.conn.query_row(
            "
            SELECT CASE
                WHEN current_generation.id IS NULL THEN 1
                ELSE EXISTS(
                    SELECT 1
                    FROM discovery_origins
                    WHERE generation_id = current_generation.id
                      AND completed = 0
                )
            END
            FROM (SELECT 1) AS singleton
            LEFT JOIN (
                SELECT id
                FROM discovery_generations
                WHERE authority_valid = 1
                  AND policy_hash = ?1
                  AND id = (
                      SELECT id
                      FROM discovery_generations
                      ORDER BY id DESC
                      LIMIT 1
                  )
            ) AS current_generation
            ",
            [policy_hash],
            |row| row.get(0),
        )?;
        Ok(incomplete)
    }

    pub fn discovery_origins(&self, generation_id: i64) -> Result<Vec<DiscoveryOriginRecord>> {
        let mut statement = self.conn.prepare(
            "
            SELECT
                id,
                generation_id,
                kind,
                configured_path,
                canonical_path,
                completed,
                error
            FROM discovery_origins
            WHERE generation_id = ?1
            ORDER BY id
            ",
        )?;
        let rows = statement.query_map([generation_id], |row| {
            let kind: String = row.get(2)?;
            let kind = match kind.as_str() {
                "scan_root" => DiscoveryOriginKind::ScanRoot,
                "explicit_project" => DiscoveryOriginKind::ExplicitProject,
                other => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        format!("unknown discovery origin kind {other:?}").into(),
                    ));
                }
            };
            let configured_path: String = row.get(3)?;
            let canonical_path: Option<String> = row.get(4)?;
            Ok(DiscoveryOriginRecord {
                id: row.get(0)?,
                generation_id: row.get(1)?,
                kind,
                configured_path: PathBuf::from(configured_path),
                canonical_path: canonical_path.map(PathBuf::from),
                completed: row.get(5)?,
                error: row.get(6)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn authorized_observations(&self, generation_id: i64) -> Result<Vec<ProjectObservation>> {
        let mut statement = self.conn.prepare(
            "
            SELECT
                observations.generation_id,
                observations.origin_id,
                observations.project_path,
                observations.project_device,
                observations.project_inode,
                observations.project_mount_id,
                observations.target_device,
                observations.target_inode,
                observations.target_mount_id,
                observations.observed_at,
                observations.authorized,
                observations.blocked_reason,
                observations.boot_session_id
            FROM project_observations AS observations
            JOIN discovery_origins AS origins
              ON origins.id = observations.origin_id
             AND origins.generation_id = observations.generation_id
            JOIN discovery_generations AS generation
              ON generation.id = observations.generation_id
            WHERE observations.generation_id = ?1
              AND observations.authorized = 1
              AND origins.completed = 1
              AND generation.authority_valid = 1
              AND generation.id = (
                  SELECT id
                  FROM discovery_generations
                  ORDER BY id DESC
                  LIMIT 1
              )
            ORDER BY observations.project_path, observations.origin_id
            ",
        )?;
        let rows = statement.query_map([generation_id], project_observation_from_row)?;
        collect_rows(rows)
    }

    pub fn mark_observation_reverified(
        &self,
        generation_id: i64,
        path: &Path,
        identity: &ReviewedIdentity,
    ) -> Result<()> {
        let updated = self.conn.execute(
            "
            UPDATE project_observations
            SET project_device = ?1,
                project_inode = ?2,
                project_mount_id = ?3,
                target_device = ?4,
                target_inode = ?5,
                target_mount_id = ?6,
                boot_session_id = ?7
            WHERE generation_id = ?8
              AND project_path = ?9
              AND authorized = 1
              AND EXISTS(
                  SELECT 1
                  FROM discovery_generations
                  WHERE discovery_generations.id =
                        project_observations.generation_id
                    AND discovery_generations.authority_valid = 1
                    AND discovery_generations.id = (
                        SELECT id
                        FROM discovery_generations
                        ORDER BY id DESC
                        LIMIT 1
                    )
              )
              AND EXISTS(
                  SELECT 1
                  FROM discovery_origins
                  WHERE discovery_origins.id = project_observations.origin_id
                    AND discovery_origins.generation_id =
                        project_observations.generation_id
                    AND discovery_origins.completed = 1
              )
            ",
            params![
                identity.project.device,
                identity.project.inode,
                identity.project.mount.0.as_str(),
                identity.target.device,
                identity.target.inode,
                identity.target.mount.0.as_str(),
                identity
                    .boot_session
                    .as_ref()
                    .map(|boot_session| &boot_session.0),
                generation_id,
                path_to_string(path)?,
            ],
        )?;
        if updated == 0 {
            bail!(
                "no authorized observation for generation {generation_id} and path {}",
                path.display()
            );
        }
        Ok(())
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

    pub fn scan_coverage_incomplete_since(&self, since: SystemTime) -> Result<bool> {
        let incomplete = self.conn.query_row(
            "
            SELECT EXISTS(
                SELECT 1
                FROM errors
                WHERE ts >= ?1
                  AND (
                      category = 'scan'
                      OR (
                          category = 'worktree_discovery'
                          AND (
                              path IS NULL
                              OR EXISTS(
                                  SELECT 1
                                  FROM worktree_discovery_failures
                                  WHERE primary_path = errors.path
                                     OR canonical_primary_path = errors.path
                              )
                          )
                      )
                  )
            )
            ",
            [to_epoch(since)?],
            |row| row.get(0),
        )?;
        Ok(incomplete)
    }

    pub fn total_bytes_recovered(&self, since: SystemTime) -> Result<i64> {
        let total = self.conn.query_row(
            "
            SELECT COALESCE(SUM(bytes_before - bytes_after), 0)
            FROM clean_events WHERE ts >= ?1 AND exit_code = 0
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
            WHERE ts >= ?1 AND exit_code = 0
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

    pub fn failed_clean_attempts(&self, since: SystemTime) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM clean_events WHERE ts >= ?1 AND exit_code <> 0",
                [to_epoch(since)?],
                |row| row.get(0),
            )
            .map_err(Into::into)
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

    pub fn last_forced_scan_at(&self) -> Result<Option<SystemTime>> {
        self.conn
            .query_row(
                "
                SELECT last_forced_scan_at
                FROM scheduler_state
                WHERE id = 1
                ",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map(|value| value.flatten().map(from_epoch))
            .map_err(Into::into)
    }

    pub fn record_forced_scan_at(&self, when: SystemTime) -> Result<()> {
        let when = to_epoch(when)?;
        self.conn.execute(
            "
            INSERT INTO scheduler_state (
                id, updated_at, next_clean_at, next_scan_at, last_forced_scan_at
            )
            VALUES (1, ?1, ?1, ?1, ?1)
            ON CONFLICT(id) DO UPDATE SET
                last_forced_scan_at = excluded.last_forced_scan_at
            ",
            [when],
        )?;
        Ok(())
    }

    pub fn scan_retry_at(&self) -> Result<Option<SystemTime>> {
        self.conn
            .query_row(
                "
                SELECT scan_retry_at
                FROM scheduler_state
                WHERE id = 1
                ",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map(|value| value.flatten().map(from_epoch))
            .map_err(Into::into)
    }

    pub fn record_scan_retry_at(&self, when: SystemTime) -> Result<()> {
        let changed = self.conn.execute(
            "
            UPDATE scheduler_state
            SET scan_retry_at = ?1
            WHERE id = 1
            ",
            [to_epoch(when)?],
        )?;
        if changed == 0 {
            anyhow::bail!("record scan retry deadline without scheduler state");
        }
        Ok(())
    }

    pub fn clear_scan_retry_at(&self) -> Result<()> {
        self.conn.execute(
            "
            UPDATE scheduler_state
            SET scan_retry_at = NULL
            WHERE id = 1
            ",
            [],
        )?;
        Ok(())
    }
}

fn prepare_worktree_reconciliation(
    reconciliation: &WorktreeReconciliation,
    now: SystemTime,
) -> Result<PreparedWorktreeReconciliation> {
    match reconciliation {
        WorktreeReconciliation::Success {
            primary,
            linked,
            excluded,
            out_of_scope,
        } => Ok(PreparedWorktreeReconciliation::Success(
            prepare_worktree_success(primary, linked, excluded, out_of_scope)?,
        )),
        WorktreeReconciliation::Failure { primary, message } => {
            Ok(PreparedWorktreeReconciliation::Failure(
                prepare_worktree_failure(primary, now, message)?,
            ))
        }
    }
}

fn prepare_worktree_success(
    primary: &Path,
    linked: &[PathBuf],
    excluded: &[PathBuf],
    out_of_scope: &[PathBuf],
) -> Result<PreparedWorktreeSuccess> {
    let persisted_primary = path_to_string(primary)?;
    let canonical_primary = fs::canonicalize(primary)
        .ok()
        .map(|canonical| path_to_string(&canonical))
        .transpose()?;
    let primary = canonical_primary
        .as_deref()
        .unwrap_or(&persisted_primary)
        .to_owned();
    Ok(PreparedWorktreeSuccess {
        primary,
        canonical_primary,
        linked: linked
            .iter()
            .map(|path| path_to_string(path))
            .collect::<Result<_>>()?,
        excluded: excluded
            .iter()
            .map(|path| path_to_string(path))
            .collect::<Result<_>>()?,
        out_of_scope: out_of_scope
            .iter()
            .map(|path| path_to_string(path))
            .collect::<Result<_>>()?,
    })
}

fn prepare_worktree_failure(
    primary: &Path,
    now: SystemTime,
    message: &str,
) -> Result<PreparedWorktreeFailure> {
    Ok(PreparedWorktreeFailure {
        primary: path_to_string(primary)?,
        canonical_primary: fs::canonicalize(primary)
            .ok()
            .map(|canonical| path_to_string(&canonical))
            .transpose()?,
        failed_at: to_epoch(now)?,
        message: message.to_string(),
    })
}

fn replace_linked_worktrees_in_transaction(
    tx: &Transaction<'_>,
    reconciliation: &PreparedWorktreeSuccess,
) -> Result<()> {
    if reconciliation.canonical_primary.is_some() {
        normalize_failed_primary_aliases_for_success(tx, &reconciliation.primary)?;
        rekey_trusted_linked_associations_for_success(tx, &reconciliation.primary)?;
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
        let rows = stmt.query_map([&reconciliation.primary], |row| row.get::<_, String>(0))?;
        collect_rows(rows)?.into_iter().collect::<BTreeSet<_>>()
    };
    tx.execute(
        "
        DELETE FROM linked_worktrees
        WHERE canonical_primary_path=?1
        ",
        [&reconciliation.primary],
    )?;
    for linked_path in &reconciliation.linked {
        tx.execute(
            "
            INSERT OR IGNORE INTO linked_worktrees (
                primary_path,
                linked_path,
                canonical_primary_path
            )
            VALUES (?1, ?2, ?3)
            ",
            params![
                reconciliation.primary,
                linked_path,
                reconciliation.canonical_primary.as_deref()
            ],
        )?;
    }
    for stale_path in previous_linked.difference(&reconciliation.linked) {
        tx.execute("DELETE FROM projects WHERE path=?1", [stale_path])?;
    }
    for excluded_path in &reconciliation.excluded {
        tx.execute("DELETE FROM projects WHERE path=?1", [excluded_path])?;
    }
    for out_of_scope_path in &reconciliation.out_of_scope {
        tx.execute("DELETE FROM projects WHERE path=?1", [out_of_scope_path])?;
    }
    tx.execute(
        "
        DELETE FROM worktree_discovery_failures
        WHERE primary_path=?1 AND canonical_primary_path=?1
        ",
        [&reconciliation.primary],
    )?;
    Ok(())
}

fn mark_worktree_discovery_failed_in_transaction(
    tx: &Transaction<'_>,
    reconciliation: &PreparedWorktreeFailure,
) -> Result<()> {
    tx.execute(
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
            reconciliation.primary,
            reconciliation.failed_at,
            reconciliation.message,
            reconciliation.canonical_primary,
        ],
    )?;
    Ok(())
}

fn reconcile_generation_in_transaction(
    tx: &Transaction<'_>,
    created_at_epoch: i64,
    created_at: SystemTime,
    reconciliation: &GenerationReconciliation,
) -> Result<DiscoveryGeneration> {
    tx.execute(
        "UPDATE discovery_generations SET authority_valid = 0 WHERE authority_valid = 1",
        [],
    )?;
    tx.execute(
        "
        INSERT INTO discovery_generations (
            created_at, policy_hash, boot_session_id, authority_valid
        )
        VALUES (?1, ?2, ?3, 1)
        ",
        params![
            created_at_epoch,
            reconciliation.policy_hash,
            reconciliation.boot_session_id
        ],
    )?;
    let generation_id = tx.last_insert_rowid();

    for origin in &reconciliation.origins {
        tx.execute(
            "
            INSERT INTO discovery_origins (
                generation_id, kind, configured_path, canonical_path, completed, error
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                generation_id,
                origin.kind.as_str(),
                path_to_string(&origin.configured_path)?,
                origin
                    .canonical_path
                    .as_deref()
                    .map(path_to_string)
                    .transpose()?,
                origin.completed,
                origin.error,
            ],
        )?;
        let origin_id = tx.last_insert_rowid();

        for observation in &origin.observations {
            let project_path = path_to_string(&observation.project_path)?;
            let authorized = origin.completed && observation.authorized;
            let blocked_reason = if authorized {
                observation.blocked_reason.clone()
            } else if !origin.completed && observation.blocked_reason.is_none() {
                Some(match origin.error.as_deref() {
                    Some(error) => format!("origin incomplete: {error}"),
                    None => "origin incomplete".to_string(),
                })
            } else {
                observation.blocked_reason.clone()
            };
            tx.execute(
                "
                INSERT INTO project_observations (
                    generation_id, origin_id, project_path,
                    project_device, project_inode, project_mount_id,
                    target_device, target_inode, target_mount_id,
                    observed_at, authorized, blocked_reason, boot_session_id
                )
                VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                    ?8, ?9, ?10, ?11, ?12, ?13
                )
                ",
                params![
                    generation_id,
                    origin_id,
                    project_path,
                    observation.project_identity.device,
                    observation.project_identity.inode,
                    observation.project_identity.mount.0.as_str(),
                    observation
                        .target_identity
                        .as_ref()
                        .map(|identity| identity.device),
                    observation
                        .target_identity
                        .as_ref()
                        .map(|identity| identity.inode),
                    observation
                        .target_identity
                        .as_ref()
                        .map(|identity| identity.mount.0.as_str()),
                    to_epoch(observation.observed_at)?,
                    authorized,
                    blocked_reason,
                    reconciliation.boot_session_id,
                ],
            )?;
            tx.execute(
                "
                INSERT INTO projects (path, discovered_at, last_seen_at)
                VALUES (?1, ?2, ?2)
                ON CONFLICT(path) DO UPDATE SET
                    last_seen_at = MAX(projects.last_seen_at, excluded.last_seen_at)
                ",
                params![project_path, to_epoch(observation.observed_at)?],
            )?;
        }
    }

    Ok(DiscoveryGeneration {
        id: generation_id,
        created_at,
        policy_hash: reconciliation.policy_hash.clone(),
        boot_session_id: reconciliation.boot_session_id.clone(),
    })
}

fn record_error_in_transaction(
    tx: &Transaction<'_>,
    timestamp: i64,
    error: &ErrorRecord,
) -> Result<()> {
    tx.execute(
        "INSERT INTO errors (ts, category, path, message) VALUES (?1, ?2, ?3, ?4)",
        params![timestamp, error.category, error.path, error.message],
    )?;
    Ok(())
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

fn project_observation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectObservation> {
    let target_device = row.get::<_, Option<u64>>(6)?;
    let target_inode = row.get::<_, Option<u64>>(7)?;
    let target_mount = row.get::<_, Option<String>>(8)?;
    let target_identity = match (target_device, target_inode, target_mount) {
        (Some(device), Some(inode), Some(mount)) => Some(FilesystemIdentity {
            device,
            inode,
            mount: MountIdentity(mount),
        }),
        (None, None, None) => None,
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                6,
                "target_device/target_inode/target_mount_id".to_string(),
                rusqlite::types::Type::Null,
            ));
        }
    };
    Ok(ProjectObservation {
        generation_id: row.get(0)?,
        origin_id: row.get(1)?,
        project_path: PathBuf::from(row.get::<_, String>(2)?),
        project_identity: FilesystemIdentity {
            device: row.get(3)?,
            inode: row.get(4)?,
            mount: MountIdentity(row.get(5)?),
        },
        target_identity,
        boot_session_id: row.get(12)?,
        observed_at: from_epoch(row.get(9)?),
        authorized: row.get(10)?,
        blocked_reason: row.get(11)?,
    })
}

fn connection_table_exists(connection: &Connection, table: &str) -> Result<bool> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists)
}

fn connection_column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
            params![table, column],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists)
}

fn generation_is_current(
    connection: &Connection,
    policy_hash: &str,
    generation_id: i64,
) -> Result<bool> {
    let newest = connection
        .query_row(
            "
            SELECT id, policy_hash, authority_valid
            FROM discovery_generations
            ORDER BY id DESC
            LIMIT 1
            ",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            },
        )
        .optional()?;
    Ok(matches!(
        newest,
        Some((id, policy, true)) if id == generation_id && policy == policy_hash
    ))
}

fn insert_review_plan_target(
    tx: &Transaction<'_>,
    plan_id: i64,
    ordinal: usize,
    review: &ProjectReview,
) -> Result<()> {
    let (
        project_device,
        project_inode,
        project_mount_id,
        target_device,
        target_inode,
        target_mount_id,
        boot_session_id,
    ) = match review.reviewed_identity.as_ref() {
        Some(identity) => (
            Some(i64::try_from(identity.project.device)?),
            Some(i64::try_from(identity.project.inode)?),
            Some(identity.project.mount.0.as_str()),
            Some(i64::try_from(identity.target.device)?),
            Some(i64::try_from(identity.target.inode)?),
            Some(identity.target.mount.0.as_str()),
            identity
                .boot_session
                .as_ref()
                .map(|boot_session| boot_session.0.as_str()),
        ),
        None => (None, None, None, None, None, None, None),
    };
    let (decision, skip_reason, skip_newest_age_secs) = decision_parts(&review.decision);
    let reviewed_bytes = i64::try_from(review.target_bytes)
        .context("reviewed target bytes exceed SQLite integer range")?;
    let skip_newest_age_secs = skip_newest_age_secs
        .map(i64::try_from)
        .transpose()
        .context("skip age exceeds SQLite integer range")?;
    tx.execute(
        "
        INSERT INTO review_plan_targets (
            plan_id,
            ordinal,
            project_path,
            canonical_project_path,
            project_class,
            target_path,
            project_device,
            project_inode,
            project_mount_id,
            target_device,
            target_inode,
            target_mount_id,
            review_boot_session_id,
            reviewed_bytes,
            decision,
            skip_reason,
            skip_newest_age_secs
        )
        VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14, ?15, ?16, ?17
        )
        ",
        params![
            plan_id,
            i64::try_from(ordinal)?,
            path_to_string(&review.path)?,
            review
                .canonical_path
                .as_deref()
                .map(path_to_string)
                .transpose()?,
            project_class_label(review.class),
            path_to_string(&review.target_path)?,
            project_device,
            project_inode,
            project_mount_id,
            target_device,
            target_inode,
            target_mount_id,
            boot_session_id,
            reviewed_bytes,
            decision,
            skip_reason,
            skip_newest_age_secs,
        ],
    )?;
    Ok(())
}

fn load_review_plan_from_connection(
    connection: &Connection,
    id: i64,
) -> Result<Option<ReviewPlan>> {
    let header = connection
        .query_row(
            "
            SELECT
                id,
                created_at,
                expires_at,
                policy_hash,
                generation_id,
                coverage_incomplete,
                candidate_bytes
            FROM review_plans
            WHERE id = ?1
            ",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((
        id,
        created_at,
        expires_at,
        policy_hash,
        generation_id,
        coverage_incomplete,
        candidate_bytes,
    )) = header
    else {
        return Ok(None);
    };

    let mut statement = connection.prepare(
        "
        SELECT
            ordinal,
            project_path,
            canonical_project_path,
            project_class,
            target_path,
            project_device,
            project_inode,
            project_mount_id,
            target_device,
            target_inode,
            target_mount_id,
            review_boot_session_id,
            reviewed_bytes,
            decision,
            skip_reason,
            skip_newest_age_secs
        FROM review_plan_targets
        WHERE plan_id = ?1
        ORDER BY ordinal
        ",
    )?;
    let mut rows = statement.query([id])?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next()? {
        let ordinal = usize::try_from(row.get::<_, i64>(0)?)
            .context("review-plan target ordinal is out of range")?;
        let project_path = PathBuf::from(row.get::<_, String>(1)?);
        let canonical_project_path = row.get::<_, Option<String>>(2)?.map(PathBuf::from);
        let project_class = parse_project_class(&row.get::<_, String>(3)?)?;
        let target_path = PathBuf::from(row.get::<_, String>(4)?);
        let project_device = row.get::<_, Option<i64>>(5)?;
        let project_inode = row.get::<_, Option<i64>>(6)?;
        let project_mount_id = row.get::<_, Option<String>>(7)?;
        let target_device = row.get::<_, Option<i64>>(8)?;
        let target_inode = row.get::<_, Option<i64>>(9)?;
        let target_mount_id = row.get::<_, Option<String>>(10)?;
        let boot_session_id = row.get::<_, Option<String>>(11)?;
        let reviewed_identity = parse_reviewed_identity(
            project_device,
            project_inode,
            project_mount_id,
            target_device,
            target_inode,
            target_mount_id,
            boot_session_id,
        )?;
        let reviewed_bytes = u64::try_from(row.get::<_, i64>(12)?)
            .context("reviewed target bytes are out of range")?;
        let decision = parse_decision(
            &row.get::<_, String>(13)?,
            row.get::<_, Option<String>>(14)?.as_deref(),
            row.get::<_, Option<i64>>(15)?,
        )?;
        targets.push(ReviewPlanTarget {
            ordinal,
            review: ProjectReview {
                path: project_path,
                canonical_path: canonical_project_path,
                class: project_class,
                target_path,
                target_bytes: reviewed_bytes,
                reviewed_identity,
                decision,
            },
        });
    }

    Ok(Some(ReviewPlan {
        id,
        created_at: from_epoch(created_at),
        expires_at: from_epoch(expires_at),
        policy_hash,
        generation_id,
        coverage_incomplete,
        candidate_bytes,
        targets,
    }))
}

fn prune_review_plans_in_transaction(
    tx: &Transaction<'_>,
    now_epoch: i64,
    current_authority: Option<(&str, i64)>,
) -> Result<usize> {
    let mut pruned = tx.execute(
        "
        DELETE FROM review_plans
        WHERE expires_at <= ?1
           OR NOT EXISTS(
                SELECT 1
                FROM discovery_generations AS generation
                WHERE generation.id = review_plans.generation_id
                  AND generation.policy_hash = review_plans.policy_hash
                  AND generation.authority_valid = 1
                  AND generation.id = (
                      SELECT newest.id
                      FROM discovery_generations AS newest
                      ORDER BY newest.id DESC
                      LIMIT 1
                  )
           )
        ",
        [now_epoch],
    )?;
    if let Some((policy_hash, generation_id)) = current_authority {
        pruned += tx.execute(
            "
            DELETE FROM review_plans
            WHERE policy_hash <> ?1 OR generation_id <> ?2
            ",
            params![policy_hash, generation_id],
        )?;
    }
    pruned += prune_review_plan_retention_in_transaction(tx)?;
    Ok(pruned)
}

fn prune_review_plan_retention_in_transaction(tx: &Transaction<'_>) -> Result<usize> {
    Ok(tx.execute(
        "
        DELETE FROM review_plans
        WHERE id IN (
            SELECT id
            FROM review_plans
            ORDER BY created_at DESC, id DESC
            LIMIT -1 OFFSET ?1
        )
        ",
        [i64::try_from(REVIEW_PLAN_RETENTION)?],
    )?)
}

fn project_class_label(class: ProjectClass) -> &'static str {
    match class {
        ProjectClass::Workspace => "workspace",
        ProjectClass::ManagedCache => "managed_cache",
        ProjectClass::ContainerStorage => "container_storage",
    }
}

fn parse_project_class(value: &str) -> Result<ProjectClass> {
    match value {
        "workspace" => Ok(ProjectClass::Workspace),
        "managed_cache" => Ok(ProjectClass::ManagedCache),
        "container_storage" => Ok(ProjectClass::ContainerStorage),
        other => bail!("unknown persisted project class {other:?}"),
    }
}

fn decision_parts(decision: &CleanDecision) -> (&'static str, Option<&'static str>, Option<u64>) {
    match decision {
        CleanDecision::Cleanable => ("cleanable", None, None),
        CleanDecision::Skipped(reason) => {
            let (reason, age) = skip_reason_parts(reason);
            ("skipped", Some(reason), age)
        }
    }
}

fn skip_reason_parts(reason: &SkipReason) -> (&'static str, Option<u64>) {
    match reason {
        SkipReason::NoTarget => ("no_target", None),
        SkipReason::ActiveRecentWrite { newest_age_secs } => {
            ("active_recent_write", Some(*newest_age_secs))
        }
        SkipReason::ActiveProcess => ("active_process", None),
        SkipReason::ManagedCache => ("managed_cache", None),
        SkipReason::ContainerStorage => ("container_storage", None),
        SkipReason::ScanError => ("scan_error", None),
        SkipReason::TargetReadError => ("target_read_error", None),
        SkipReason::InvalidManifest => ("invalid_manifest", None),
        SkipReason::ProjectIdentityUnavailable => ("project_identity_unavailable", None),
        SkipReason::TargetIdentityUnavailable => ("target_identity_unavailable", None),
        SkipReason::CrossDeviceTarget => ("cross_device_target", None),
        SkipReason::CrossMountTarget => ("cross_mount_target", None),
        SkipReason::ProjectIdentityChanged => ("project_identity_changed", None),
        SkipReason::TargetIdentityChanged => ("target_identity_changed", None),
        SkipReason::OutOfScope => ("out_of_scope", None),
        SkipReason::Excluded => ("excluded", None),
    }
}

fn parse_decision(
    decision: &str,
    skip_reason: Option<&str>,
    skip_newest_age_secs: Option<i64>,
) -> Result<CleanDecision> {
    match (decision, skip_reason) {
        ("cleanable", None) if skip_newest_age_secs.is_none() => Ok(CleanDecision::Cleanable),
        ("skipped", Some(reason)) => Ok(CleanDecision::Skipped(parse_skip_reason(
            reason,
            skip_newest_age_secs,
        )?)),
        _ => bail!(
            "invalid persisted cleanup decision {decision:?} with skip reason {skip_reason:?}"
        ),
    }
}

fn parse_skip_reason(reason: &str, newest_age_secs: Option<i64>) -> Result<SkipReason> {
    if reason == "active_recent_write" {
        let newest_age_secs = newest_age_secs
            .context("active_recent_write is missing newest age")?
            .try_into()
            .context("active_recent_write newest age is out of range")?;
        return Ok(SkipReason::ActiveRecentWrite { newest_age_secs });
    }
    if newest_age_secs.is_some() {
        bail!("persisted skip reason {reason:?} unexpectedly has a newest age");
    }
    match reason {
        "no_target" => Ok(SkipReason::NoTarget),
        "active_process" => Ok(SkipReason::ActiveProcess),
        "managed_cache" => Ok(SkipReason::ManagedCache),
        "container_storage" => Ok(SkipReason::ContainerStorage),
        "scan_error" => Ok(SkipReason::ScanError),
        "target_read_error" => Ok(SkipReason::TargetReadError),
        "invalid_manifest" => Ok(SkipReason::InvalidManifest),
        "project_identity_unavailable" => Ok(SkipReason::ProjectIdentityUnavailable),
        "target_identity_unavailable" => Ok(SkipReason::TargetIdentityUnavailable),
        "cross_device_target" => Ok(SkipReason::CrossDeviceTarget),
        "cross_mount_target" => Ok(SkipReason::CrossMountTarget),
        "project_identity_changed" => Ok(SkipReason::ProjectIdentityChanged),
        "target_identity_changed" => Ok(SkipReason::TargetIdentityChanged),
        "out_of_scope" => Ok(SkipReason::OutOfScope),
        "excluded" => Ok(SkipReason::Excluded),
        other => bail!("unknown persisted skip reason {other:?}"),
    }
}

fn parse_reviewed_identity(
    project_device: Option<i64>,
    project_inode: Option<i64>,
    project_mount_id: Option<String>,
    target_device: Option<i64>,
    target_inode: Option<i64>,
    target_mount_id: Option<String>,
    boot_session_id: Option<String>,
) -> Result<Option<ReviewedIdentity>> {
    match (
        project_device,
        project_inode,
        project_mount_id,
        target_device,
        target_inode,
        target_mount_id,
    ) {
        (None, None, None, None, None, None) if boot_session_id.is_none() => Ok(None),
        (
            Some(project_device),
            Some(project_inode),
            Some(project_mount_id),
            Some(target_device),
            Some(target_inode),
            Some(target_mount_id),
        ) => Ok(Some(ReviewedIdentity {
            project: FilesystemIdentity {
                device: project_device
                    .try_into()
                    .context("project device is out of range")?,
                inode: project_inode
                    .try_into()
                    .context("project inode is out of range")?,
                mount: MountIdentity(project_mount_id),
            },
            target: FilesystemIdentity {
                device: target_device
                    .try_into()
                    .context("target device is out of range")?,
                inode: target_inode
                    .try_into()
                    .context("target inode is out of range")?,
                mount: MountIdentity(target_mount_id),
            },
            boot_session: boot_session_id.map(crate::identity::BootSessionId),
        })),
        _ => bail!("persisted review identity is incomplete"),
    }
}

fn plan_storage_error(error: impl fmt::Display) -> PlanLoadError {
    PlanLoadError::Storage(error.to_string())
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

fn should_remove_cached_path<F>(path: &Path, is_excluded: &mut F) -> Result<bool>
where
    F: FnMut(&Path) -> bool,
{
    if is_excluded(path) {
        return Ok(true);
    }
    match std::fs::canonicalize(path) {
        Ok(physical) => Ok(is_excluded(&physical)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(err) => {
            Err(err).with_context(|| format!("canonicalize cached project {}", path.display()))
        }
    }
}

fn should_remove_durable_identity<F>(path: &Path, is_excluded: &mut F) -> Result<bool>
where
    F: FnMut(&Path) -> bool,
{
    if is_excluded(path) {
        return Ok(true);
    }
    match std::fs::canonicalize(path) {
        Ok(physical) => Ok(is_excluded(&physical)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => {
            Err(err).with_context(|| format!("canonicalize cached project {}", path.display()))
        }
    }
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
