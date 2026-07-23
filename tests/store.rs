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
    let primary = Path::new("/workspace/main");
    let linked = PathBuf::from("/workspace/main/.worktrees/feature");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);

    store
        .replace_linked_worktrees(primary, &[linked.clone()])
        .unwrap();
    store
        .mark_worktree_discovery_failed(primary, now, "git failed")
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![primary.to_path_buf(), linked.clone()]
    );

    store.replace_linked_worktrees(primary, &[linked]).unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
}

#[test]
fn removing_project_removes_linked_worktree_provenance() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let primary = Path::new("/workspace/main");
    let linked = PathBuf::from("/workspace/main/.worktrees/feature");
    store
        .replace_linked_worktrees(primary, &[linked.clone()])
        .unwrap();
    store.remove_project(primary).unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
}

#[test]
fn removing_failed_primary_project_preserves_durable_association_until_success() {
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
    let primary = Path::new("/workspace/main");
    let linked = PathBuf::from("/workspace/feature");
    store
        .replace_linked_worktrees(primary, &[linked.clone()])
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
        .replace_linked_worktrees(primary, &[linked.clone()])
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
        .replace_linked_worktrees(old, &[linked.clone()])
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
        .replace_linked_worktrees(&alias, &[stale.clone()])
        .unwrap();
    store
        .mark_worktree_discovery_failed(&alias, SystemTime::UNIX_EPOCH, "legacy failure")
        .unwrap();

    store.normalize_resolvable_project_aliases().unwrap();
    store
        .replace_linked_worktrees(&canonical, &[current.clone()])
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
