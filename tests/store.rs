use std::path::Path;
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
