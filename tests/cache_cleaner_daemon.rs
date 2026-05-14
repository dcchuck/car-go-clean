use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use car_go_clean::activity::NoopProcessInspector;
use car_go_clean::cache::Cache;
use car_go_clean::cleaner::{CleanOutcome, Cleaner, CommandRunner};
use car_go_clean::daemon::{Daemon, DaemonOptions, ShutdownFlag};
use car_go_clean::safety::SafetyOptions;
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

#[test]
fn daemon_run_cycle_skips_recent_targets_by_default() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&project, std::time::SystemTime::now())
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_secs(60));
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        cleaner,
        DaemonOptions::default(),
    );

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::from_secs(2 * 60 * 60),
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[test]
fn daemon_shutdown_flag_stops_forever_loop_after_initial_scan() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_millis(1));
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        scanner,
        cleaner,
        DaemonOptions {
            clean_interval: Duration::from_millis(1),
            scan_interval: Duration::from_secs(60),
        },
    );
    let shutdown = ShutdownFlag::new();
    shutdown.request();

    daemon.run_until_shutdown(&shutdown).unwrap();

    assert_eq!(store.all_projects().unwrap().len(), 1);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_scan_cycle_records_unreadable_directories_as_scan_errors() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    let blocked = root.path().join("blocked");
    fs::create_dir_all(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });
    let cleaner = Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60));
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        scanner,
        cleaner,
        DaemonOptions::default(),
    );

    daemon.scan_cycle().unwrap();

    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(store.all_projects().unwrap().len(), 1);
    let errors = store
        .errors_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].category, "scan");
    assert_eq!(errors[0].path.as_deref(), Some(blocked.to_str().unwrap()));
    assert!(errors[0].message.contains("Permission denied"));
}
