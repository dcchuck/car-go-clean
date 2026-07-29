use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use car_go_clean::identity::{BootSessionId, FilesystemIdentity, ReviewedIdentity};
use car_go_clean::safety::ReviewSummary;
use car_go_clean::store::{
    CleanEvent, DiscoveryOriginKind, ErrorRecord, GenerationReconciliation,
    ObservationReconciliation, OriginReconciliation, Store,
};

fn test_store(path: &Path) -> Store {
    let store = Store::open(path).unwrap();
    store.migrate().unwrap();
    store
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
                (1, 100, 300, 2, 1500, 0),
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
        vec![(1, 100, Some(300), 1, 900, 1), (2, 400, Some(500), 0, 0, 5),]
    );
    let schema_version = inspection
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(schema_version, 10);
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
            exit_code: 0,
            stderr_excerpt: String::new(),
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
            exit_code: 0,
            stderr_excerpt: String::new(),
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
            exit_code: 9,
            stderr_excerpt: String::new(),
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
        1
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
    assert!(matches!(version, 1 | 4 | 7 | 8));
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
        },
        target_identity: target.map(|(device, inode)| FilesystemIdentity { device, inode }),
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
fn discovery_generation_migrations_preserve_history_without_granting_authority() {
    for version in [1, 4, 7, 8] {
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
        assert_eq!(schema_version, 10);
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

    let inspection = rusqlite::Connection::open(database).unwrap();
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
                    row.get::<_, u64>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Option<u64>>(2)?,
                    row.get::<_, Option<u64>>(3)?,
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
    assert_eq!(schema_version, 10);
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
            inode: 11
        }
    );
    assert_eq!(
        authorized[0].target_identity,
        Some(FilesystemIdentity {
            device: 7,
            inode: 12
        })
    );
    assert_eq!(store.all_projects().unwrap().len(), 2);
}

#[test]
fn discovery_generation_new_snapshot_revokes_removed_projects_and_separates_policy_hashes() {
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

    assert_eq!(
        store
            .authorized_observations(first.id)
            .unwrap()
            .iter()
            .map(|observation| observation.project_path.clone())
            .collect::<Vec<_>>(),
        vec![
            PathBuf::from("/workspace/kept"),
            PathBuf::from("/workspace/removed")
        ]
    );
    assert_eq!(
        store
            .authorized_observations(second.id)
            .unwrap()
            .iter()
            .map(|observation| observation.project_path.clone())
            .collect::<Vec<_>>(),
        vec![PathBuf::from("/workspace/kept")]
    );
    assert_eq!(store.current_generation("policy-a").unwrap(), Some(second));
    assert_eq!(
        store.current_generation("policy-b").unwrap(),
        Some(other_policy)
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
    let duplicate = observed_project("/workspace/duplicate", 1, 2, Some((1, 3)), observed_at);
    let result = store.reconcile_generation(
        observed_at,
        &GenerationReconciliation {
            policy_hash: "policy-a".to_string(),
            boot_session_id: None,
            origins: vec![completed_origin(
                "/workspace",
                vec![duplicate.clone(), duplicate],
            )],
        },
    );

    assert!(result.is_err());
    assert_eq!(store.current_generation("policy-a").unwrap(), None);
    assert!(store.all_projects().unwrap().is_empty());
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
        },
        target: FilesystemIdentity {
            device: 4,
            inode: 6,
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
