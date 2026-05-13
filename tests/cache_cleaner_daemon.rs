use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use car_go_clean::cache::Cache;
use car_go_clean::cleaner::{CleanOutcome, Cleaner, CommandRunner};
use car_go_clean::daemon::{Daemon, DaemonOptions};
use car_go_clean::scanner::{Scanner, ScannerOptions};
use car_go_clean::store::Store;

fn write_file(path: &Path, body: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn cache_verify_and_sync_remove_dead_projects() {
    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let cache = Cache::new(&store);

    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    store
        .upsert_project(project.path(), std::time::SystemTime::now())
        .unwrap();
    store
        .upsert_project("/definitely/not/here", std::time::SystemTime::now())
        .unwrap();

    assert!(cache.verify(project.path()).unwrap());
    assert!(!cache.verify("/definitely/not/here").unwrap());

    let removed = cache.sync_on_disk().unwrap();
    assert_eq!(removed, vec![PathBuf::from("/definitely/not/here")]);
    assert_eq!(store.all_projects().unwrap().len(), 1);
}

#[derive(Clone, Default)]
struct FakeRunner {
    calls: Arc<Mutex<Vec<PathBuf>>>,
    delete_target: bool,
    exit_code: i32,
    stderr: String,
}

impl CommandRunner for FakeRunner {
    fn run(&self, dir: &Path, _cmd: &mut Command) -> anyhow::Result<CleanOutcome> {
        self.calls.lock().unwrap().push(dir.to_path_buf());
        if self.delete_target {
            let _ = fs::remove_dir_all(dir.join("target"));
        }
        Ok(CleanOutcome {
            exit_code: self.exit_code,
            stderr: self.stderr.clone(),
        })
    }
}

#[test]
fn cleaner_measures_bytes_and_skips_missing_target() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");

    let runner = FakeRunner::default();
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_secs(60));
    let skipped = cleaner.clean(project.path()).unwrap();
    assert!(skipped.skipped);
    assert!(runner.calls.lock().unwrap().is_empty());

    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_secs(60));
    let result = cleaner.clean(project.path()).unwrap();
    assert!(!result.skipped);
    assert!(result.bytes_before >= 4096);
    assert_eq!(result.bytes_after, 0);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
}

#[test]
fn daemon_scan_and_run_cycle_record_state() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });
    let cleaner = Cleaner::new(
        "cargo",
        FakeRunner {
            delete_target: true,
            ..FakeRunner::default()
        },
        Duration::from_secs(60),
    );
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        scanner,
        cleaner,
        DaemonOptions::default(),
    );

    daemon.scan_cycle().unwrap();
    assert_eq!(store.all_projects().unwrap().len(), 1);

    daemon.run_cycle().unwrap();
    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 1);
    assert!(run.bytes_recovered >= 2048);
}
