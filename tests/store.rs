use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use car_go_clean::cleaner::CleanAttemptOutcome;
use car_go_clean::identity::{BootSessionId, FilesystemIdentity, MountIdentity, ReviewedIdentity};
use car_go_clean::safety::{CleanDecision, ProjectClass, ProjectReview, ReviewSummary, SkipReason};
use car_go_clean::store::{
    CleanEvent, DiscoveryOriginKind, ErrorRecord, GenerationReconciliation,
    ObservationReconciliation, OriginReconciliation, PlanLoadError, ScanPublication, Store,
    WorktreeReconciliation, REVIEW_PLAN_RETENTION, REVIEW_PLAN_TTL,
};
fn test_store(path: &Path) -> Store {
    let store = Store::open(path).unwrap();
    store.migrate().unwrap();
    store
}

fn sqlite_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let bytes = row.get::<_, Vec<u8>>(index)?;
    Ok(u64::from_be_bytes(bytes.try_into().map_err(
        |bytes: Vec<u8>| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Blob,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("expected 8 identity bytes, got {}", bytes.len()),
                )
                .into(),
            )
        },
    )?))
}

#[test]
fn open_creates_file_and_migrations_create_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let store = test_store(&db);

    store.ping().unwrap();
    assert!(db.exists());
    assert!(store.table_exists("projects").unwrap());
    assert!(store.table_exists("clean_events").unwrap());
    assert!(store.table_exists("errors").unwrap());
    assert!(store.table_exists("runs").unwrap());
    assert!(store.table_exists("review_status").unwrap());
    assert!(store.table_exists("scheduler_state").unwrap());
    assert!(store.table_exists("linked_worktrees").unwrap());
    assert!(store.table_exists("worktree_discovery_failures").unwrap());
    assert!(store.table_exists("discovery_generations").unwrap());
    assert!(store.table_exists("discovery_origins").unwrap());
    assert!(store.table_exists("project_observations").unwrap());
    assert!(store.table_exists("review_plans").unwrap());
    assert!(store.table_exists("review_plan_targets").unwrap());
}

#[test]
fn migration_repairs_historical_false_success_accounting_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let successful_path = dir.path().join("successful");
    let failed_path = dir.path().join("partial-failure");
    let successful_path = successful_path.to_string_lossy().into_owned();
    let failed_path = failed_path.to_string_lossy().into_owned();
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "
            CREATE TABLE schema_version (version INTEGER NOT NULL);
            INSERT INTO schema_version (version) VALUES (7);
            CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                discovered_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                last_cleaned_at INTEGER
            );
            CREATE TABLE runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                projects_cleaned INTEGER NOT NULL DEFAULT 0,
                bytes_recovered INTEGER NOT NULL DEFAULT 0,
                errors_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE clean_events (
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
            CREATE TABLE errors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                category TEXT NOT NULL,
                path TEXT,
                message TEXT NOT NULL
            );
            ",
        )
        .unwrap();
    connection
        .execute(
            "
            INSERT INTO projects (path, discovered_at, last_seen_at, last_cleaned_at)
            VALUES (?1, 10, 300, 999), (?2, 20, 300, 999)
            ",
            rusqlite::params![successful_path, failed_path],
        )
        .unwrap();
    connection
        .execute_batch(
            "
            INSERT INTO runs (
                id, started_at, finished_at, projects_cleaned, bytes_recovered, errors_count
            ) VALUES
                (1, 100, 300, 2, 1500, 2),
                (2, 400, 500, 4, 444, 5);
            ",
        )
        .unwrap();
    connection
        .execute(
            "
            INSERT INTO clean_events (
                id, run_id, ts, path, bytes_before, bytes_after,
                duration_ms, exit_code, stderr_excerpt
            ) VALUES
                (1, 1, 200, ?1, 1000, 100, 25, 0, ''),
                (2, 1, 210, ?2, 1000, 400, 30, 7, 'partial deletion failed')
            ",
            rusqlite::params![successful_path, failed_path],
        )
        .unwrap();
    connection
        .execute(
            "
            INSERT INTO errors (ts, category, path, message)
            VALUES (50, 'scan', NULL, 'unrelated scan history')
            ",
            [],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&db).unwrap();
    store.migrate().unwrap();

    let inspection = rusqlite::Connection::open(&db).unwrap();
    let runs = {
        let mut statement = inspection
            .prepare(
                "
                SELECT id, started_at, finished_at, projects_cleaned, bytes_recovered, errors_count
                FROM runs
                ORDER BY id
                ",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(
        runs,
        vec![(1, 100, Some(300), 1, 900, 3), (2, 400, Some(500), 0, 0, 5),]
    );
    let schema_version = inspection
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(schema_version, 15);
    drop(inspection);

    let projects = store.all_projects().unwrap();
    let successful_project = projects
        .iter()
        .find(|project| project.path == successful_path)
        .unwrap();
    assert_eq!(
        successful_project.last_cleaned_at,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(200))
    );
    let failed_project = projects
        .iter()
        .find(|project| project.path == failed_path)
        .unwrap();
    assert!(failed_project.last_cleaned_at.is_none());

    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        900
    );
    let ranking = store
        .top_projects_by_bytes(SystemTime::UNIX_EPOCH, 10)
        .unwrap();
    assert_eq!(ranking.len(), 1);
    assert_eq!(ranking[0].path, successful_path);
    assert_eq!(ranking[0].bytes, 900);
    assert_eq!(
        store
            .clean_events_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .len(),
        2
    );

    let errors_after_first_migration = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(errors_after_first_migration.len(), 2);
    assert!(errors_after_first_migration.iter().any(|error| {
        error.category == "scan"
            && error.path.is_none()
            && error.message == "unrelated scan history"
    }));
    let historical_clean_error = errors_after_first_migration
        .iter()
        .find(|error| error.category == "clean")
        .unwrap();
    assert_eq!(
        historical_clean_error.path.as_deref(),
        Some(failed_path.as_str())
    );
    assert!(historical_clean_error.message.contains("exited 7"));
    assert!(historical_clean_error
        .message
        .contains("partial deletion failed"));

    let repaired_runs = runs;
    let repaired_projects = projects;
    store.migrate().unwrap();
    assert_eq!(
        store.errors_since(SystemTime::UNIX_EPOCH).unwrap(),
        errors_after_first_migration
    );
    assert_eq!(store.all_projects().unwrap(), repaired_projects);
    let inspection = rusqlite::Connection::open(&db).unwrap();
    let runs_after_second_migration = {
        let mut statement = inspection
            .prepare(
                "
                SELECT id, started_at, finished_at, projects_cleaned, bytes_recovered, errors_count
                FROM runs
                ORDER BY id
                ",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(runs_after_second_migration, repaired_runs);
}

#[test]
fn linked_worktree_failure_blocks_cached_children_until_success() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("main");
    let linked = root.path().join("feature");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(&linked).unwrap();
    let primary = primary.canonicalize().unwrap();
    let linked = linked.canonicalize().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);

    store
        .replace_linked_worktrees(&primary, std::slice::from_ref(&linked))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&primary, now, "git failed")
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![linked.clone(), primary.clone()]
    );

    store.replace_linked_worktrees(&primary, &[linked]).unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
}

#[test]
fn removing_project_preserves_linked_worktree_provenance() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let primary = Path::new("/workspace/main");
    let linked = PathBuf::from("/workspace/main/.worktrees/feature");
    store
        .replace_linked_worktrees(primary, std::slice::from_ref(&linked))
        .unwrap();
    store.remove_project(primary).unwrap();
    store
        .mark_worktree_discovery_failed(primary, SystemTime::UNIX_EPOCH, "git failed")
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![primary.to_path_buf(), linked]
    );
}

#[test]
fn removing_failed_primary_project_preserves_durable_association_until_success() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let primary = Path::new("/workspace/main");
    let linked = PathBuf::from("/workspace/feature");
    store
        .replace_linked_worktrees(primary, std::slice::from_ref(&linked))
        .unwrap();
    store
        .mark_worktree_discovery_failed(primary, SystemTime::UNIX_EPOCH, "git failed")
        .unwrap();

    store.remove_project(primary).unwrap();

    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![linked, primary.to_path_buf()]
    );
}

#[test]
fn reconcile_excluded_discovery_state_prunes_only_matching_active_state() {
    let root = tempfile::tempdir().unwrap();
    let excluded_root = root.path().join("OrbStack");
    let excluded_primary = excluded_root.join("docker/primary");
    let excluded_linked = excluded_root.join("docker/linked");
    let kept_primary = root.path().join("src/primary");
    let kept_linked = root.path().join("src/linked");
    for path in [
        &excluded_primary,
        &excluded_linked,
        &kept_primary,
        &kept_linked,
    ] {
        fs::create_dir_all(path).unwrap();
    }

    let excluded_root = excluded_root.canonicalize().unwrap();
    let excluded_primary = excluded_primary.canonicalize().unwrap();
    let excluded_linked = excluded_linked.canonicalize().unwrap();
    let kept_primary = kept_primary.canonicalize().unwrap();
    let kept_linked = kept_linked.canonicalize().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));

    for path in [
        &excluded_primary,
        &excluded_linked,
        &kept_primary,
        &kept_linked,
    ] {
        store.upsert_project(path, now).unwrap();
    }
    store
        .replace_linked_worktrees(&excluded_primary, std::slice::from_ref(&excluded_linked))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&excluded_primary, now, "excluded failure")
        .unwrap();
    store
        .replace_linked_worktrees(
            &kept_primary,
            &[kept_linked.clone(), excluded_linked.clone()],
        )
        .unwrap();
    store
        .mark_worktree_discovery_failed(&kept_primary, now, "kept failure")
        .unwrap();
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "worktree_discovery".to_string(),
            path: Some(excluded_primary.to_string_lossy().into_owned()),
            message: "historical error".to_string(),
        })
        .unwrap();
    let run_id = store.start_run(now).unwrap();
    store
        .record_clean_event(&CleanEvent {
            id: 0,
            run_id,
            ts: now,
            path: excluded_primary.to_string_lossy().into_owned(),
            bytes_before: 1024,
            bytes_after: 0,
            duration_ms: 10,
            exit_code: Some(0),
            stderr_excerpt: String::new(),
            outcome: CleanAttemptOutcome::Success,
            measurement_failed: false,
        })
        .unwrap();
    let review_summary = ReviewSummary {
        total_projects: 4,
        cleanable_projects: 2,
        skipped_projects: 2,
        cleanable_bytes: 1024,
        active_recent_write: 0,
        active_process: 1,
        managed_cache: 1,
        container_storage: 0,
        scan_error: 0,
        no_target: 0,
        target_read_error: 0,
    };
    store
        .record_review_status(now, "reconciliation-test", &review_summary)
        .unwrap();
    store
        .record_scheduler_status(
            now,
            now + Duration::from_secs(60),
            now + Duration::from_secs(120),
        )
        .unwrap();
    let review_before = store.last_review_status().unwrap();
    let scheduler_before = store.scheduler_status().unwrap();

    store
        .reconcile_excluded_discovery_state(|path| path.starts_with(&excluded_root))
        .unwrap();

    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| PathBuf::from(project.path))
            .collect::<Vec<_>>(),
        vec![kept_linked.clone(), kept_primary.clone()]
    );
    assert!(!store
        .is_active_worktree_discovery_identity(&excluded_primary)
        .unwrap());
    assert!(!store
        .is_active_worktree_discovery_identity(&excluded_linked)
        .unwrap());
    assert!(store
        .is_active_worktree_discovery_identity(&kept_primary)
        .unwrap());
    assert!(store
        .is_active_worktree_discovery_identity(&kept_linked)
        .unwrap());
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![kept_linked, kept_primary]
    );
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 1);
    assert_eq!(
        store
            .clean_events_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.last_review_status().unwrap(), review_before);
    assert_eq!(store.scheduler_status().unwrap(), scheduler_before);
}

#[cfg(unix)]
#[test]
fn reconcile_excluded_discovery_state_removes_physically_excluded_project() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let excluded_root = root.path().join("OrbStack");
    let canonical_primary = excluded_root.join("primary");
    let legacy_primary = root.path().join("legacy-primary");
    let linked = root.path().join("src/linked");
    fs::create_dir_all(&canonical_primary).unwrap();
    fs::create_dir_all(&linked).unwrap();
    symlink(&canonical_primary, &legacy_primary).unwrap();
    let excluded_root = excluded_root.canonicalize().unwrap();
    let canonical_primary = canonical_primary.canonicalize().unwrap();

    assert!(!legacy_primary.starts_with(&excluded_root));
    assert!(!linked.starts_with(&excluded_root));
    assert!(canonical_primary.starts_with(&excluded_root));

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let store = test_store(&db_path);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    store.upsert_project(&legacy_primary, now).unwrap();
    let inspection = rusqlite::Connection::open(&db_path).unwrap();
    inspection
        .execute(
            "
            INSERT INTO linked_worktrees (
                primary_path,
                linked_path,
                canonical_primary_path
            )
            VALUES (?1, ?2, ?3)
            ",
            rusqlite::params![
                legacy_primary.to_str().unwrap(),
                linked.to_str().unwrap(),
                canonical_primary.to_str().unwrap()
            ],
        )
        .unwrap();
    inspection
        .execute(
            "
            INSERT INTO worktree_discovery_failures (
                primary_path,
                failed_at,
                message,
                canonical_primary_path
            )
            VALUES (?1, ?2, ?3, ?4)
            ",
            rusqlite::params![
                legacy_primary.to_str().unwrap(),
                100,
                "legacy failure",
                canonical_primary.to_str().unwrap()
            ],
        )
        .unwrap();
    drop(inspection);

    store
        .reconcile_excluded_discovery_state(|path| path.starts_with(&excluded_root))
        .unwrap();

    assert!(store.all_projects().unwrap().is_empty());
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn reconcile_excluded_discovery_state_aborts_without_mutation_on_canonicalize_error() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let loop_a = root.path().join("loop-a");
    let loop_b = root.path().join("loop-b");
    symlink(&loop_b, &loop_a).unwrap();
    symlink(&loop_a, &loop_b).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = test_store(&db_dir.path().join("state.db"));
    store.upsert_project(&loop_a, SystemTime::now()).unwrap();

    let error = store
        .reconcile_excluded_discovery_state(|_| false)
        .unwrap_err();
    assert!(error.to_string().contains("canonicalize cached project"));
    assert_eq!(store.all_projects().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn non_utf8_provenance_is_rejected_before_replacing_existing_failure_state() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let primary = Path::new("/workspace/main");
    let linked = PathBuf::from("/workspace/main/.worktrees/saved");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    store
        .replace_linked_worktrees(primary, std::slice::from_ref(&linked))
        .unwrap();
    store
        .mark_worktree_discovery_failed(primary, now, "git failed")
        .unwrap();

    let first = PathBuf::from(OsString::from_vec(
        b"/workspace/main/.worktrees/\xff".to_vec(),
    ));
    let second = PathBuf::from(OsString::from_vec(
        b"/workspace/main/.worktrees/\xfe".to_vec(),
    ));
    assert_eq!(first.to_string_lossy(), second.to_string_lossy());
    assert!(store.replace_linked_worktrees(primary, &[first]).is_err());
    assert!(store.replace_linked_worktrees(primary, &[second]).is_err());
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![primary.to_path_buf(), linked]
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_project_path_is_not_persisted_under_a_lossy_alias() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let path = PathBuf::from(OsString::from_vec(b"/workspace/\xff".to_vec()));

    assert!(store.upsert_project(&path, SystemTime::UNIX_EPOCH).is_err());
    assert!(store.all_projects().unwrap().is_empty());
}

#[test]
fn upsert_project_preserves_discovery_and_updates_last_seen() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(200);

    store.upsert_project("/a", t0).unwrap();
    store.upsert_project("/a", t1).unwrap();

    let projects = store.all_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].path, "/a");
    assert_eq!(projects[0].discovered_at, t0);
    assert_eq!(projects[0].last_seen_at, t1);
}

#[test]
fn replacing_project_path_deduplicates_metadata_and_provenance() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let old = Path::new("/workspace/legacy-alias");
    let new = Path::new("/workspace/canonical");
    let linked = PathBuf::from("/workspace/feature");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
    let t2 = SystemTime::UNIX_EPOCH + Duration::from_secs(300);

    store.upsert_project(old, t0).unwrap();
    store.upsert_project(new, t1).unwrap();
    store.upsert_project(old, t2).unwrap();
    store.mark_project_cleaned(old, t2).unwrap();
    store
        .replace_linked_worktrees(old, std::slice::from_ref(&linked))
        .unwrap();
    store
        .mark_worktree_discovery_failed(old, t2, "git failed")
        .unwrap();

    store.replace_project_path(old, new).unwrap();

    let projects = store.all_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].path, new.to_str().unwrap());
    assert_eq!(projects[0].discovered_at, t0);
    assert_eq!(projects[0].last_seen_at, t2);
    assert_eq!(projects[0].last_cleaned_at, Some(t2));
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![new.to_path_buf(), linked]
    );
}

#[cfg(unix)]
#[test]
fn normalizing_resolvable_orphan_alias_rekeys_provenance_before_success() {
    use std::fs;
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let canonical = root.path().join("canonical");
    let alias = root.path().join("orphan-alias");
    let stale = root.path().join("stale-linked");
    let current = root.path().join("current-linked");
    fs::create_dir_all(&canonical).unwrap();
    symlink(&canonical, &alias).unwrap();
    let canonical = canonical.canonicalize().unwrap();

    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    store
        .replace_linked_worktrees(&alias, std::slice::from_ref(&stale))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&alias, SystemTime::UNIX_EPOCH, "legacy failure")
        .unwrap();

    store.normalize_resolvable_project_aliases().unwrap();
    store
        .replace_linked_worktrees(&canonical, std::slice::from_ref(&current))
        .unwrap();

    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
    store
        .mark_worktree_discovery_failed(&canonical, SystemTime::UNIX_EPOCH, "new failure")
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical, current]
    );
}

#[cfg(unix)]
#[test]
fn retargeted_failed_primary_alias_is_not_claimed_by_success_at_its_new_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    let replacement = root.path().join("replacement");
    let alias = root.path().join("primary-alias");
    let child = root.path().join("original-child");
    for path in [&original, &replacement, &child] {
        fs::create_dir_all(path).unwrap();
    }
    symlink(&original, &alias).unwrap();
    let canonical_original = original.canonicalize().unwrap();
    let canonical_replacement = replacement.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();

    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    store
        .upsert_project(&canonical_child, SystemTime::UNIX_EPOCH)
        .unwrap();
    store
        .replace_linked_worktrees(&alias, std::slice::from_ref(&canonical_child))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&alias, SystemTime::UNIX_EPOCH, "git failed")
        .unwrap();

    fs::remove_file(&alias).unwrap();
    symlink(&replacement, &alias).unwrap();
    store
        .replace_linked_worktrees(&canonical_replacement, &[])
        .unwrap();

    let mut expected = vec![canonical_child, canonical_original];
    expected.sort();
    assert_eq!(store.blocked_worktree_discovery_paths().unwrap(), expected);
}

#[cfg(unix)]
#[test]
fn retargeted_trusted_association_primary_is_not_normalized_to_its_new_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    let replacement = root.path().join("replacement");
    let child = root.path().join("child");
    for path in [&original, &replacement, &child] {
        fs::create_dir_all(path).unwrap();
    }
    let canonical_original = original.canonicalize().unwrap();
    let canonical_replacement = replacement.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();

    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    store
        .replace_linked_worktrees(&canonical_original, std::slice::from_ref(&canonical_child))
        .unwrap();

    fs::remove_dir(&canonical_original).unwrap();
    symlink(&canonical_replacement, &canonical_original).unwrap();
    store.normalize_resolvable_project_aliases().unwrap();

    store
        .replace_linked_worktrees(
            &canonical_replacement,
            std::slice::from_ref(&canonical_child),
        )
        .unwrap();

    fs::remove_file(&canonical_original).unwrap();
    fs::create_dir(&canonical_original).unwrap();
    store
        .mark_worktree_discovery_failed(
            &canonical_original,
            SystemTime::UNIX_EPOCH,
            "original failed",
        )
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_child, canonical_original]
    );
}

#[cfg(unix)]
#[test]
fn project_only_cache_rekey_preserves_trusted_association_provenance() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    let replacement = root.path().join("Library/Caches/replacement");
    let child = root.path().join("child");
    for path in [&original, &replacement, &child] {
        fs::create_dir_all(path).unwrap();
    }
    let canonical_original = original.canonicalize().unwrap();
    let canonical_replacement = replacement.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    store
        .upsert_project(&canonical_original, SystemTime::UNIX_EPOCH)
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_original, std::slice::from_ref(&canonical_child))
        .unwrap();

    fs::remove_dir(&canonical_original).unwrap();
    symlink(&canonical_replacement, &canonical_original).unwrap();
    store
        .replace_cached_project_path(&canonical_original, &canonical_replacement)
        .unwrap();

    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
        vec![canonical_replacement.to_string_lossy().into_owned()]
    );
    fs::remove_file(&canonical_original).unwrap();
    fs::create_dir(&canonical_original).unwrap();
    store
        .mark_worktree_discovery_failed(
            &canonical_original,
            SystemTime::UNIX_EPOCH,
            "original failed",
        )
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_child, canonical_original]
    );
}

#[cfg(unix)]
#[test]
fn trusted_linked_association_path_is_frozen_during_ordinary_normalization() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let primary = root_path.join("primary");
    let linked = root_path.join("linked");
    let unrelated = root_path.join("unrelated");
    for path in [&primary, &linked, &unrelated] {
        fs::create_dir_all(path).unwrap();
    }
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();
    let canonical_unrelated = unrelated.canonicalize().unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let store = test_store(&db_path);
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&canonical_linked))
        .unwrap();

    fs::remove_dir(&canonical_linked).unwrap();
    symlink(&canonical_unrelated, &canonical_linked).unwrap();
    store.normalize_resolvable_project_aliases().unwrap();

    let inspection = rusqlite::Connection::open(&db_path).unwrap();
    let persisted_linked = inspection
        .query_row(
            "SELECT linked_path FROM linked_worktrees WHERE canonical_primary_path=?1",
            [canonical_primary.to_str().unwrap()],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(persisted_linked, canonical_linked.to_str().unwrap());
    drop(inspection);

    fs::remove_file(&canonical_linked).unwrap();
    fs::create_dir(&canonical_linked).unwrap();
    store
        .mark_worktree_discovery_failed(
            &canonical_primary,
            SystemTime::UNIX_EPOCH,
            "primary failed",
        )
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_linked, canonical_primary]
    );
}

#[cfg(unix)]
#[test]
fn migrated_broken_primary_alias_remains_globally_fail_closed_after_primary_success() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let alias = root.path().join("legacy-primary-alias");
    let child = root.path().join("child");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(&child).unwrap();
    symlink(&primary, &alias).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE schema_version (version INTEGER NOT NULL);
        INSERT INTO schema_version (version) VALUES (4);
        CREATE TABLE projects (
            path TEXT PRIMARY KEY,
            discovered_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            last_cleaned_at INTEGER
        );
        CREATE TABLE runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at INTEGER NOT NULL,
            finished_at INTEGER,
            projects_cleaned INTEGER NOT NULL DEFAULT 0,
            bytes_recovered INTEGER NOT NULL DEFAULT 0,
            errors_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE clean_events (
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
        CREATE TABLE errors (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            category TEXT NOT NULL,
            path TEXT,
            message TEXT NOT NULL
        );
        CREATE TABLE linked_worktrees (
            primary_path TEXT NOT NULL,
            linked_path TEXT NOT NULL,
            PRIMARY KEY (primary_path, linked_path)
        );
        CREATE TABLE worktree_discovery_failures (
            primary_path TEXT PRIMARY KEY,
            failed_at INTEGER NOT NULL,
            message TEXT NOT NULL
        );
        ",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO projects (path, discovered_at, last_seen_at) VALUES (?1, 0, 0)",
        [canonical_child.to_str().unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
        rusqlite::params![alias.to_str().unwrap(), canonical_child.to_str().unwrap()],
    )
    .unwrap();
    conn.execute(
        "
        INSERT INTO worktree_discovery_failures (primary_path, failed_at, message)
        VALUES (?1, 0, 'legacy failure')
        ",
        [alias.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    fs::remove_file(&alias).unwrap();

    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, &[])
        .unwrap();

    let blocked = store.blocked_worktree_discovery_paths().unwrap();
    assert!(blocked.contains(&alias));
    assert!(blocked.contains(&canonical_child));
}

#[cfg(unix)]
fn assert_v4_alias_association_blocks_child_after_fresh_primary_failure(retarget: bool) {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let replacement = root.path().join("replacement");
    let alias = root.path().join("legacy-primary-alias");
    let child = root.path().join("child");
    for path in [&primary, &replacement, &child] {
        fs::create_dir_all(path).unwrap();
    }
    symlink(&primary, &alias).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE schema_version (version INTEGER NOT NULL);
        INSERT INTO schema_version (version) VALUES (4);
        CREATE TABLE projects (
            path TEXT PRIMARY KEY,
            discovered_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            last_cleaned_at INTEGER
        );
        CREATE TABLE runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at INTEGER NOT NULL,
            finished_at INTEGER,
            projects_cleaned INTEGER NOT NULL DEFAULT 0,
            bytes_recovered INTEGER NOT NULL DEFAULT 0,
            errors_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE clean_events (
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
        CREATE TABLE errors (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            category TEXT NOT NULL,
            path TEXT,
            message TEXT NOT NULL
        );
        CREATE TABLE linked_worktrees (
            primary_path TEXT NOT NULL,
            linked_path TEXT NOT NULL,
            PRIMARY KEY (primary_path, linked_path)
        );
        CREATE TABLE worktree_discovery_failures (
            primary_path TEXT PRIMARY KEY,
            failed_at INTEGER NOT NULL,
            message TEXT NOT NULL
        );
        ",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO projects (path, discovered_at, last_seen_at) VALUES (?1, 0, 0)",
        [canonical_child.to_str().unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
        rusqlite::params![alias.to_str().unwrap(), canonical_child.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);

    fs::remove_file(&alias).unwrap();
    if retarget {
        symlink(&replacement, &alias).unwrap();
    }

    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::UNIX_EPOCH, "fresh failure")
        .unwrap();

    let blocked = store.blocked_worktree_discovery_paths().unwrap();
    assert!(blocked.contains(&canonical_primary));
    assert!(blocked.contains(&canonical_child));

    store
        .replace_linked_worktrees(&canonical_primary, &[])
        .unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
    store
        .mark_worktree_discovery_failed(
            &canonical_primary,
            SystemTime::UNIX_EPOCH,
            "later fresh failure",
        )
        .unwrap();
    assert!(store
        .blocked_worktree_discovery_paths()
        .unwrap()
        .contains(&canonical_child));
}

#[cfg(unix)]
#[test]
fn migrated_v4_broken_primary_association_blocks_child_after_fresh_failure() {
    assert_v4_alias_association_blocks_child_after_fresh_primary_failure(false);
}

#[cfg(unix)]
#[test]
fn migrated_v4_retargeted_primary_association_blocks_child_after_fresh_failure() {
    assert_v4_alias_association_blocks_child_after_fresh_primary_failure(true);
}

#[cfg(unix)]
#[test]
fn reused_v4_untrusted_primary_spelling_does_not_claim_historical_association() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let original = root_path.join("original");
    let alias = root_path.join("reused-primary");
    let child = root_path.join("historical-child");
    for path in [&original, &child] {
        fs::create_dir_all(path).unwrap();
    }
    symlink(&original, &alias).unwrap();
    let canonical_original = original.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE schema_version (version INTEGER NOT NULL);
        INSERT INTO schema_version (version) VALUES (4);
        CREATE TABLE projects (
            path TEXT PRIMARY KEY,
            discovered_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            last_cleaned_at INTEGER
        );
        CREATE TABLE runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at INTEGER NOT NULL,
            finished_at INTEGER,
            projects_cleaned INTEGER NOT NULL DEFAULT 0,
            bytes_recovered INTEGER NOT NULL DEFAULT 0,
            errors_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE clean_events (
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
        CREATE TABLE errors (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            category TEXT NOT NULL,
            path TEXT,
            message TEXT NOT NULL
        );
        CREATE TABLE linked_worktrees (
            primary_path TEXT NOT NULL,
            linked_path TEXT NOT NULL,
            PRIMARY KEY (primary_path, linked_path)
        );
        CREATE TABLE worktree_discovery_failures (
            primary_path TEXT PRIMARY KEY,
            failed_at INTEGER NOT NULL,
            message TEXT NOT NULL
        );
        ",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO projects (path, discovered_at, last_seen_at) VALUES (?1, 0, 0)",
        [canonical_child.to_str().unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
        rusqlite::params![alias.to_str().unwrap(), canonical_child.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);

    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    fs::remove_file(&alias).unwrap();
    fs::create_dir(&alias).unwrap();
    store.replace_linked_worktrees(&alias, &[]).unwrap();
    let inspection = rusqlite::Connection::open(&db_path).unwrap();
    let untrusted_associations = inspection
        .query_row(
            "
            SELECT COUNT(*)
            FROM linked_worktrees
            WHERE primary_path=?1 AND canonical_primary_path IS NULL
            ",
            [alias.to_str().unwrap()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(untrusted_associations, 1);
    drop(inspection);
    store
        .mark_worktree_discovery_failed(
            &canonical_original,
            SystemTime::UNIX_EPOCH,
            "original failed",
        )
        .unwrap();

    assert!(store
        .blocked_worktree_discovery_paths()
        .unwrap()
        .contains(&canonical_child));
}

#[test]
fn fresh_canonical_primary_failure_clears_on_canonical_success() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let child = root.path().join("child");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(&child).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));

    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&canonical_child))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::UNIX_EPOCH, "git failed")
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_child.clone(), canonical_primary.clone()]
    );

    store
        .replace_linked_worktrees(&canonical_primary, &[canonical_child])
        .unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
}

#[test]
fn records_runs_clean_events_errors_and_stats() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let run_id = store.start_run(t0).unwrap();

    store
        .record_clean_event(&CleanEvent {
            id: 0,
            run_id,
            ts: t0,
            path: "/a".to_string(),
            bytes_before: 1000,
            bytes_after: 100,
            duration_ms: 25,
            exit_code: Some(0),
            stderr_excerpt: String::new(),
            outcome: CleanAttemptOutcome::Success,
            measurement_failed: false,
        })
        .unwrap();
    store
        .record_clean_event(&CleanEvent {
            id: 0,
            run_id,
            ts: t0 + Duration::from_secs(10),
            path: "/b".to_string(),
            bytes_before: 500,
            bytes_after: 0,
            duration_ms: 10,
            exit_code: Some(9),
            stderr_excerpt: String::new(),
            outcome: CleanAttemptOutcome::CargoNonzero,
            measurement_failed: false,
        })
        .unwrap();
    store
        .record_clean_event(&CleanEvent {
            id: 0,
            run_id,
            ts: t0 + Duration::from_secs(20),
            path: "/a".to_string(),
            bytes_before: 100,
            bytes_after: 300,
            duration_ms: 10,
            exit_code: Some(0),
            stderr_excerpt: String::new(),
            outcome: CleanAttemptOutcome::Success,
            measurement_failed: false,
        })
        .unwrap();
    store
        .record_clean_event(&CleanEvent {
            id: 0,
            run_id,
            ts: t0 + Duration::from_secs(30),
            path: "/runner".to_string(),
            bytes_before: 500,
            bytes_after: 500,
            duration_ms: 10,
            exit_code: None,
            stderr_excerpt: String::new(),
            outcome: CleanAttemptOutcome::RunnerFailure,
            measurement_failed: false,
        })
        .unwrap();
    store
        .record_clean_event(&CleanEvent {
            id: 0,
            run_id,
            ts: t0 + Duration::from_secs(40),
            path: "/measurement".to_string(),
            bytes_before: 500,
            bytes_after: 0,
            duration_ms: 10,
            exit_code: Some(0),
            stderr_excerpt: String::new(),
            outcome: CleanAttemptOutcome::Success,
            measurement_failed: true,
        })
        .unwrap();
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: t0,
            category: "scan".to_string(),
            path: Some("/x".to_string()),
            message: "boom".to_string(),
        })
        .unwrap();
    store
        .finish_run(run_id, t0 + Duration::from_secs(60), 1, 900, 1)
        .unwrap();

    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 1);
    assert_eq!(run.bytes_recovered, 900);
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        900
    );
    assert_eq!(
        store
            .total_bytes_recovered(t0 + Duration::from_secs(5))
            .unwrap(),
        0
    );
    let top = store
        .top_projects_by_bytes(SystemTime::UNIX_EPOCH, 10)
        .unwrap();
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].path, "/a");
    assert_eq!(top[0].bytes, 900);
    assert_eq!(
        store.failed_clean_attempts(SystemTime::UNIX_EPOCH).unwrap(),
        3
    );
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 1);
}

#[test]
fn scan_error_paths_since_returns_only_scan_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let now = std::time::SystemTime::now();

    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "scan".to_string(),
            path: Some("/tmp/blocked".to_string()),
            message: "Permission denied".to_string(),
        })
        .unwrap();
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "clean".to_string(),
            path: Some("/tmp/project".to_string()),
            message: "cargo failed".to_string(),
        })
        .unwrap();

    assert_eq!(
        store
            .scan_error_paths_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap(),
        vec![std::path::PathBuf::from("/tmp/blocked")]
    );
}

#[test]
fn scan_coverage_incomplete_since_includes_recent_pathless_errors() {
    let cutoff = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let recent = SystemTime::UNIX_EPOCH + Duration::from_secs(200);

    let scan_store = test_store(&tempfile::tempdir().unwrap().path().join("scan.db"));
    scan_store
        .record_error(&ErrorRecord {
            id: 0,
            ts: cutoff - Duration::from_secs(1),
            category: "scan".to_string(),
            path: None,
            message: "old pathless scan failure".to_string(),
        })
        .unwrap();
    assert!(!scan_store.scan_coverage_incomplete_since(cutoff).unwrap());
    scan_store
        .record_error(&ErrorRecord {
            id: 0,
            ts: recent,
            category: "scan".to_string(),
            path: None,
            message: "recent pathless scan failure".to_string(),
        })
        .unwrap();
    assert!(scan_store.scan_coverage_incomplete_since(cutoff).unwrap());
    assert!(scan_store
        .scan_error_paths_since(cutoff)
        .unwrap()
        .is_empty());

    let discovery_store = test_store(&tempfile::tempdir().unwrap().path().join("discovery.db"));
    discovery_store
        .record_error(&ErrorRecord {
            id: 0,
            ts: recent,
            category: "worktree_discovery".to_string(),
            path: None,
            message: "pathless discovery failure".to_string(),
        })
        .unwrap();
    assert!(discovery_store
        .scan_coverage_incomplete_since(cutoff)
        .unwrap());
}

#[test]
fn resolved_discovery_error_stops_blocking_without_hiding_ordinary_scan_error() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let unrelated = root.path().join("unrelated");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    let primary = primary.canonicalize().unwrap();
    let unrelated = unrelated.canonicalize().unwrap();
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "worktree_discovery".to_string(),
            path: Some(primary.to_string_lossy().into_owned()),
            message: "git failed".to_string(),
        })
        .unwrap();
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "scan".to_string(),
            path: Some(unrelated.to_string_lossy().into_owned()),
            message: "permission denied".to_string(),
        })
        .unwrap();
    store
        .mark_worktree_discovery_failed(&primary, now, "git failed")
        .unwrap();

    assert_eq!(
        store
            .scan_error_paths_since(SystemTime::UNIX_EPOCH)
            .unwrap(),
        vec![primary.clone(), unrelated.clone()]
    );
    assert!(store
        .scan_coverage_incomplete_since(SystemTime::UNIX_EPOCH)
        .unwrap());

    store.replace_linked_worktrees(&primary, &[]).unwrap();
    assert_eq!(
        store
            .scan_error_paths_since(SystemTime::UNIX_EPOCH)
            .unwrap(),
        vec![unrelated]
    );
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 2);
}

#[test]
fn migration_classifies_matching_active_legacy_discovery_diagnostic() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    fs::create_dir_all(&primary).unwrap();
    let primary = primary.canonicalize().unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    {
        let store = test_store(&db_path);
        store
            .record_error(&ErrorRecord {
                id: 0,
                ts: now,
                category: "scan".to_string(),
                path: Some(primary.to_string_lossy().into_owned()),
                message: "git failed".to_string(),
            })
            .unwrap();
        store
            .mark_worktree_discovery_failed(&primary, now, "git failed")
            .unwrap();
    }
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("DELETE FROM schema_version WHERE version >= 7", [])
        .unwrap();
    drop(conn);

    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    store.replace_linked_worktrees(&primary, &[]).unwrap();

    assert!(store
        .scan_error_paths_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .is_empty());
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].category, "worktree_discovery");
}

#[test]
fn records_latest_review_status_snapshot() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);

    store
        .record_review_status(
            t0,
            "projects",
            &ReviewSummary {
                total_projects: 2,
                cleanable_projects: 1,
                skipped_projects: 1,
                cleanable_bytes: 512,
                active_recent_write: 0,
                active_process: 1,
                managed_cache: 0,
                container_storage: 0,
                scan_error: 0,
                no_target: 0,
                target_read_error: 0,
            },
        )
        .unwrap();
    store
        .record_review_status(
            t1,
            "dry-run",
            &ReviewSummary {
                total_projects: 3,
                cleanable_projects: 2,
                skipped_projects: 1,
                cleanable_bytes: 1024,
                active_recent_write: 1,
                active_process: 0,
                managed_cache: 0,
                container_storage: 0,
                scan_error: 0,
                no_target: 0,
                target_read_error: 0,
            },
        )
        .unwrap();

    let status = store.last_review_status().unwrap().unwrap();
    assert_eq!(status.reviewed_at, t1);
    assert_eq!(status.source, "dry-run");
    assert_eq!(status.summary.total_projects, 3);
    assert_eq!(status.summary.cleanable_projects, 2);
    assert_eq!(status.summary.cleanable_bytes, 1024);
}

#[test]
fn records_scheduler_status_snapshot() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let next_clean = now + Duration::from_secs(60);
    let next_scan = now + Duration::from_secs(120);

    store
        .record_scheduler_status(now, next_clean, next_scan)
        .unwrap();

    let status = store.scheduler_status().unwrap().unwrap();
    assert_eq!(status.updated_at, now);
    assert_eq!(status.next_clean_at, next_clean);
    assert_eq!(status.next_scan_at, next_scan);
}

fn create_legacy_database(path: &Path, version: i64) {
    assert!((1..=8).contains(&version));
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(
            "
            CREATE TABLE schema_version (version INTEGER NOT NULL);
            CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                discovered_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                last_cleaned_at INTEGER
            );
            CREATE TABLE runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                projects_cleaned INTEGER NOT NULL DEFAULT 0,
                bytes_recovered INTEGER NOT NULL DEFAULT 0,
                errors_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE clean_events (
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
            CREATE TABLE errors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                category TEXT NOT NULL,
                path TEXT,
                message TEXT NOT NULL
            );
            INSERT INTO projects (path, discovered_at, last_seen_at, last_cleaned_at)
            VALUES
                ('/history/success', 10, 30, 20),
                ('/history/failure', 11, 31, 21);
            INSERT INTO runs (
                id, started_at, finished_at, projects_cleaned, bytes_recovered, errors_count
            ) VALUES (1, 10, 40, 1, 900, 1);
            INSERT INTO clean_events (
                id, run_id, ts, path, bytes_before, bytes_after,
                duration_ms, exit_code, stderr_excerpt
            ) VALUES
                (1, 1, 20, '/history/success', 1000, 100, 5, 0, ''),
                (2, 1, 21, '/history/failure', 1000, 200, 5, 9, 'failed');
            ",
        )
        .unwrap();

    if version >= 2 {
        connection
            .execute_batch(
                "
                CREATE TABLE review_status (
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
                ",
            )
            .unwrap();
    }
    if version >= 3 {
        connection
            .execute_batch(
                "
                CREATE TABLE scheduler_state (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    updated_at INTEGER NOT NULL,
                    next_clean_at INTEGER NOT NULL,
                    next_scan_at INTEGER NOT NULL
                );
                ",
            )
            .unwrap();
    }
    if version >= 4 {
        connection
            .execute_batch(
                "
                CREATE TABLE linked_worktrees (
                    primary_path TEXT NOT NULL,
                    linked_path TEXT NOT NULL,
                    PRIMARY KEY (primary_path, linked_path)
                );
                CREATE INDEX idx_linked_worktrees_linked
                    ON linked_worktrees(linked_path);
                CREATE TABLE worktree_discovery_failures (
                    primary_path TEXT PRIMARY KEY,
                    failed_at INTEGER NOT NULL,
                    message TEXT NOT NULL
                );
                ",
            )
            .unwrap();
    }
    if version >= 5 {
        connection
            .execute(
                "ALTER TABLE worktree_discovery_failures ADD COLUMN canonical_primary_path TEXT",
                [],
            )
            .unwrap();
    }
    if version >= 6 {
        connection
            .execute(
                "ALTER TABLE linked_worktrees ADD COLUMN canonical_primary_path TEXT",
                [],
            )
            .unwrap();
    }
    connection
        .execute("INSERT INTO schema_version(version) VALUES (?1)", [version])
        .unwrap();
}

fn observed_project(
    path: &str,
    project_device: u64,
    project_inode: u64,
    target: Option<(u64, u64)>,
    observed_at: SystemTime,
) -> ObservationReconciliation {
    ObservationReconciliation {
        project_path: PathBuf::from(path),
        project_identity: FilesystemIdentity {
            device: project_device,
            inode: project_inode,
            mount: MountIdentity("store-project-mount".to_string()),
        },
        target_identity: target.map(|(device, inode)| FilesystemIdentity {
            device,
            inode,
            mount: MountIdentity("store-project-mount".to_string()),
        }),
        observed_at,
        authorized: true,
        blocked_reason: None,
    }
}

fn completed_origin(
    configured_path: &str,
    observations: Vec<ObservationReconciliation>,
) -> OriginReconciliation {
    OriginReconciliation {
        kind: DiscoveryOriginKind::ScanRoot,
        configured_path: PathBuf::from(configured_path),
        canonical_path: Some(PathBuf::from(configured_path)),
        completed: true,
        error: None,
        observations,
    }
}

#[test]
fn observation_identities_round_trip_u64_max() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(90);
    let generation = store
        .reconcile_generation(
            observed_at,
            &GenerationReconciliation {
                policy_hash: "policy-u64-observation".to_string(),
                boot_session_id: Some("boot-u64".to_string()),
                origins: vec![completed_origin(
                    "/workspace",
                    vec![observed_project(
                        "/workspace/project",
                        u64::MAX,
                        u64::MAX - 1,
                        Some((u64::MAX - 2, u64::MAX - 3)),
                        observed_at,
                    )],
                )],
            },
        )
        .unwrap();

    let observations = store.authorized_observations(generation.id).unwrap();

    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].project_identity.device, u64::MAX);
    assert_eq!(observations[0].project_identity.inode, u64::MAX - 1);
    assert_eq!(
        observations[0].target_identity.as_ref().unwrap().device,
        u64::MAX - 2
    );
    assert_eq!(
        observations[0].target_identity.as_ref().unwrap().inode,
        u64::MAX - 3
    );
}

#[test]
fn discovery_generation_migrations_preserve_history_without_granting_authority() {
    for version in 1..=8 {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join(format!("v{version}.db"));
        create_legacy_database(&database, version);

        let store = Store::open(&database).unwrap();
        store.migrate().unwrap();

        assert_eq!(
            store.current_generation("policy-after-upgrade").unwrap(),
            None,
            "schema v{version} migration must not manufacture authority"
        );
        assert!(!store
            .has_matching_generation("policy-after-upgrade")
            .unwrap());
        assert!(store.authorized_observations(999).unwrap().is_empty());
        assert_eq!(store.all_projects().unwrap().len(), 2);
        assert_eq!(
            store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
            900
        );
        assert_eq!(store.last_forced_scan_at().unwrap(), None);

        let inspection = rusqlite::Connection::open(&database).unwrap();
        let schema_version = inspection
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(schema_version, 15);
    }
}

#[test]
fn version_nine_observations_lose_authority_without_manufactured_boot_scope() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("v9.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "
            CREATE TABLE schema_version (version INTEGER NOT NULL);
            INSERT INTO schema_version(version) VALUES (9);
            CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                discovered_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                last_cleaned_at INTEGER
            );
            CREATE TABLE runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                projects_cleaned INTEGER NOT NULL DEFAULT 0,
                bytes_recovered INTEGER NOT NULL DEFAULT 0,
                errors_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE clean_events (
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
            CREATE TABLE errors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                category TEXT NOT NULL,
                path TEXT,
                message TEXT NOT NULL
            );
            CREATE TABLE scheduler_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                updated_at INTEGER NOT NULL,
                next_clean_at INTEGER NOT NULL,
                next_scan_at INTEGER NOT NULL,
                last_forced_scan_at INTEGER
            );
            CREATE TABLE discovery_generations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL,
                policy_hash TEXT NOT NULL,
                boot_session_id TEXT
            );
            CREATE TABLE discovery_origins (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                generation_id INTEGER NOT NULL,
                kind TEXT NOT NULL,
                configured_path TEXT NOT NULL,
                canonical_path TEXT,
                completed INTEGER NOT NULL,
                error TEXT
            );
            CREATE TABLE project_observations (
                generation_id INTEGER NOT NULL,
                origin_id INTEGER NOT NULL,
                project_path TEXT NOT NULL,
                project_device INTEGER NOT NULL,
                project_inode INTEGER NOT NULL,
                target_device INTEGER,
                target_inode INTEGER,
                observed_at INTEGER NOT NULL,
                authorized INTEGER NOT NULL,
                blocked_reason TEXT,
                PRIMARY KEY(generation_id, origin_id, project_path)
            );
            INSERT INTO discovery_generations (
                id, created_at, policy_hash, boot_session_id
            ) VALUES (1, 100, 'policy-a', 'boot-a');
            INSERT INTO discovery_origins (
                id, generation_id, kind, configured_path,
                canonical_path, completed, error
            ) VALUES (
                1, 1, 'scan_root', '/workspace', '/workspace', 1, NULL
            );
            INSERT INTO project_observations (
                generation_id, origin_id, project_path,
                project_device, project_inode, target_device, target_inode,
                observed_at, authorized, blocked_reason
            ) VALUES (
                1, 1, '/workspace/project', 7, 11, 7, 12,
                100, 1, NULL
            );
            ",
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&database).unwrap();
    store.migrate().unwrap();

    assert!(store.authorized_observations(1).unwrap().is_empty());
    assert_eq!(store.current_generation("policy-a").unwrap(), None);

    let inspection = rusqlite::Connection::open(&database).unwrap();
    let preserved_generation = inspection
        .query_row(
            "
            SELECT policy_hash, boot_session_id
            FROM discovery_generations
            WHERE id = 1
            ",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .unwrap();
    assert_eq!(
        preserved_generation,
        ("policy-a".to_string(), Some("boot-a".to_string()))
    );
    let preserved_observation = inspection
        .query_row(
            "
            SELECT
                project_device, project_inode, target_device, target_inode,
                authorized, blocked_reason, boot_session_id
            FROM project_observations
            WHERE generation_id = 1
              AND origin_id = 1
              AND project_path = '/workspace/project'
            ",
            [],
            |row| {
                Ok((
                    sqlite_u64(row, 0)?,
                    sqlite_u64(row, 1)?,
                    Some(sqlite_u64(row, 2)?),
                    Some(sqlite_u64(row, 3)?),
                    row.get::<_, bool>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        preserved_observation,
        (
            7,
            11,
            Some(7),
            Some(12),
            false,
            Some("migration requires fresh discovery".to_string()),
            None,
        )
    );
    let schema_version = inspection
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(schema_version, 15);
}

#[test]
fn version_twelve_mount_identity_migration_revokes_observations_and_review_plans() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("v12.db");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
    let (generation_id, plan_id) = {
        let store = test_store(&database);
        let generation = store
            .reconcile_generation(
                now,
                &GenerationReconciliation {
                    policy_hash: "policy-v12".to_string(),
                    boot_session_id: Some("boot-v12".to_string()),
                    origins: vec![completed_origin(
                        "/workspace",
                        vec![observed_project(
                            "/workspace/project",
                            7,
                            11,
                            Some((7, 12)),
                            now,
                        )],
                    )],
                },
            )
            .unwrap();
        let review = persisted_review(
            "/workspace/project",
            Some("/workspace/project"),
            ProjectClass::Workspace,
            100,
            Some((7, 11, 7, 12, Some("boot-v12"))),
            CleanDecision::Cleanable,
        );
        let plan = store
            .create_review_plan(now, "policy-v12", generation.id, false, 100, &[review])
            .unwrap();
        (generation.id, plan.id)
    };
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "
                DROP INDEX idx_discovery_generations_single_valid;
                ALTER TABLE project_observations RENAME TO project_observations_current;
                CREATE TABLE project_observations (
                    generation_id INTEGER NOT NULL,
                    origin_id INTEGER NOT NULL,
                    project_path TEXT NOT NULL,
                    project_device,
                    project_inode,
                    target_device,
                    target_inode,
                    observed_at INTEGER NOT NULL,
                    authorized INTEGER NOT NULL,
                    blocked_reason TEXT,
                    boot_session_id TEXT,
                    PRIMARY KEY(generation_id, origin_id, project_path)
                );
                INSERT INTO project_observations
                SELECT
                    generation_id,
                    origin_id,
                    project_path,
                    project_device,
                    project_inode,
                    target_device,
                    target_inode,
                    observed_at,
                    authorized,
                    blocked_reason,
                    boot_session_id
                FROM project_observations_current;
                DROP TABLE project_observations_current;
                CREATE INDEX idx_project_observations_authorized
                    ON project_observations(generation_id, authorized, project_path);

                ALTER TABLE review_plan_targets RENAME TO review_plan_targets_current;
                CREATE TABLE review_plan_targets (
                    plan_id INTEGER NOT NULL
                        REFERENCES review_plans(id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL,
                    project_path TEXT NOT NULL,
                    canonical_project_path TEXT,
                    project_class TEXT NOT NULL,
                    target_path TEXT NOT NULL,
                    project_device,
                    project_inode,
                    target_device,
                    target_inode,
                    review_boot_session_id TEXT,
                    reviewed_bytes INTEGER NOT NULL,
                    decision TEXT NOT NULL,
                    skip_reason TEXT,
                    skip_newest_age_secs INTEGER,
                    PRIMARY KEY(plan_id, ordinal)
                );
                INSERT INTO review_plan_targets
                SELECT
                    plan_id,
                    ordinal,
                    project_path,
                    canonical_project_path,
                    project_class,
                    target_path,
                    project_device,
                    project_inode,
                    target_device,
                    target_inode,
                    review_boot_session_id,
                    reviewed_bytes,
                    decision,
                    skip_reason,
                    skip_newest_age_secs
                FROM review_plan_targets_current;
                DROP TABLE review_plan_targets_current;
                DELETE FROM schema_version WHERE version >= 13;
                ",
            )
            .unwrap();
    }

    let store = Store::open(&database).unwrap();
    store.migrate().unwrap();

    assert_eq!(store.current_generation("policy-v12").unwrap(), None);
    assert!(store
        .authorized_observations(generation_id)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.load_review_plan(plan_id, now, "policy-v12", generation_id),
        Err(PlanLoadError::Missing)
    );
    assert_eq!(store.all_projects().unwrap().len(), 1);
    let inspection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        inspection
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        15
    );
    assert_eq!(
        inspection
            .query_row(
                "
                SELECT authorized, blocked_reason, project_mount_id, target_mount_id
                FROM project_observations
                WHERE generation_id = ?1
                ",
                [generation_id],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .unwrap(),
        (
            false,
            Some("migration requires fresh mount identity discovery".to_string()),
            None,
            None,
        )
    );
}

#[test]
fn discovery_generation_reconciliation_authorizes_only_completed_origins() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let reconciliation = GenerationReconciliation {
        policy_hash: "policy-a".to_string(),
        boot_session_id: Some("boot-a".to_string()),
        origins: vec![
            completed_origin(
                "/workspace",
                vec![observed_project(
                    "/workspace/allowed",
                    7,
                    11,
                    Some((7, 12)),
                    observed_at,
                )],
            ),
            OriginReconciliation {
                kind: DiscoveryOriginKind::ExplicitProject,
                configured_path: PathBuf::from("/unreadable"),
                canonical_path: Some(PathBuf::from("/unreadable")),
                completed: false,
                error: Some("permission denied".to_string()),
                observations: vec![observed_project(
                    "/unreadable/partial",
                    8,
                    21,
                    Some((8, 22)),
                    observed_at,
                )],
            },
        ],
    };

    let generation = store
        .reconcile_generation(observed_at, &reconciliation)
        .unwrap();

    assert_eq!(generation.policy_hash, "policy-a");
    assert_eq!(generation.boot_session_id.as_deref(), Some("boot-a"));
    assert_eq!(
        store.current_generation("policy-a").unwrap(),
        Some(generation.clone())
    );
    assert!(store.has_matching_generation("policy-a").unwrap());
    assert!(!store.has_matching_generation("policy-b").unwrap());
    let authorized = store.authorized_observations(generation.id).unwrap();
    assert_eq!(authorized.len(), 1);
    assert_eq!(
        authorized[0].project_path,
        PathBuf::from("/workspace/allowed")
    );
    assert_eq!(
        authorized[0].project_identity,
        FilesystemIdentity {
            device: 7,
            inode: 11,
            mount: MountIdentity("store-project-mount".to_string()),
        }
    );
    assert_eq!(
        authorized[0].target_identity,
        Some(FilesystemIdentity {
            device: 7,
            inode: 12,
            mount: MountIdentity("store-project-mount".to_string()),
        })
    );
    assert_eq!(store.all_projects().unwrap().len(), 2);
}

#[test]
fn discovery_generation_authority_is_global_and_never_resurrects_across_policy_changes() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let first_time = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let second_time = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
    let first = store
        .reconcile_generation(
            first_time,
            &GenerationReconciliation {
                policy_hash: "policy-a".to_string(),
                boot_session_id: None,
                origins: vec![completed_origin(
                    "/workspace",
                    vec![
                        observed_project("/workspace/kept", 1, 2, Some((1, 3)), first_time),
                        observed_project("/workspace/removed", 1, 4, Some((1, 5)), first_time),
                    ],
                )],
            },
        )
        .unwrap();
    let second = store
        .reconcile_generation(
            second_time,
            &GenerationReconciliation {
                policy_hash: "policy-a".to_string(),
                boot_session_id: None,
                origins: vec![completed_origin(
                    "/workspace",
                    vec![observed_project(
                        "/workspace/kept",
                        1,
                        2,
                        Some((1, 3)),
                        second_time,
                    )],
                )],
            },
        )
        .unwrap();
    assert!(store.authorized_observations(first.id).unwrap().is_empty());
    assert_eq!(
        store
            .authorized_observations(second.id)
            .unwrap()
            .iter()
            .map(|observation| observation.project_path.clone())
            .collect::<Vec<_>>(),
        vec![PathBuf::from("/workspace/kept")]
    );
    let plan_preserved_until_policy_returns = store
        .create_review_plan(second_time, "policy-a", second.id, false, 0, &[])
        .unwrap();

    let other_policy = store
        .reconcile_generation(
            second_time + Duration::from_secs(1),
            &GenerationReconciliation {
                policy_hash: "policy-b".to_string(),
                boot_session_id: None,
                origins: vec![completed_origin("/other", vec![])],
            },
        )
        .unwrap();

    assert!(store.authorized_observations(second.id).unwrap().is_empty());
    assert_eq!(store.current_generation("policy-a").unwrap(), None);
    assert_eq!(
        store.current_generation("policy-b").unwrap(),
        Some(other_policy.clone())
    );

    let returned_policy = store
        .reconcile_generation(
            second_time + Duration::from_secs(2),
            &GenerationReconciliation {
                policy_hash: "policy-a".to_string(),
                boot_session_id: None,
                origins: vec![completed_origin(
                    "/workspace",
                    vec![observed_project(
                        "/workspace/kept",
                        1,
                        2,
                        Some((1, 3)),
                        second_time,
                    )],
                )],
            },
        )
        .unwrap();

    assert!(returned_policy.id > other_policy.id);
    assert_eq!(
        store.current_generation("policy-a").unwrap(),
        Some(returned_policy.clone())
    );
    assert_eq!(store.current_generation("policy-b").unwrap(), None);
    assert!(store.authorized_observations(first.id).unwrap().is_empty());
    assert_eq!(
        store.load_review_plan(
            plan_preserved_until_policy_returns.id,
            second_time + Duration::from_secs(2),
            "policy-a",
            returned_policy.id,
        ),
        Err(PlanLoadError::GenerationMismatch)
    );
    assert_eq!(store.all_projects().unwrap().len(), 2);
}

#[test]
fn discovery_generation_clock_regression_still_selects_latest_inserted_generation() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let later_wall_time = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
    let earlier_wall_time = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let first = store
        .reconcile_generation(
            later_wall_time,
            &GenerationReconciliation {
                policy_hash: "policy-a".to_string(),
                boot_session_id: None,
                origins: vec![completed_origin(
                    "/workspace",
                    vec![observed_project(
                        "/workspace/removed",
                        1,
                        2,
                        Some((1, 3)),
                        later_wall_time,
                    )],
                )],
            },
        )
        .unwrap();
    let second = store
        .reconcile_generation(
            earlier_wall_time,
            &GenerationReconciliation {
                policy_hash: "policy-a".to_string(),
                boot_session_id: None,
                origins: vec![completed_origin("/workspace", vec![])],
            },
        )
        .unwrap();

    assert!(second.id > first.id);
    assert_eq!(
        store.current_generation("policy-a").unwrap(),
        Some(second.clone())
    );
    assert!(store.authorized_observations(second.id).unwrap().is_empty());
}

#[test]
fn discovery_generation_reconciliation_rolls_back_generation_and_history_on_failure() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let previous = store
        .reconcile_generation(
            observed_at,
            &GenerationReconciliation {
                policy_hash: "policy-before-failure".to_string(),
                boot_session_id: None,
                origins: vec![completed_origin(
                    "/workspace",
                    vec![observed_project(
                        "/workspace/preserved",
                        1,
                        20,
                        Some((1, 21)),
                        observed_at,
                    )],
                )],
            },
        )
        .unwrap();
    let duplicate = observed_project("/workspace/duplicate", 1, 2, Some((1, 3)), observed_at);
    let result = store.reconcile_generation(
        observed_at + Duration::from_secs(1),
        &GenerationReconciliation {
            policy_hash: "policy-failed".to_string(),
            boot_session_id: None,
            origins: vec![completed_origin(
                "/workspace",
                vec![duplicate.clone(), duplicate],
            )],
        },
    );

    assert!(result.is_err());
    assert_eq!(
        store.current_generation("policy-before-failure").unwrap(),
        Some(previous.clone())
    );
    assert_eq!(store.current_generation("policy-failed").unwrap(), None);
    assert_eq!(
        store
            .authorized_observations(previous.id)
            .unwrap()
            .iter()
            .map(|observation| observation.project_path.clone())
            .collect::<Vec<_>>(),
        vec![PathBuf::from("/workspace/preserved")]
    );
    assert_eq!(store.all_projects().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn scan_publication_rolls_back_worktrees_cache_generation_and_diagnostics_at_every_failure_point() {
    use std::os::unix::fs::symlink;

    for (trigger_name, trigger_target) in [
        ("fail_generation", "discovery_generations"),
        ("fail_observation", "project_observations"),
        ("fail_diagnostic", "errors"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("state.db");
        let store = test_store(&database);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

        let primary = directory.path().join("primary");
        let old_linked = directory.path().join("old-linked");
        let new_linked = directory.path().join("new-linked");
        let canonical_project = directory.path().join("canonical-project");
        let project_alias = directory.path().join("project-alias");
        for path in [&primary, &old_linked, &new_linked, &canonical_project] {
            fs::create_dir_all(path).unwrap();
        }
        symlink(&canonical_project, &project_alias).unwrap();
        let primary = primary.canonicalize().unwrap();
        let old_linked = old_linked.canonicalize().unwrap();
        let new_linked = new_linked.canonicalize().unwrap();

        store
            .replace_linked_worktrees(&primary, std::slice::from_ref(&old_linked))
            .unwrap();
        store
            .mark_worktree_discovery_failed(&primary, now, "preserved failure")
            .unwrap();
        store.upsert_project(&project_alias, now).unwrap();
        store
            .record_error(&ErrorRecord {
                id: 0,
                ts: now,
                category: "scan".to_string(),
                path: Some("/preserved".to_string()),
                message: "preserved diagnostic".to_string(),
            })
            .unwrap();
        let previous = store
            .reconcile_generation(
                now,
                &GenerationReconciliation {
                    policy_hash: "policy-before-failure".to_string(),
                    boot_session_id: None,
                    origins: vec![completed_origin(
                        "/workspace",
                        vec![observed_project(
                            "/workspace/preserved",
                            1,
                            2,
                            Some((1, 3)),
                            now,
                        )],
                    )],
                },
            )
            .unwrap();

        let projects_before = store.all_projects().unwrap();
        let blocks_before = store.blocked_worktree_discovery_paths().unwrap();
        let diagnostics_before = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(&format!(
                "
                CREATE TRIGGER {trigger_name}
                BEFORE INSERT ON {trigger_target}
                BEGIN
                    SELECT RAISE(ABORT, 'injected publication failure');
                END;
                "
            ))
            .unwrap();
        drop(connection);

        let result = store.publish_scan(
            now + Duration::from_secs(1),
            &ScanPublication {
                generation: GenerationReconciliation {
                    policy_hash: "policy-failed".to_string(),
                    boot_session_id: None,
                    origins: vec![completed_origin(
                        "/workspace",
                        vec![observed_project(
                            "/workspace/new",
                            1,
                            4,
                            Some((1, 5)),
                            now + Duration::from_secs(1),
                        )],
                    )],
                },
                worktrees: vec![WorktreeReconciliation::Success {
                    primary: primary.clone(),
                    linked: vec![new_linked],
                    excluded: vec![],
                    out_of_scope: vec![],
                }],
                diagnostics: vec![ErrorRecord {
                    id: 0,
                    ts: now + Duration::from_secs(1),
                    category: "scan".to_string(),
                    path: Some("/new".to_string()),
                    message: "must roll back".to_string(),
                }],
            },
        );

        assert!(result.is_err(), "{trigger_name} must abort publication");
        assert_eq!(
            store.current_generation("policy-before-failure").unwrap(),
            Some(previous)
        );
        assert_eq!(store.current_generation("policy-failed").unwrap(), None);
        assert_eq!(
            store.blocked_worktree_discovery_paths().unwrap(),
            blocks_before
        );
        assert_eq!(store.all_projects().unwrap(), projects_before);
        assert_eq!(
            store.errors_since(SystemTime::UNIX_EPOCH).unwrap(),
            diagnostics_before
        );
    }
}

#[test]
fn discovery_generation_reverification_replaces_persisted_identity() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let generation = store
        .reconcile_generation(
            observed_at,
            &GenerationReconciliation {
                policy_hash: "policy-a".to_string(),
                boot_session_id: Some("boot-a".to_string()),
                origins: vec![completed_origin(
                    "/workspace",
                    vec![observed_project(
                        "/workspace/project",
                        1,
                        2,
                        Some((1, 3)),
                        observed_at,
                    )],
                )],
            },
        )
        .unwrap();
    let reverified = ReviewedIdentity {
        project: FilesystemIdentity {
            device: 4,
            inode: 5,
            mount: MountIdentity("reverified-mount".to_string()),
        },
        target: FilesystemIdentity {
            device: 4,
            inode: 6,
            mount: MountIdentity("reverified-mount".to_string()),
        },
        boot_session: Some(BootSessionId("boot-b".to_string())),
    };

    store
        .mark_observation_reverified(generation.id, Path::new("/workspace/project"), &reverified)
        .unwrap();

    let observation = store.authorized_observations(generation.id).unwrap();
    assert_eq!(observation.len(), 1);
    assert_eq!(observation[0].project_identity, reverified.project);
    assert_eq!(
        observation[0].target_identity.as_ref(),
        Some(&reverified.target)
    );
    assert_eq!(observation[0].boot_session_id.as_deref(), Some("boot-b"));
}

#[test]
fn discovery_generation_forced_scan_timestamp_round_trips() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1234);

    assert_eq!(store.last_forced_scan_at().unwrap(), None);
    store.record_forced_scan_at(when).unwrap();
    assert_eq!(store.last_forced_scan_at().unwrap(), Some(when));
}

#[test]
fn current_generation_coverage_remains_incomplete_until_a_complete_generation_replaces_it() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    store
        .reconcile_generation(
            observed_at,
            &GenerationReconciliation {
                policy_hash: "policy-a".to_string(),
                boot_session_id: Some("boot-a".to_string()),
                origins: vec![OriginReconciliation {
                    kind: DiscoveryOriginKind::ScanRoot,
                    configured_path: PathBuf::from("/workspace"),
                    canonical_path: Some(PathBuf::from("/workspace")),
                    completed: false,
                    error: Some("traversal failed".to_string()),
                    observations: vec![],
                }],
            },
        )
        .unwrap();

    assert!(store
        .current_generation_coverage_incomplete("policy-a")
        .unwrap());

    store
        .reconcile_generation(
            observed_at + Duration::from_secs(10),
            &GenerationReconciliation {
                policy_hash: "policy-a".to_string(),
                boot_session_id: Some("boot-a".to_string()),
                origins: vec![completed_origin("/workspace", vec![])],
            },
        )
        .unwrap();

    assert!(!store
        .current_generation_coverage_incomplete("policy-a")
        .unwrap());
    assert!(store
        .current_generation_coverage_incomplete("missing-policy")
        .unwrap());
}

#[test]
fn invalid_latest_generation_does_not_fall_back_to_older_complete_authority() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.db");
    let store = test_store(&database);
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    for offset in [0, 10] {
        store
            .reconcile_generation(
                observed_at + Duration::from_secs(offset),
                &GenerationReconciliation {
                    policy_hash: "policy-a".to_string(),
                    boot_session_id: Some("boot-a".to_string()),
                    origins: vec![completed_origin("/workspace", vec![])],
                },
            )
            .unwrap();
    }
    drop(store);
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute(
            "
            UPDATE discovery_generations
            SET authority_valid = 0
            WHERE id = (SELECT MAX(id) FROM discovery_generations)
            ",
            [],
        )
        .unwrap();
    let store = Store::open(&database).unwrap();

    assert_eq!(store.current_generation("policy-a").unwrap(), None);
    assert!(store
        .current_generation_coverage_incomplete("policy-a")
        .unwrap());
}

fn review_generation(
    store: &Store,
    created_at: SystemTime,
    policy_hash: &str,
) -> car_go_clean::store::DiscoveryGeneration {
    store
        .reconcile_generation(
            created_at,
            &GenerationReconciliation {
                policy_hash: policy_hash.to_string(),
                boot_session_id: Some("generation-boot".to_string()),
                origins: vec![completed_origin("/review-root", Vec::new())],
            },
        )
        .unwrap()
}

fn persisted_review(
    project: &str,
    canonical_project: Option<&str>,
    class: ProjectClass,
    target_bytes: u64,
    identity: Option<(u64, u64, u64, u64, Option<&str>)>,
    decision: CleanDecision,
) -> ProjectReview {
    ProjectReview {
        path: PathBuf::from(project),
        canonical_path: canonical_project.map(PathBuf::from),
        class,
        target_path: PathBuf::from(format!("{project}/target")),
        target_bytes,
        reviewed_identity: identity.map(
            |(project_device, project_inode, target_device, target_inode, boot_session)| {
                ReviewedIdentity {
                    project: FilesystemIdentity {
                        device: project_device,
                        inode: project_inode,
                        mount: MountIdentity("review-plan-mount".to_string()),
                    },
                    target: FilesystemIdentity {
                        device: target_device,
                        inode: target_inode,
                        mount: MountIdentity("review-plan-mount".to_string()),
                    },
                    boot_session: boot_session.map(|value| BootSessionId(value.to_string())),
                }
            },
        ),
        decision,
    }
}

#[test]
fn review_plan_round_trips_order_and_complete_review_authority() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let normalized_now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    let now = normalized_now + Duration::from_millis(123);
    let generation = review_generation(&store, now, "policy-a");
    let reviews = vec![
        persisted_review(
            "/logical/first",
            Some("/physical/first"),
            ProjectClass::Workspace,
            4_096,
            Some((11, 12, 11, 13, Some("review-boot"))),
            CleanDecision::Cleanable,
        ),
        persisted_review(
            "/logical/second",
            Some("/physical/second"),
            ProjectClass::ManagedCache,
            8_192,
            Some((21, 22, 21, 23, None)),
            CleanDecision::Skipped(SkipReason::ActiveRecentWrite {
                newest_age_secs: 17,
            }),
        ),
        persisted_review(
            "/logical/unreadable",
            None,
            ProjectClass::ContainerStorage,
            0,
            None,
            CleanDecision::Skipped(SkipReason::ProjectIdentityUnavailable),
        ),
    ];

    let created = store
        .create_review_plan(now, "policy-a", generation.id, true, 4_096, &reviews)
        .unwrap();

    assert_eq!(created.created_at, normalized_now);
    assert_eq!(created.expires_at, normalized_now + REVIEW_PLAN_TTL);
    assert_eq!(created.policy_hash, "policy-a");
    assert_eq!(created.generation_id, generation.id);
    assert!(created.coverage_incomplete);
    assert_eq!(created.candidate_bytes, 4_096);
    assert_eq!(
        created
            .targets
            .iter()
            .map(|target| target.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        created
            .targets
            .iter()
            .map(|target| target.review.clone())
            .collect::<Vec<_>>(),
        reviews
    );

    let loaded = store
        .load_review_plan(created.id, now, "policy-a", generation.id)
        .unwrap();
    assert_eq!(loaded, created);
}

#[test]
fn review_plan_identities_round_trip_u64_max() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let store = test_store(&database);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(11_000);
    let generation = review_generation(&store, now, "policy-u64-plan");
    let review = persisted_review(
        "/logical/max",
        Some("/physical/max"),
        ProjectClass::Workspace,
        4_096,
        Some((
            u64::MAX,
            u64::MAX - 1,
            u64::MAX - 2,
            u64::MAX - 3,
            Some("boot-u64"),
        )),
        CleanDecision::Cleanable,
    );

    let created = store
        .create_review_plan(
            now,
            "policy-u64-plan",
            generation.id,
            false,
            4_096,
            std::slice::from_ref(&review),
        )
        .unwrap();
    let loaded = store
        .load_review_plan(created.id, now, "policy-u64-plan", generation.id)
        .unwrap();

    assert_eq!(loaded.targets[0].review, review);
    let inspection = rusqlite::Connection::open(database).unwrap();
    let storage = inspection
        .query_row(
            "
            SELECT
                typeof(project_device), length(project_device),
                typeof(project_inode), length(project_inode),
                typeof(target_device), length(target_device),
                typeof(target_inode), length(target_inode)
            FROM review_plan_targets
            WHERE plan_id = ?1
            ",
            [created.id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        storage,
        (
            "blob".to_string(),
            8,
            "blob".to_string(),
            8,
            "blob".to_string(),
            8,
            "blob".to_string(),
            8,
        )
    );
}

#[test]
fn version_thirteen_signed_identities_migrate_losslessly_without_revoking_authority() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let now = SystemTime::now();
    let (generation, plan, review) = {
        let store = test_store(&database);
        let generation = store
            .reconcile_generation(
                now,
                &GenerationReconciliation {
                    policy_hash: "policy-v13".to_string(),
                    boot_session_id: Some("boot-v13".to_string()),
                    origins: vec![completed_origin(
                        "/workspace",
                        vec![observed_project(
                            "/workspace/project",
                            7,
                            11,
                            Some((7, 12)),
                            now,
                        )],
                    )],
                },
            )
            .unwrap();
        let review = persisted_review(
            "/workspace/project",
            Some("/workspace/project"),
            ProjectClass::Workspace,
            100,
            Some((7, 11, 7, 12, Some("boot-v13"))),
            CleanDecision::Cleanable,
        );
        let plan = store
            .create_review_plan(
                now,
                "policy-v13",
                generation.id,
                false,
                100,
                std::slice::from_ref(&review),
            )
            .unwrap();
        (generation, plan, review)
    };
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "
                PRAGMA ignore_check_constraints = ON;
                UPDATE project_observations
                SET project_device = 7,
                    project_inode = 11,
                    target_device = 7,
                    target_inode = 12;
                UPDATE review_plan_targets
                SET project_device = 7,
                    project_inode = 11,
                    target_device = 7,
                    target_inode = 12;
                PRAGMA ignore_check_constraints = OFF;
                DELETE FROM schema_version WHERE version >= 14;
                ",
            )
            .unwrap();
    }

    let store = Store::open(&database).unwrap();
    store.migrate().unwrap();

    let current = store.current_generation("policy-v13").unwrap().unwrap();
    assert_eq!(current.id, generation.id);
    assert_eq!(current.policy_hash, generation.policy_hash);
    assert_eq!(current.boot_session_id, generation.boot_session_id);
    let observations = store.authorized_observations(generation.id).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].project_identity.device, 7);
    assert_eq!(observations[0].project_identity.inode, 11);
    assert_eq!(observations[0].target_identity.as_ref().unwrap().device, 7);
    assert_eq!(observations[0].target_identity.as_ref().unwrap().inode, 12);
    let loaded = store
        .load_review_plan(plan.id, now, "policy-v13", generation.id)
        .unwrap();
    assert_eq!(loaded.targets[0].review, review);

    let inspection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        inspection
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        15
    );
    for table in ["project_observations", "review_plan_targets"] {
        let storage = inspection
            .query_row(
                &format!(
                    "
                    SELECT
                        typeof(project_device), length(project_device),
                        typeof(project_inode), length(project_inode),
                        typeof(target_device), length(target_device),
                        typeof(target_inode), length(target_inode)
                    FROM {table}
                    LIMIT 1
                    "
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            storage,
            (
                "blob".to_string(),
                8,
                "blob".to_string(),
                8,
                "blob".to_string(),
                8,
                "blob".to_string(),
                8,
            ),
            "{table}"
        );
    }
}

fn create_permissive_v13_identity_fixture(database: &Path) {
    let now = SystemTime::now();
    {
        let store = test_store(database);
        let generation = store
            .reconcile_generation(
                now,
                &GenerationReconciliation {
                    policy_hash: "policy-malformed-v13".to_string(),
                    boot_session_id: Some("generation-boot".to_string()),
                    origins: vec![completed_origin(
                        "/workspace",
                        vec![observed_project(
                            "/workspace/project",
                            7,
                            11,
                            Some((7, 12)),
                            now,
                        )],
                    )],
                },
            )
            .unwrap();
        let review = persisted_review(
            "/workspace/project",
            Some("/workspace/project"),
            ProjectClass::Workspace,
            100,
            Some((7, 11, 7, 12, Some("review-boot"))),
            CleanDecision::Cleanable,
        );
        store
            .create_review_plan(
                now,
                "policy-malformed-v13",
                generation.id,
                false,
                100,
                std::slice::from_ref(&review),
            )
            .unwrap();
    }

    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute_batch(
            "
            ALTER TABLE project_observations RENAME TO project_observations_strict;
            CREATE TABLE project_observations (
                generation_id INTEGER,
                origin_id INTEGER,
                project_path TEXT,
                project_device,
                project_inode,
                project_mount_id,
                target_device,
                target_inode,
                target_mount_id,
                observed_at INTEGER,
                authorized INTEGER,
                blocked_reason TEXT,
                boot_session_id TEXT,
                PRIMARY KEY(generation_id, origin_id, project_path)
            );
            INSERT INTO project_observations
            SELECT * FROM project_observations_strict;
            DROP TABLE project_observations_strict;
            CREATE INDEX idx_project_observations_authorized
                ON project_observations(generation_id, authorized, project_path);

            ALTER TABLE review_plan_targets RENAME TO review_plan_targets_strict;
            CREATE TABLE review_plan_targets (
                plan_id INTEGER,
                ordinal INTEGER,
                project_path TEXT,
                canonical_project_path TEXT,
                project_class TEXT,
                target_path TEXT,
                project_device,
                project_inode,
                project_mount_id,
                target_device,
                target_inode,
                target_mount_id,
                review_boot_session_id TEXT,
                reviewed_bytes INTEGER,
                decision TEXT,
                skip_reason TEXT,
                skip_newest_age_secs INTEGER,
                PRIMARY KEY(plan_id, ordinal)
            );
            INSERT INTO review_plan_targets
            SELECT * FROM review_plan_targets_strict;
            DROP TABLE review_plan_targets_strict;

            DELETE FROM schema_version WHERE version >= 14;
            ",
        )
        .unwrap();
}

fn identity_migration_snapshot(
    connection: &rusqlite::Connection,
) -> (Vec<(String, String)>, Vec<String>, i64) {
    let schema = {
        let mut statement = connection
            .prepare(
                "
                SELECT name, sql
                FROM sqlite_master
                WHERE name IN (
                    'project_observations',
                    'review_plan_targets',
                    'clean_events',
                    'idx_project_observations_authorized',
                    'idx_clean_events_ts'
                )
                ORDER BY name
                ",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let rows = {
        let mut statement = connection
            .prepare(
                "
                SELECT
                    'observation|' || generation_id || '|' || origin_id || '|' ||
                    project_path || '|' ||
                    typeof(project_device) || ':' || hex(project_device) || '|' ||
                    typeof(project_inode) || ':' || hex(project_inode) || '|' ||
                    typeof(project_mount_id) || ':' || hex(project_mount_id) || '|' ||
                    typeof(target_device) || ':' || hex(target_device) || '|' ||
                    typeof(target_inode) || ':' || hex(target_inode) || '|' ||
                    typeof(target_mount_id) || ':' || hex(target_mount_id)
                FROM project_observations
                UNION ALL
                SELECT
                    'review|' || plan_id || '|' || ordinal || '|' ||
                    project_path || '|' ||
                    typeof(project_device) || ':' || hex(project_device) || '|' ||
                    typeof(project_inode) || ':' || hex(project_inode) || '|' ||
                    typeof(project_mount_id) || ':' || hex(project_mount_id) || '|' ||
                    typeof(target_device) || ':' || hex(target_device) || '|' ||
                    typeof(target_inode) || ':' || hex(target_inode) || '|' ||
                    typeof(target_mount_id) || ':' || hex(target_mount_id)
                FROM review_plan_targets
                ORDER BY 1
                ",
            )
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let version = connection
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .unwrap();
    (schema, rows, version)
}

#[test]
fn version_thirteen_malformed_identity_groups_fail_atomically_and_idempotently() {
    for (name, mutation, expected_message) in [
        (
            "project-incomplete-target-triple",
            "UPDATE project_observations SET target_mount_id = NULL",
            "project_observations target identity",
        ),
        (
            "project-missing-required-mount",
            "UPDATE project_observations SET project_mount_id = NULL",
            "project_observations project identity",
        ),
        (
            "review-incomplete-project-triple",
            "UPDATE review_plan_targets SET project_mount_id = NULL",
            "review_plan_targets identity",
        ),
        (
            "negative-integer",
            "UPDATE project_observations SET project_device = -1",
            "project_device",
        ),
        (
            "required-null",
            "UPDATE project_observations SET project_inode = NULL",
            "project_inode",
        ),
        (
            "short-blob",
            "UPDATE project_observations SET project_device = X'00010203040506'",
            "project_device",
        ),
        (
            "long-blob",
            "UPDATE project_observations SET project_device = X'000102030405060708'",
            "project_device",
        ),
        (
            "overflow-decimal",
            "UPDATE project_observations SET project_device = '18446744073709551616'",
            "project_device",
        ),
        (
            "malformed-decimal",
            "UPDATE project_observations SET project_device = 'not-a-number'",
            "project_device",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join(format!("{name}.db"));
        create_permissive_v13_identity_fixture(&database);
        {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection.execute(mutation, []).unwrap();
        }

        let before = {
            let connection = rusqlite::Connection::open(&database).unwrap();
            identity_migration_snapshot(&connection)
        };
        assert_eq!(before.2, 13, "{name}");

        let store = Store::open(&database).unwrap();
        let first_error = store.migrate().unwrap_err().to_string();
        assert!(
            first_error.contains(expected_message),
            "{name}: {first_error}"
        );
        let second_error = store.migrate().unwrap_err().to_string();
        assert_eq!(second_error, first_error, "{name}");
        drop(store);

        let inspection = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(identity_migration_snapshot(&inspection), before, "{name}");
        for temporary_table in [
            "project_observations_v14",
            "review_plan_targets_v14",
            "clean_events_v14",
            "clean_events_v15",
        ] {
            assert_eq!(
                inspection
                    .query_row(
                        "
                        SELECT COUNT(*)
                        FROM sqlite_master
                        WHERE type = 'table' AND name = ?1
                        ",
                        [temporary_table],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "{name}: {temporary_table}"
            );
        }
    }
}

#[test]
fn version_thirteen_clean_events_gain_deterministic_typed_outcomes() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(600);
    {
        let store = test_store(&database);
        let run_id = store.start_run(started_at).unwrap();
        for (offset, path, exit_code, outcome, measurement_failed) in [
            (1, "/success", Some(0), CleanAttemptOutcome::Success, false),
            (
                2,
                "/cargo-nonzero",
                Some(7),
                CleanAttemptOutcome::CargoNonzero,
                false,
            ),
            (
                3,
                "/measurement",
                Some(0),
                CleanAttemptOutcome::Success,
                true,
            ),
        ] {
            store
                .record_clean_event(&CleanEvent {
                    id: 0,
                    run_id,
                    ts: started_at + Duration::from_secs(offset),
                    path: path.to_string(),
                    bytes_before: 1_000,
                    bytes_after: 100,
                    duration_ms: 5,
                    exit_code,
                    stderr_excerpt: String::new(),
                    outcome,
                    measurement_failed,
                })
                .unwrap();
        }
        store
            .record_error(&ErrorRecord {
                id: 0,
                ts: started_at + Duration::from_secs(3),
                category: "clean".to_string(),
                path: Some("/measurement".to_string()),
                message: "measure target after cargo clean: injected read failure".to_string(),
            })
            .unwrap();
    }
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE clean_events_v13 (
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
                INSERT INTO clean_events_v13 (
                    id,
                    run_id,
                    ts,
                    path,
                    bytes_before,
                    bytes_after,
                    duration_ms,
                    exit_code,
                    stderr_excerpt
                )
                SELECT
                    id,
                    run_id,
                    ts,
                    path,
                    bytes_before,
                    bytes_after,
                    duration_ms,
                    exit_code,
                    stderr_excerpt
                FROM clean_events;
                DROP TABLE clean_events;
                ALTER TABLE clean_events_v13 RENAME TO clean_events;
                CREATE INDEX idx_clean_events_ts ON clean_events(ts);
                DELETE FROM schema_version WHERE version >= 14;
                ",
            )
            .unwrap();
    }

    let store = Store::open(&database).unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();

    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(
        events.iter().map(|event| event.outcome).collect::<Vec<_>>(),
        vec![
            CleanAttemptOutcome::Success,
            CleanAttemptOutcome::CargoNonzero,
            CleanAttemptOutcome::Success,
        ]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.measurement_failed)
            .collect::<Vec<_>>(),
        vec![false, false, true]
    );
    assert_eq!(
        store.failed_clean_attempts(SystemTime::UNIX_EPOCH).unwrap(),
        2
    );
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        900
    );
    let inspection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        inspection
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        15
    );
}

fn create_authentic_v14_combined_failure(database: &Path) -> (i64, SystemTime) {
    let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(650);
    let run_id = {
        let store = test_store(database);
        let run_id = store.start_run(timestamp).unwrap();
        store
            .record_clean_event(&CleanEvent {
                id: 0,
                run_id,
                ts: timestamp,
                path: "/combined-failure".to_string(),
                bytes_before: 1_000,
                bytes_after: 1_000,
                duration_ms: 5,
                exit_code: Some(7),
                stderr_excerpt: "cargo failed".to_string(),
                outcome: CleanAttemptOutcome::CargoNonzero,
                measurement_failed: true,
            })
            .unwrap();
        store
            .record_error(&ErrorRecord {
                id: 0,
                ts: timestamp,
                category: "clean".to_string(),
                path: Some("/combined-failure".to_string()),
                message: "measure target after cargo clean: injected read failure".to_string(),
            })
            .unwrap();
        store
            .finish_run(run_id, timestamp + Duration::from_secs(1), 0, 0, 1)
            .unwrap();
        run_id
    };
    {
        let connection = rusqlite::Connection::open(database).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE clean_events_v14_legacy (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id INTEGER NOT NULL REFERENCES runs(id),
                    ts INTEGER NOT NULL,
                    path TEXT NOT NULL,
                    bytes_before INTEGER NOT NULL,
                    bytes_after INTEGER NOT NULL,
                    duration_ms INTEGER NOT NULL DEFAULT 0,
                    exit_code INTEGER,
                    stderr_excerpt TEXT NOT NULL DEFAULT '',
                    attempt_outcome TEXT NOT NULL
                        CHECK(attempt_outcome IN (
                            'success',
                            'cargo_nonzero',
                            'runner_failure',
                            'measurement_failure'
                        )),
                    CHECK(
                        (attempt_outcome = 'success' AND exit_code = 0)
                        OR (attempt_outcome = 'cargo_nonzero'
                            AND exit_code IS NOT NULL AND exit_code <> 0)
                        OR (attempt_outcome = 'runner_failure' AND exit_code IS NULL)
                        OR (attempt_outcome = 'measurement_failure'
                            AND exit_code IS NOT NULL)
                    )
                );
                INSERT INTO clean_events_v14_legacy (
                    id,
                    run_id,
                    ts,
                    path,
                    bytes_before,
                    bytes_after,
                    duration_ms,
                    exit_code,
                    stderr_excerpt,
                    attempt_outcome
                )
                SELECT
                    id,
                    run_id,
                    ts,
                    path,
                    bytes_before,
                    bytes_after,
                    duration_ms,
                    exit_code,
                    stderr_excerpt,
                    'measurement_failure'
                FROM clean_events;
                DROP TABLE clean_events;
                ALTER TABLE clean_events_v14_legacy RENAME TO clean_events;
                CREATE INDEX idx_clean_events_ts ON clean_events(ts);
                DELETE FROM schema_version WHERE version >= 15;
                ",
            )
            .unwrap();
    }
    (run_id, timestamp)
}

#[test]
fn version_fourteen_combined_failure_repairs_the_missing_cargo_audit_and_run_count_once() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let (run_id, timestamp) = create_authentic_v14_combined_failure(&database);
    {
        let inspection = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            inspection
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            14
        );
        assert_eq!(
            inspection
                .query_row(
                    "
                    SELECT attempt_outcome, exit_code
                    FROM clean_events
                    WHERE run_id = ?1
                    ",
                    [run_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?)),
                )
                .unwrap(),
            ("measurement_failure".to_string(), 7)
        );
        assert_eq!(
            inspection
                .query_row(
                    "SELECT errors_count FROM runs WHERE id = ?1",
                    [run_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            inspection
                .query_row("SELECT COUNT(*) FROM errors", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
    }

    let store = Store::open(&database).unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();

    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].exit_code, Some(7));
    assert_eq!(events[0].outcome, CleanAttemptOutcome::CargoNonzero);
    assert!(events[0].measurement_failed);
    assert_eq!(
        store.failed_clean_attempts(SystemTime::UNIX_EPOCH).unwrap(),
        1
    );
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        0
    );
    let run = store.last_run().unwrap();
    assert_eq!(run.id, run_id);
    assert_eq!(run.errors_count, 2);
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(errors.len(), 2);
    assert_eq!(
        errors
            .iter()
            .filter(|error| {
                error.ts == timestamp
                    && error.category == "clean"
                    && error.path.as_deref() == Some("/combined-failure")
                    && error.message == "cargo clean exited 7: cargo failed"
            })
            .count(),
        1
    );
    assert_eq!(
        errors
            .iter()
            .filter(|error| {
                error.ts == timestamp
                    && error.category == "clean"
                    && error.path.as_deref() == Some("/combined-failure")
                    && error
                        .message
                        .starts_with("measure target after cargo clean:")
            })
            .count(),
        1
    );

    store.migrate().unwrap();
    assert_eq!(store.last_run().unwrap().errors_count, 2);
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 2);
}

#[test]
fn version_fourteen_combined_failure_preserves_existing_cargo_audit_and_run_count() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let (run_id, _) = create_authentic_v14_combined_failure(&database);
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "
                INSERT INTO errors (ts, category, path, message)
                VALUES (?1, 'clean', '/combined-failure',
                        'cargo clean exited 7: cargo failed')
                ",
                [650_i64],
            )
            .unwrap();
        connection
            .execute("UPDATE runs SET errors_count = 2 WHERE id = ?1", [run_id])
            .unwrap();
    }

    let store = Store::open(&database).unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();

    assert_eq!(store.last_run().unwrap().errors_count, 2);
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(errors.len(), 2);
    assert_eq!(
        errors
            .iter()
            .filter(|error| error.message == "cargo clean exited 7: cargo failed")
            .count(),
        1
    );
    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, CleanAttemptOutcome::CargoNonzero);
    assert!(events[0].measurement_failed);
}

#[test]
fn version_fourteen_combined_failure_repair_rolls_back_as_one_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let (run_id, _) = create_authentic_v14_combined_failure(&database);
    let clean_events_schema = {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "
                CREATE TRIGGER reject_run_count_repair
                BEFORE UPDATE OF errors_count ON runs
                WHEN NEW.errors_count = 2
                BEGIN
                    SELECT RAISE(ABORT, 'injected run count repair failure');
                END;
                ",
            )
            .unwrap();
        connection
            .query_row(
                "
                SELECT sql
                FROM sqlite_master
                WHERE type = 'table' AND name = 'clean_events'
                ",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
    };

    let store = Store::open(&database).unwrap();
    let first_error = store.migrate().unwrap_err().to_string();
    assert!(
        first_error.contains("injected run count repair failure"),
        "{first_error}"
    );
    let second_error = store.migrate().unwrap_err().to_string();
    assert_eq!(second_error, first_error);
    drop(store);

    let inspection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        inspection
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        14
    );
    assert_eq!(
        inspection
            .query_row(
                "
                SELECT sql
                FROM sqlite_master
                WHERE type = 'table' AND name = 'clean_events'
                ",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        clean_events_schema
    );
    assert_eq!(
        inspection
            .query_row(
                "
                SELECT attempt_outcome, exit_code
                FROM clean_events
                WHERE run_id = ?1
                ",
                [run_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?)),
            )
            .unwrap(),
        ("measurement_failure".to_string(), 7)
    );
    assert_eq!(
        inspection
            .query_row(
                "SELECT errors_count FROM runs WHERE id = ?1",
                [run_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        inspection
            .query_row("SELECT COUNT(*) FROM errors", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        inspection
            .query_row(
                "
                SELECT COUNT(*)
                FROM sqlite_master
                WHERE type = 'table' AND name = 'clean_events_v15'
                ",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn version_thirteen_measurement_inference_rejects_ambiguous_collisions_atomically() {
    for (clean_event_count, measurement_error_count) in [(2, 1), (1, 2)] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join(format!(
            "collision-{clean_event_count}-{measurement_error_count}.db"
        ));
        let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(700);
        {
            let store = test_store(&database);
            let run_id = store.start_run(timestamp).unwrap();
            for _ in 0..clean_event_count {
                store
                    .record_clean_event(&CleanEvent {
                        id: 0,
                        run_id,
                        ts: timestamp,
                        path: "/collision".to_string(),
                        bytes_before: 1_000,
                        bytes_after: 1_000,
                        duration_ms: 5,
                        exit_code: Some(0),
                        stderr_excerpt: String::new(),
                        outcome: CleanAttemptOutcome::Success,
                        measurement_failed: false,
                    })
                    .unwrap();
            }
            for ordinal in 0..measurement_error_count {
                store
                    .record_error(&ErrorRecord {
                        id: 0,
                        ts: timestamp,
                        category: "clean".to_string(),
                        path: Some("/collision".to_string()),
                        message: format!(
                            "measure target after cargo clean: injected collision {ordinal}"
                        ),
                    })
                    .unwrap();
            }
        }
        let expected_events = {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute_batch(
                    "
                    CREATE TABLE clean_events_v13 (
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
                    INSERT INTO clean_events_v13 (
                        id,
                        run_id,
                        ts,
                        path,
                        bytes_before,
                        bytes_after,
                        duration_ms,
                        exit_code,
                        stderr_excerpt
                    )
                    SELECT
                        id,
                        run_id,
                        ts,
                        path,
                        bytes_before,
                        bytes_after,
                        duration_ms,
                        exit_code,
                        stderr_excerpt
                    FROM clean_events;
                    DROP TABLE clean_events;
                    ALTER TABLE clean_events_v13 RENAME TO clean_events;
                    CREATE INDEX idx_clean_events_ts ON clean_events(ts);
                    DELETE FROM schema_version WHERE version >= 14;
                    ",
                )
                .unwrap();
            let mut statement = connection
                .prepare(
                    "
                    SELECT
                        id, run_id, ts, path, bytes_before, bytes_after,
                        duration_ms, exit_code, stderr_excerpt
                    FROM clean_events
                    ORDER BY id
                    ",
                )
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };

        let store = Store::open(&database).unwrap();
        let first_error = store.migrate().unwrap_err().to_string();
        assert!(
            first_error.contains("ambiguous measurement failure audit"),
            "{first_error}"
        );
        assert!(first_error.contains(&format!("{clean_event_count} clean event")));
        assert!(first_error.contains(&format!("{measurement_error_count} matching error")));
        let second_error = store.migrate().unwrap_err().to_string();
        assert_eq!(second_error, first_error);
        drop(store);

        let inspection = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            inspection
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            13
        );
        let actual_events = {
            let mut statement = inspection
                .prepare(
                    "
                    SELECT
                        id, run_id, ts, path, bytes_before, bytes_after,
                        duration_ms, exit_code, stderr_excerpt
                    FROM clean_events
                    ORDER BY id
                    ",
                )
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(actual_events, expected_events);
        for temporary_table in ["clean_events_v14", "clean_events_v15"] {
            assert_eq!(
                inspection
                    .query_row(
                        "
                        SELECT COUNT(*)
                        FROM sqlite_master
                        WHERE type = 'table' AND name = ?1
                        ",
                        [temporary_table],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "{temporary_table}"
            );
        }
        let columns = {
            let mut statement = inspection
                .prepare("SELECT name FROM pragma_table_info('clean_events') ORDER BY cid")
                .unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert!(!columns.iter().any(|column| column == "attempt_outcome"));
        assert!(!columns.iter().any(|column| column == "measurement_failed"));
    }
}

#[test]
fn review_plan_load_reports_each_authority_failure_without_fallback() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000);
    let first = review_generation(&store, now, "policy-a");
    let plan = store
        .create_review_plan(now, "policy-a", first.id, false, 0, &[])
        .unwrap();

    assert_eq!(
        store.load_review_plan(9_999, now, "policy-a", first.id),
        Err(PlanLoadError::Missing)
    );
    assert_eq!(
        store.load_review_plan(
            plan.id,
            now + REVIEW_PLAN_TTL - Duration::from_secs(1),
            "policy-a",
            first.id,
        ),
        Ok(plan.clone())
    );
    assert_eq!(
        store.load_review_plan(plan.id, now + REVIEW_PLAN_TTL, "policy-a", first.id),
        Err(PlanLoadError::Expired)
    );

    let policy_plan = store
        .create_review_plan(
            now + Duration::from_secs(1),
            "policy-a",
            first.id,
            false,
            0,
            &[],
        )
        .unwrap();
    let other_policy = review_generation(&store, now + Duration::from_secs(2), "policy-b");
    assert_eq!(
        store.load_review_plan(
            policy_plan.id,
            now + Duration::from_secs(2),
            "policy-b",
            other_policy.id,
        ),
        Err(PlanLoadError::PolicyMismatch)
    );

    let current_a = review_generation(&store, now + Duration::from_secs(3), "policy-a");
    let generation_plan = store
        .create_review_plan(
            now + Duration::from_secs(3),
            "policy-a",
            current_a.id,
            false,
            0,
            &[],
        )
        .unwrap();
    let superseding_a = review_generation(&store, now + Duration::from_secs(4), "policy-a");
    assert_eq!(
        store.load_review_plan(
            generation_plan.id,
            now + Duration::from_secs(4),
            "policy-a",
            superseding_a.id,
        ),
        Err(PlanLoadError::GenerationMismatch)
    );
}

#[test]
fn review_plan_creation_is_atomic_and_retains_only_newest_twenty() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let store = test_store(&database);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(30_000);
    let generation = review_generation(&store, now, "policy");
    let unrepresentable = persisted_review(
        "/too-large",
        Some("/too-large"),
        ProjectClass::Workspace,
        u64::MAX,
        Some((1, 2, 1, 3, Some("boot"))),
        CleanDecision::Cleanable,
    );

    assert!(store
        .create_review_plan(
            now,
            "policy",
            generation.id,
            false,
            i64::MAX,
            &[unrepresentable],
        )
        .is_err());
    {
        let inspection = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            inspection
                .query_row("SELECT COUNT(*) FROM review_plans", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0,
            "a failed target insert must roll back the plan header"
        );
    }

    let mut plans = Vec::new();
    for offset in 0..(REVIEW_PLAN_RETENTION + 2) {
        let created_at = now + Duration::from_secs(offset as u64 + 1);
        plans.push(
            store
                .create_review_plan(
                    created_at,
                    "policy",
                    generation.id,
                    false,
                    offset as i64,
                    &[],
                )
                .unwrap(),
        );
    }
    assert_eq!(
        store.load_review_plan(
            plans[0].id,
            now + Duration::from_secs(25),
            "policy",
            generation.id,
        ),
        Err(PlanLoadError::Missing)
    );
    assert_eq!(
        store.load_review_plan(
            plans[1].id,
            now + Duration::from_secs(25),
            "policy",
            generation.id,
        ),
        Err(PlanLoadError::Missing)
    );
    assert_eq!(
        store
            .load_review_plan(
                plans[2].id,
                now + Duration::from_secs(25),
                "policy",
                generation.id,
            )
            .unwrap()
            .candidate_bytes,
        2
    );

    drop(store);
    let inspection = rusqlite::Connection::open(database).unwrap();
    assert_eq!(
        inspection
            .query_row("SELECT COUNT(*) FROM review_plans", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        REVIEW_PLAN_RETENTION as i64
    );
}

#[test]
fn review_plan_pruning_on_open_removes_expired_or_invalid_authority_and_cascades_targets() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let now = SystemTime::now();
    let (expired_plan_id, generation_id) = {
        let store = test_store(&database);
        let generation = review_generation(&store, now, "policy");
        let review = persisted_review(
            "/project",
            Some("/project"),
            ProjectClass::Workspace,
            100,
            Some((1, 2, 1, 3, Some("boot"))),
            CleanDecision::Cleanable,
        );
        let plan = store
            .create_review_plan(now, "policy", generation.id, false, 100, &[review])
            .unwrap();
        (plan.id, generation.id)
    };
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE review_plans SET expires_at = 0 WHERE id = ?1",
                [expired_plan_id],
            )
            .unwrap();
    }

    let reopened = Store::open(&database).unwrap();
    drop(reopened);
    let inspection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        inspection
            .query_row("SELECT COUNT(*) FROM review_plans", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        inspection
            .query_row("SELECT COUNT(*) FROM review_plan_targets", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0,
        "plan pruning must cascade so no stale target authority survives"
    );
    drop(inspection);

    let invalid_plan_id = {
        let store = Store::open(&database).unwrap();
        store.migrate().unwrap();
        let review = persisted_review(
            "/project",
            Some("/project"),
            ProjectClass::Workspace,
            100,
            Some((1, 2, 1, 3, Some("boot"))),
            CleanDecision::Cleanable,
        );
        store
            .create_review_plan(now, "policy", generation_id, false, 100, &[review])
            .unwrap()
            .id
    };
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE discovery_generations SET authority_valid = 0 WHERE id = ?1",
                [generation_id],
            )
            .unwrap();
    }
    drop(Store::open(&database).unwrap());
    let inspection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        inspection
            .query_row(
                "SELECT COUNT(*) FROM review_plans WHERE id = ?1",
                [invalid_plan_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        inspection
            .query_row(
                "SELECT COUNT(*) FROM review_plan_targets WHERE plan_id = ?1",
                [invalid_plan_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    drop(inspection);

    let (missing_plan_id, missing_generation_id) = {
        let store = Store::open(&database).unwrap();
        store.migrate().unwrap();
        let generation = review_generation(&store, now, "other-policy");
        let review = persisted_review(
            "/missing-generation",
            Some("/missing-generation"),
            ProjectClass::Workspace,
            100,
            Some((1, 4, 1, 5, Some("boot"))),
            CleanDecision::Cleanable,
        );
        let plan = store
            .create_review_plan(now, "other-policy", generation.id, false, 100, &[review])
            .unwrap();
        (plan.id, generation.id)
    };
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        connection
            .execute(
                "DELETE FROM discovery_generations WHERE id = ?1",
                [missing_generation_id],
            )
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM review_plans WHERE id = ?1",
                    [missing_plan_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1,
            "the fixture must leave an orphan for Store::open to prune"
        );
    }
    drop(Store::open(&database).unwrap());
    let inspection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        inspection
            .query_row(
                "SELECT COUNT(*) FROM review_plans WHERE id = ?1",
                [missing_plan_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        inspection
            .query_row(
                "SELECT COUNT(*) FROM review_plan_targets WHERE plan_id = ?1",
                [missing_plan_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn review_plan_load_rejects_an_older_generation_when_the_newest_is_invalid() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let store = test_store(&database);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(35_000);
    let first = review_generation(&store, now, "policy");
    let plan = store
        .create_review_plan(now, "policy", first.id, false, 0, &[])
        .unwrap();
    let newest = review_generation(&store, now + Duration::from_secs(1), "policy");
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE discovery_generations SET authority_valid = 0 WHERE id = ?1",
                [newest.id],
            )
            .unwrap();
    }

    assert_eq!(
        store.load_review_plan(plan.id, now + Duration::from_secs(1), "policy", first.id,),
        Err(PlanLoadError::GenerationMismatch)
    );
}

#[test]
fn versions_ten_and_eleven_add_review_plans_without_changing_history() {
    for version in 10..=11 {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join(format!("v{version}.db"));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(37_000);
        {
            let store = test_store(&database);
            store.upsert_project("/history/project", now).unwrap();
            let run_id = store.start_run(now).unwrap();
            store
                .record_clean_event(&CleanEvent {
                    id: 0,
                    run_id,
                    ts: now,
                    path: "/history/project".to_string(),
                    bytes_before: 900,
                    bytes_after: 100,
                    duration_ms: 3,
                    exit_code: Some(0),
                    stderr_excerpt: String::new(),
                    outcome: CleanAttemptOutcome::Success,
                    measurement_failed: false,
                })
                .unwrap();
            store
                .record_error(&ErrorRecord {
                    id: 0,
                    ts: now,
                    category: "scan".to_string(),
                    path: Some("/history/project".to_string()),
                    message: "historical warning".to_string(),
                })
                .unwrap();
            store.finish_run(run_id, now, 1, 800, 1).unwrap();
        }
        {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute_batch(&format!(
                    "
                    DROP INDEX idx_discovery_generations_single_valid;
                    ALTER TABLE project_observations
                        RENAME TO project_observations_current;
                    CREATE TABLE project_observations (
                        generation_id INTEGER NOT NULL
                            REFERENCES discovery_generations(id) ON DELETE CASCADE,
                        origin_id INTEGER NOT NULL
                            REFERENCES discovery_origins(id) ON DELETE CASCADE,
                        project_path TEXT NOT NULL,
                        project_device,
                        project_inode,
                        target_device,
                        target_inode,
                        observed_at INTEGER NOT NULL,
                        authorized INTEGER NOT NULL,
                        blocked_reason TEXT,
                        boot_session_id TEXT,
                        PRIMARY KEY(generation_id, origin_id, project_path)
                    );
                    INSERT INTO project_observations
                    SELECT
                        generation_id,
                        origin_id,
                        project_path,
                        project_device,
                        project_inode,
                        target_device,
                        target_inode,
                        observed_at,
                        authorized,
                        blocked_reason,
                        boot_session_id
                    FROM project_observations_current;
                    DROP TABLE project_observations_current;
                    CREATE INDEX idx_project_observations_authorized
                        ON project_observations(generation_id, authorized, project_path);
                    DROP TABLE review_plan_targets;
                    DROP TABLE review_plans;
                    DELETE FROM schema_version WHERE version > {version};
                    "
                ))
                .unwrap();
            assert_eq!(
                connection
                    .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                version
            );
        }

        let store = Store::open(&database).unwrap();
        store.migrate().unwrap();
        store.migrate().unwrap();
        assert!(store.table_exists("review_plans").unwrap());
        assert!(store.table_exists("review_plan_targets").unwrap());
        assert_eq!(store.all_projects().unwrap().len(), 1);
        assert_eq!(
            store
                .clean_events_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 1);
        assert_eq!(store.last_run().unwrap().bytes_recovered, 800);
        assert_eq!(
            store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
            800
        );
        let inspection = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            inspection
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            15
        );
    }
}

#[test]
fn review_plan_pruning_preserves_run_clean_project_error_and_recovery_history() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let store = test_store(&database);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(40_000);
    let generation = review_generation(&store, now, "policy");
    let plan = store
        .create_review_plan(now, "policy", generation.id, false, 0, &[])
        .unwrap();
    store.upsert_project("/history/project", now).unwrap();
    let run_id = store.start_run(now).unwrap();
    store
        .record_clean_event(&CleanEvent {
            id: 0,
            run_id,
            ts: now,
            path: "/history/project".to_string(),
            bytes_before: 500,
            bytes_after: 200,
            duration_ms: 5,
            exit_code: Some(0),
            stderr_excerpt: String::new(),
            outcome: CleanAttemptOutcome::Success,
            measurement_failed: false,
        })
        .unwrap();
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "scan".to_string(),
            path: Some("/history/project".to_string()),
            message: "preserve me".to_string(),
        })
        .unwrap();
    store.finish_run(run_id, now, 1, 300, 1).unwrap();

    assert_eq!(
        store
            .prune_review_plans(now + REVIEW_PLAN_TTL, Some(("policy", generation.id)),)
            .unwrap(),
        1
    );
    assert_eq!(
        store.load_review_plan(plan.id, now + REVIEW_PLAN_TTL, "policy", generation.id,),
        Err(PlanLoadError::Missing)
    );
    assert_eq!(store.all_projects().unwrap().len(), 1);
    assert_eq!(
        store
            .clean_events_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 1);
    assert_eq!(store.last_run().unwrap().bytes_recovered, 300);
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        300
    );
}

#[test]
fn scheduler_scan_retry_deadline_round_trips_and_clears() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_234);
    let retry_at = now + Duration::from_secs(60 * 60);
    store
        .record_scheduler_status(now, retry_at, retry_at)
        .unwrap();

    assert_eq!(store.scan_retry_at().unwrap(), None);
    store.record_scan_retry_at(retry_at).unwrap();
    assert_eq!(store.scan_retry_at().unwrap(), Some(retry_at));
    store.clear_scan_retry_at().unwrap();
    assert_eq!(store.scan_retry_at().unwrap(), None);
}
