use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use car_go_clean::safety::ReviewSummary;
use car_go_clean::store::{CleanEvent, ErrorRecord, Store};

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
            ts: t0,
            path: "/b".to_string(),
            bytes_before: 500,
            bytes_after: 0,
            duration_ms: 10,
            exit_code: 0,
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
        .finish_run(run_id, t0 + Duration::from_secs(60), 2, 1400, 1)
        .unwrap();

    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 2);
    assert_eq!(run.bytes_recovered, 1400);
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        1400
    );
    let top = store
        .top_projects_by_bytes(SystemTime::UNIX_EPOCH, 1)
        .unwrap();
    assert_eq!(top[0].path, "/a");
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
