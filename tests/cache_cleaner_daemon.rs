use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime};

use assert_cmd::Command as AssertCommand;
use car_go_clean::activity::{
    activity_signals_for_process, NoopProcessInspector, ProcessInspector,
};
use car_go_clean::cache::Cache;
use car_go_clean::cleaner::{CleanOutcome, Cleaner, CommandRunner};
use car_go_clean::config;
use car_go_clean::daemon::{
    clamp_next_scan_at, Clock, Daemon, DaemonCycleFactory, DaemonCycleSnapshot, DaemonOptions,
    RunSource, ShutdownFlag,
};
use car_go_clean::identity::{
    BootSessionId, FilesystemIdentity, IdentityProvider, SystemIdentityProvider,
};
use car_go_clean::logging::{Logger, LoggerOptions};
use car_go_clean::policy::{Environment, ScopePolicy};
use car_go_clean::safety::{
    review_project_with_identity_provider, CleanDecision, ProjectReview, SafetyOptions,
};
use car_go_clean::scanner::{
    GitWorktreeError, GitWorktreeResolver, Scanner, ScannerOptions, SystemGitWorktreeResolver,
};
use car_go_clean::store::{ErrorRecord, Store};
use predicates::prelude::*;
use predicates::str::contains;

fn write_file(path: &Path, body: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn authoritative_scanner(options: ScannerOptions) -> Scanner {
    let scanner = Scanner::new(options.clone());
    bind_test_authority(scanner, &options)
}

fn authoritative_scanner_with_identity(
    options: ScannerOptions,
    identity: Arc<dyn IdentityProvider>,
) -> Scanner {
    let scanner = Scanner::new(options.clone());
    bind_test_authority_with_identity(scanner, &options, identity)
}

fn authoritative_scanner_with_resolver(
    options: ScannerOptions,
    resolver: Arc<dyn GitWorktreeResolver>,
) -> Scanner {
    let scanner = Scanner::with_worktree_resolver(options.clone(), resolver);
    bind_test_authority(scanner, &options)
}

fn bind_test_authority(scanner: Scanner, options: &ScannerOptions) -> Scanner {
    bind_test_authority_with_identity(scanner, options, Arc::new(SystemIdentityProvider))
}

fn bind_test_authority_with_identity(
    scanner: Scanner,
    options: &ScannerOptions,
    identity: Arc<dyn IdentityProvider>,
) -> Scanner {
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.toml");
    let body = format!(
        "scan_dirs = {}\nproject_dirs = {}\noverride_excludes = {}\ntarget_quiet_period = \"1ms\"\n",
        serde_json::to_string(&options.roots).unwrap(),
        serde_json::to_string(&options.project_dirs).unwrap(),
        serde_json::to_string(&options.excludes).unwrap(),
    );
    fs::write(&config_path, body).unwrap();
    let config = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(
        &config,
        Path::new("/car-go-clean/tests/config.toml"),
        &EmptyEnvironment,
    )
    .unwrap();
    scanner.with_authority(policy, identity)
}

fn sqlite_column_exists(connection: &rusqlite::Connection, table: &str, column: &str) -> bool {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let mut rows = statement.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        if row.get::<_, String>(1).unwrap() == column {
            return true;
        }
    }
    false
}

fn downgrade_runtime_database_to_version_nine(database: &Path) {
    let connection = rusqlite::Connection::open(database).unwrap();
    if sqlite_column_exists(&connection, "scheduler_state", "scan_retry_at") {
        connection
            .execute("ALTER TABLE scheduler_state DROP COLUMN scan_retry_at", [])
            .unwrap();
    }
    if sqlite_column_exists(&connection, "project_observations", "boot_session_id") {
        connection
            .execute(
                "ALTER TABLE project_observations DROP COLUMN boot_session_id",
                [],
            )
            .unwrap();
    }
    if sqlite_column_exists(&connection, "discovery_generations", "authority_valid") {
        connection
            .execute(
                "ALTER TABLE discovery_generations DROP COLUMN authority_valid",
                [],
            )
            .unwrap();
    }
    connection
        .execute("DELETE FROM schema_version WHERE version >= 10", [])
        .unwrap();
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

#[test]
fn generation_deduplicates_overlapping_origins_and_revokes_absent_projects() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![project.clone()],
            excludes: vec![],
        }),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );

    let first = daemon.scan_cycle().unwrap();
    assert_eq!(first.origins.len(), 2);
    assert_eq!(
        store
            .authorized_observations(first.generation)
            .unwrap()
            .len(),
        1
    );

    fs::remove_dir_all(&project).unwrap();
    let second = daemon.scan_cycle().unwrap();
    assert!(store
        .authorized_observations(second.generation)
        .unwrap()
        .is_empty());
    assert_eq!(store.all_projects().unwrap().len(), 1);
}

#[test]
fn failed_origin_preserves_history_but_grants_no_current_authority() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::failure("git failed")),
        ),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );

    let scan = daemon.scan_cycle().unwrap();

    assert_eq!(scan.origins.len(), 1);
    assert!(!scan.origins[0].completed);
    assert!(store
        .authorized_observations(scan.generation)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.all_projects().unwrap()[0].path,
        primary.canonicalize().unwrap().to_string_lossy()
    );
}

#[test]
fn cache_eviction_preserves_association_for_a_later_discovery_failure() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = root.path().join("linked");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let options = ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    };
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let successful = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            options.clone(),
            Arc::new(FakeWorktreeResolver::paths(vec![linked.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    successful.scan_cycle().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();

    fs::remove_dir_all(&primary).unwrap();
    Cache::new(&store).sync_on_disk().unwrap();
    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
        vec![canonical_linked.to_string_lossy().into_owned()]
    );

    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    let failed = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            options.clone(),
            Arc::new(FakeWorktreeResolver::failure("git failed")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    failed.scan_cycle().unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_linked.clone(), canonical_primary.clone()]
    );
    let failed_result = failed
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(failed_result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());

    let successful = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            options,
            Arc::new(FakeWorktreeResolver::paths(vec![linked.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    successful.scan_cycle().unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
    let successful_result = successful
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(successful_result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    assert_eq!(runner.calls.lock().unwrap()[0].dir, canonical_linked);
}

#[test]
fn first_v4_scan_reconciles_v3_cached_excluded_worktree_without_provenance() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let excluded = root.path().join("excluded/team/worktree");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&excluded.join("Cargo.toml"), b"[workspace]\n");
    write_file(&excluded.join("target/blob.bin"), &[0; 2048]);
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_excluded = excluded.canonicalize().unwrap();
    let excluded_candidate = root.path().join("excluded/team/../team/worktree");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    {
        let store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "
            DROP TABLE linked_worktrees;
            DROP TABLE worktree_discovery_failures;
            DELETE FROM schema_version WHERE version >= 4;
            DELETE FROM projects;
            ",
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        for path in [&canonical_primary, &canonical_excluded] {
            conn.execute(
                "
                INSERT INTO projects (path, discovered_at, last_seen_at)
                VALUES (?1, ?2, ?2)
                ",
                rusqlite::params![path.to_str().unwrap(), now],
            )
            .unwrap();
        }
    }

    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    assert!(store.table_exists("linked_worktrees").unwrap());
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec!["excluded/team".to_string()],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![excluded_candidate])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    assert_eq!(store.all_projects().unwrap().len(), 2);
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: true,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[test]
fn first_v4_scan_reconciles_v3_cached_out_of_scope_worktree_without_provenance() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&outside.path().join("Cargo.toml"), b"[workspace]\n");
    write_file(&outside.path().join("target/blob.bin"), &[0; 2048]);
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_outside = outside.path().canonicalize().unwrap();

    let state_dir = tempfile::tempdir().unwrap();
    let db_path = state_dir.path().join("state.db");
    {
        let store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "
            DROP TABLE linked_worktrees;
            DROP TABLE worktree_discovery_failures;
            DELETE FROM schema_version WHERE version >= 4;
            DELETE FROM projects;
            ",
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        for path in [&canonical_primary, &canonical_outside] {
            conn.execute(
                "
                INSERT INTO projects (path, discovered_at, last_seen_at)
                VALUES (?1, ?2, ?2)
                ",
                rusqlite::params![path.to_str().unwrap(), now],
            )
            .unwrap();
        }
    }

    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![canonical_outside.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
        vec![canonical_primary.to_string_lossy().into_owned()]
    );
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());

    let config = state_dir.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.path().display()
        ),
    )
    .unwrap();
    AssertCommand::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(state_dir.path())
        .assert()
        .code(2)
        .stdout(contains(canonical_outside.display().to_string()).not());
    AssertCommand::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(state_dir.path())
        .assert()
        .code(2)
        .stdout(contains("Cleanable projects: 0"))
        .stdout(contains(canonical_outside.display().to_string()).not());
}

#[test]
fn successful_out_of_scope_reconciliation_preserves_explicit_project_dir() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&outside.path().join("Cargo.toml"), b"[workspace]\n");
    write_file(&outside.path().join("target/blob.bin"), &[0; 2048]);
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_explicit = outside.path().canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_explicit, SystemTime::now())
        .unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![canonical_explicit.clone()],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::paths(
                vec![canonical_explicit.clone()],
            )),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    assert!(store
        .all_projects()
        .unwrap()
        .iter()
        .any(|project| project.path == canonical_explicit.to_string_lossy()));
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "git failed")
        .unwrap();
    let mut expected_blocks = vec![canonical_explicit.clone(), canonical_primary];
    expected_blocks.sort();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        expected_blocks
    );
    daemon.scan_cycle().unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    assert_eq!(runner.calls.lock().unwrap()[0].dir, canonical_explicit);
}

#[test]
fn configured_project_dir_does_not_override_exclusion_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let explicit = root.path().join("excluded/team/worktree");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&explicit.join("Cargo.toml"), b"[workspace]\n");
    write_file(&explicit.join("target/blob.bin"), &[0; 2048]);
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_explicit = explicit.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![explicit.clone()],
                excludes: vec!["excluded/team".to_string()],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![explicit])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    assert!(!store
        .all_projects()
        .unwrap()
        .iter()
        .any(|project| project.path == canonical_explicit.to_string_lossy()));
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "git failed")
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_primary]
    );
    daemon.scan_cycle().unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_does_not_clean_a_persisted_alias_of_a_managed_cache_project() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("Library/Caches/cached-project");
    let alias = root.path().join("legacy-alias");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);
    symlink(&project, &alias).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&alias, SystemTime::now()).unwrap();

    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![alias.clone()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
        vec![project
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned()]
    );

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
        vec![project
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned()]
    );
}

#[cfg(unix)]
#[test]
fn daemon_physically_classifies_frozen_trusted_and_untrusted_primary_rows() {
    use std::os::unix::fs::symlink;

    for (trusted, class_path) in [
        (true, "Library/Caches/replacement"),
        (true, "OrbStack/docker/replacement"),
        (false, "Library/Caches/replacement"),
        (false, "OrbStack/docker/replacement"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let original = root_path.join("original");
        let frozen_primary = root_path.join("frozen-primary");
        let replacement = root_path.join(class_path);
        let child = root_path.join("historical-child");
        for path in [&original, &replacement, &child] {
            fs::create_dir_all(path).unwrap();
        }
        write_file(&replacement.join("Cargo.toml"), b"[package]\n");
        write_file(&replacement.join("target/blob.bin"), &[0; 2048]);
        let canonical_replacement = replacement.canonicalize().unwrap();
        let canonical_child = child.canonicalize().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("state.db");

        let store = if trusted {
            fs::create_dir_all(&frozen_primary).unwrap();
            write_file(&frozen_primary.join("Cargo.toml"), b"[package]\n");
            let store = Store::open(&db_path).unwrap();
            store.migrate().unwrap();
            store
                .upsert_project(&frozen_primary, SystemTime::now())
                .unwrap();
            store
                .replace_linked_worktrees(&frozen_primary, std::slice::from_ref(&canonical_child))
                .unwrap();
            fs::remove_dir_all(&frozen_primary).unwrap();
            symlink(&canonical_replacement, &frozen_primary).unwrap();
            store
        } else {
            symlink(&original, &frozen_primary).unwrap();
            {
                let store = Store::open(&db_path).unwrap();
                store.migrate().unwrap();
                store
                    .upsert_project(&frozen_primary, SystemTime::now())
                    .unwrap();
            }
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                DROP TABLE linked_worktrees;
                CREATE TABLE linked_worktrees (
                    primary_path TEXT NOT NULL,
                    linked_path TEXT NOT NULL,
                    PRIMARY KEY (primary_path, linked_path)
                );
                DELETE FROM schema_version WHERE version >= 5;
                ",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
                rusqlite::params![
                    frozen_primary.to_str().unwrap(),
                    canonical_child.to_str().unwrap()
                ],
            )
            .unwrap();
            drop(conn);
            let store = Store::open(&db_path).unwrap();
            store.migrate().unwrap();
            fs::remove_file(&frozen_primary).unwrap();
            symlink(&canonical_replacement, &frozen_primary).unwrap();
            store
        };

        let runner = FakeRunner {
            delete_target: true,
            ..FakeRunner::default()
        };
        let daemon = Daemon::new(
            &store,
            Cache::new(&store),
            authoritative_scanner(ScannerOptions {
                roots: vec![root_path.clone()],
                project_dirs: vec![],
                excludes: vec![],
            }),
            Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
            DaemonOptions {
                target_quiet_period: Duration::ZERO,
                ..DaemonOptions::default()
            },
        );
        daemon.scan_cycle().unwrap();
        let result = daemon
            .run_cycle_with_safety(
                SafetyOptions {
                    target_quiet_period: Duration::ZERO,
                    include_managed_cache: false,
                    include_active: false,
                    force: false,
                },
                &NoopProcessInspector,
            )
            .unwrap();

        assert_eq!(result.cleaned, 0, "trusted={trusted}, path={class_path}");
        assert_eq!(result.skipped, 1, "trusted={trusted}, path={class_path}");
        assert!(
            runner.calls.lock().unwrap().is_empty(),
            "trusted={trusted}, path={class_path}"
        );
        assert!(
            replacement.join("target/blob.bin").exists(),
            "trusted={trusted}, path={class_path}"
        );
        assert!(store
            .all_projects()
            .unwrap()
            .iter()
            .any(|project| project.path == canonical_replacement.to_string_lossy()));
    }
}

#[cfg(unix)]
#[test]
fn daemon_reused_v4_untrusted_primary_does_not_release_historical_child() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let original = root_path.join("original");
    let reused = root_path.join("reused-primary");
    let child = root_path.join("historical-child");
    fs::create_dir_all(&original).unwrap();
    write_file(&child.join("Cargo.toml"), b"[package]\n");
    write_file(&child.join("target/blob.bin"), &[0; 2048]);
    symlink(&original, &reused).unwrap();
    let canonical_original = original.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    {
        let store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
        store
            .upsert_project(&canonical_child, SystemTime::now())
            .unwrap();
    }
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "
        DROP TABLE linked_worktrees;
        CREATE TABLE linked_worktrees (
            primary_path TEXT NOT NULL,
            linked_path TEXT NOT NULL,
            PRIMARY KEY (primary_path, linked_path)
        );
        DELETE FROM schema_version WHERE version >= 5;
        ",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
        rusqlite::params![reused.to_str().unwrap(), canonical_child.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();

    fs::remove_file(&reused).unwrap();
    fs::create_dir_all(reused.join(".git")).unwrap();
    write_file(&reused.join("Cargo.toml"), b"[workspace]\n");
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![],
                project_dirs: vec![reused],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_original, SystemTime::now(), "original failed")
        .unwrap();
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(child.join("target/blob.bin").exists());
}

#[cfg(unix)]
#[test]
fn daemon_cache_review_cannot_retarget_trusted_linked_provenance() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let primary = root_path.join("primary");
    let linked = root_path.join("linked");
    let unrelated = root_path.join("unrelated");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[package]\n");
    write_file(&unrelated.join("Cargo.toml"), b"[package]\n");
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();
    let canonical_unrelated = unrelated.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let scanner_options = ScannerOptions {
        roots: vec![root_path],
        project_dirs: vec![],
        excludes: vec![],
    };
    let successful = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options.clone(),
            Arc::new(FakeWorktreeResolver::paths(vec![canonical_linked.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    successful.scan_cycle().unwrap();

    fs::remove_dir_all(&canonical_linked).unwrap();
    symlink(&canonical_unrelated, &canonical_linked).unwrap();
    successful
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    fs::remove_file(&canonical_linked).unwrap();
    write_file(&canonical_linked.join("Cargo.toml"), b"[package]\n");
    write_file(&canonical_linked.join("target/blob.bin"), &[0; 2048]);
    let failed = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options,
            Arc::new(FakeWorktreeResolver::failure("git failed")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    failed.scan_cycle().unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_linked.clone(), canonical_primary]
    );
    let result = failed
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(canonical_linked.join("target/blob.bin").exists());
}

#[derive(Clone, Default)]
struct FakeRunner {
    calls: Arc<Mutex<Vec<FakeCall>>>,
    delete_target: bool,
    delete_relative_path: Option<PathBuf>,
    replace_target_with_file_for: Option<PathBuf>,
    exit_code: i32,
    stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeCall {
    dir: PathBuf,
    args: Vec<String>,
    envs: Vec<(String, Option<String>)>,
}

#[derive(Clone)]
struct ArgumentsProcessInspector {
    arguments: Vec<PathBuf>,
    cwd: Option<PathBuf>,
}

struct MutatingProcessInspector {
    calls: AtomicUsize,
    mutate_on_call: usize,
    mutation: Box<dyn Fn() + Send + Sync>,
}

struct ActiveOnSecondInspector {
    calls: AtomicUsize,
    project: PathBuf,
}

struct FixedClock {
    now: SystemTime,
}

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.now
    }

    fn wait_until_or_shutdown(&self, deadline: SystemTime, shutdown: &ShutdownFlag) -> bool {
        if deadline <= self.now {
            false
        } else {
            shutdown.request();
            true
        }
    }
}

struct AdvancingClock {
    initial: SystemTime,
    step: Duration,
    calls: AtomicUsize,
}

struct RollbackClock {
    initial: SystemTime,
    calls: AtomicUsize,
}

impl Clock for RollbackClock {
    fn now(&self) -> SystemTime {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => self.initial,
            1 => self.initial + Duration::from_secs(100),
            _ => self.initial + Duration::from_secs(50),
        }
    }
}

type ScheduledClockHook = (usize, Box<dyn Fn() + Send + Sync>);

struct HookClock {
    now: Mutex<SystemTime>,
    calls: AtomicUsize,
    hook: Mutex<Option<ScheduledClockHook>>,
}

impl HookClock {
    fn new(now: SystemTime) -> Self {
        Self {
            now: Mutex::new(now),
            calls: AtomicUsize::new(0),
            hook: Mutex::new(None),
        }
    }

    fn set_now(&self, now: SystemTime) {
        *self.now.lock().unwrap() = now;
    }

    fn on_second_next_call(&self, hook: impl Fn() + Send + Sync + 'static) {
        let call = self.calls.load(Ordering::SeqCst) + 2;
        *self.hook.lock().unwrap() = Some((call, Box::new(hook)));
    }
}

impl Clock for HookClock {
    fn now(&self) -> SystemTime {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = {
            let mut hook = self.hook.lock().unwrap();
            if hook
                .as_ref()
                .is_some_and(|(hook_call, _)| *hook_call == call)
            {
                hook.take().map(|(_, hook)| hook)
            } else {
                None
            }
        };
        if let Some(hook) = hook {
            hook();
        }
        *self.now.lock().unwrap()
    }

    fn wait_until_or_shutdown(&self, deadline: SystemTime, shutdown: &ShutdownFlag) -> bool {
        if deadline <= self.now() {
            false
        } else {
            shutdown.request();
            true
        }
    }
}

impl AdvancingClock {
    fn by(step: Duration) -> Self {
        Self {
            initial: SystemTime::now(),
            step,
            calls: AtomicUsize::new(0),
        }
    }
}

impl Clock for AdvancingClock {
    fn now(&self) -> SystemTime {
        self.initial + self.step * self.calls.fetch_add(1, Ordering::SeqCst) as u32
    }

    fn wait_until_or_shutdown(&self, deadline: SystemTime, shutdown: &ShutdownFlag) -> bool {
        if deadline <= self.now() {
            false
        } else {
            shutdown.request();
            true
        }
    }
}

struct SwitchableIdentityProvider {
    boot_phase: AtomicUsize,
    target_revision: AtomicUsize,
    cross_device: AtomicUsize,
}

impl SwitchableIdentityProvider {
    fn switch_boot(&self) {
        self.boot_phase.store(1, Ordering::SeqCst);
        self.target_revision.store(1, Ordering::SeqCst);
    }

    fn replace_target_in_same_boot(&self) {
        self.target_revision.fetch_add(1, Ordering::SeqCst);
    }

    fn move_target_to_other_device(&self) {
        self.cross_device.store(1, Ordering::SeqCst);
    }
}

impl IdentityProvider for SwitchableIdentityProvider {
    fn boot_session(&self) -> anyhow::Result<Option<BootSessionId>> {
        let boot = if self.boot_phase.load(Ordering::SeqCst) == 0 {
            "boot-a"
        } else {
            "boot-b"
        };
        Ok(Some(BootSessionId(boot.to_string())))
    }

    fn identity(&self, path: &Path) -> anyhow::Result<FilesystemIdentity> {
        Ok(FilesystemIdentity {
            device: if path.file_name() == Some(OsStr::new("target"))
                && self.cross_device.load(Ordering::SeqCst) != 0
            {
                8
            } else {
                7
            },
            inode: if path.file_name() == Some(OsStr::new("target")) {
                20 + self.target_revision.load(Ordering::SeqCst) as u64
            } else {
                10 + self.boot_phase.load(Ordering::SeqCst) as u64
            },
        })
    }
}

#[derive(Clone, Copy)]
struct FixedBootSystemIdentityProvider;

impl IdentityProvider for FixedBootSystemIdentityProvider {
    fn boot_session(&self) -> anyhow::Result<Option<BootSessionId>> {
        Ok(Some(BootSessionId("test-boot".to_string())))
    }

    fn identity(&self, path: &Path) -> anyhow::Result<FilesystemIdentity> {
        SystemIdentityProvider.identity(path)
    }
}

struct UnavailableBootIdentityProvider {
    boot_available: AtomicUsize,
    target_revision: AtomicUsize,
}

impl UnavailableBootIdentityProvider {
    fn new(boot_available: bool) -> Self {
        Self {
            boot_available: AtomicUsize::new(usize::from(boot_available)),
            target_revision: AtomicUsize::new(0),
        }
    }

    fn make_boot_unavailable(&self) {
        self.boot_available.store(0, Ordering::SeqCst);
    }

    fn replace_target(&self) {
        self.target_revision.fetch_add(1, Ordering::SeqCst);
    }
}

impl IdentityProvider for UnavailableBootIdentityProvider {
    fn boot_session(&self) -> anyhow::Result<Option<BootSessionId>> {
        Ok((self.boot_available.load(Ordering::SeqCst) != 0)
            .then(|| BootSessionId("test-boot".to_string())))
    }

    fn identity(&self, path: &Path) -> anyhow::Result<FilesystemIdentity> {
        Ok(FilesystemIdentity {
            device: 7,
            inode: if path.file_name() == Some(OsStr::new("target")) {
                20 + self.target_revision.load(Ordering::SeqCst) as u64
            } else {
                10
            },
        })
    }
}

struct EmptyEnvironment;

impl Environment for EmptyEnvironment {
    fn var_os(&self, _name: &str) -> Option<std::ffi::OsString> {
        None
    }
}

#[derive(Clone)]
struct FileCycleFactory {
    config_path: PathBuf,
}

impl DaemonCycleFactory for FileCycleFactory {
    fn snapshot(&self) -> anyhow::Result<DaemonCycleSnapshot> {
        let cfg = config::load(&self.config_path)?;
        cfg.validate()?;
        let policy = ScopePolicy::build(&cfg, &self.config_path, &EmptyEnvironment)?;
        let scanner = Scanner::new(ScannerOptions {
            roots: cfg.scan_dirs.clone(),
            project_dirs: cfg.project_dirs.clone(),
            excludes: cfg.effective_excludes(),
        })
        .with_authority(policy, Arc::new(SystemIdentityProvider));
        Ok(DaemonCycleSnapshot::new(
            scanner,
            DaemonOptions {
                clean_interval: cfg.clean_interval,
                scan_interval: cfg.scan_interval,
                target_quiet_period: cfg.target_quiet_period,
            },
        ))
    }
}

impl MutatingProcessInspector {
    fn on_second_call(mutation: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            mutate_on_call: 2,
            mutation: Box::new(mutation),
        }
    }
}

impl ProcessInspector for ArgumentsProcessInspector {
    fn active_projects(
        &self,
        projects: &[PathBuf],
    ) -> anyhow::Result<Vec<car_go_clean::activity::ActivitySignal>> {
        Ok(activity_signals_for_process(
            42,
            self.cwd.as_deref(),
            &self.arguments,
            projects,
        ))
    }
}

impl ProcessInspector for MutatingProcessInspector {
    fn active_projects(
        &self,
        _projects: &[PathBuf],
    ) -> anyhow::Result<Vec<car_go_clean::activity::ActivitySignal>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.mutate_on_call {
            (self.mutation)();
        }
        Ok(Vec::new())
    }
}

impl ProcessInspector for ActiveOnSecondInspector {
    fn active_projects(
        &self,
        _projects: &[PathBuf],
    ) -> anyhow::Result<Vec<car_go_clean::activity::ActivitySignal>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok((call >= 2)
            .then(|| car_go_clean::activity::ActivitySignal {
                pid: 42,
                project_path: self.project.clone(),
                reason: "became active after review".to_string(),
            })
            .into_iter()
            .collect())
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, dir: &Path, cmd: &mut Command) -> anyhow::Result<CleanOutcome> {
        self.calls.lock().unwrap().push(FakeCall {
            dir: dir.to_path_buf(),
            args: cmd
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
            envs: cmd
                .get_envs()
                .map(|(key, value)| (to_string(key), value.map(to_string)))
                .collect(),
        });
        if self.delete_target {
            let _ = fs::remove_dir_all(dir.join("target"));
        }
        if let Some(relative) = &self.delete_relative_path {
            fs::remove_file(dir.join("target").join(relative)).unwrap();
        }
        if self.replace_target_with_file_for.as_deref() == Some(dir) {
            fs::write(dir.join("target"), b"not a directory").unwrap();
        }
        Ok(CleanOutcome {
            exit_code: self.exit_code,
            stderr: self.stderr.clone(),
        })
    }
}

#[derive(Clone)]
struct FakeWorktreeResolver {
    result: Result<Vec<PathBuf>, String>,
}

impl FakeWorktreeResolver {
    fn paths(paths: Vec<PathBuf>) -> Self {
        Self { result: Ok(paths) }
    }

    fn failure(message: &str) -> Self {
        Self {
            result: Err(message.to_string()),
        }
    }
}

impl GitWorktreeResolver for FakeWorktreeResolver {
    fn linked_worktrees(&self, _primary: &Path) -> Result<Vec<PathBuf>, GitWorktreeError> {
        self.result.clone().map_err(GitWorktreeError::new)
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct SuccessfulOutputResolver {
    stdout: Vec<u8>,
}

#[cfg(unix)]
impl GitWorktreeResolver for SuccessfulOutputResolver {
    fn linked_worktrees(&self, primary: &Path) -> Result<Vec<PathBuf>, GitWorktreeError> {
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;

        SystemGitWorktreeResolver.worktree_paths_from_output(
            primary,
            &Output {
                status: ExitStatus::from_raw(0),
                stdout: self.stdout.clone(),
                stderr: Vec::new(),
            },
        )
    }
}

fn to_string(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

fn shutdown_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
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
fn cleaner_reports_only_after_preflight_and_immediately_before_runner() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    let events = Arc::new(Mutex::new(Vec::new()));
    let runner = SequencedRunner {
        events: events.clone(),
        exit_code: 0,
    };
    let cleaner = Cleaner::new("cargo", runner, Duration::from_secs(60));
    let report_events = events.clone();

    let skipped = cleaner
        .clean_with_attempt_reporter(project.path(), move |_project, target| {
            report_events
                .lock()
                .unwrap()
                .push(format!("target:{}", target.display()));
        })
        .unwrap();

    assert!(skipped.skipped);
    assert!(events.lock().unwrap().is_empty());

    write_file(&project.path().join("target/blob.bin"), &[0; 4096]);
    let report_events = events.clone();
    let result = cleaner
        .clean_with_attempt_reporter(project.path(), move |_project, target| {
            report_events
                .lock()
                .unwrap()
                .push(format!("target:{}", target.display()));
        })
        .unwrap();

    assert!(!result.skipped);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            format!("target:{}", project.path().join("target").display()),
            format!("cargo:{}", project.path().display()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn cleaner_preflight_error_emits_no_target_event_and_runs_no_cargo() {
    use std::os::unix::fs::PermissionsExt;

    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/blob.bin"), &[0; 4096]);
    let target = project.path().join("target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();
    let runner = FakeRunner::default();
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_secs(60));
    let reported = Arc::new(AtomicUsize::new(0));
    let reported_for_callback = reported.clone();

    let result = cleaner.clean_with_attempt_reporter(project.path(), move |_, _| {
        reported_for_callback.fetch_add(1, Ordering::SeqCst);
    });

    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert_eq!(reported.load(Ordering::SeqCst), 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_skips_canonical_project_for_symlink_spelled_active_argument() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("canonical-project");
    let alias = root.path().join("project-alias");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/debug/server"), &[0; 2048]);
    symlink(&project, &alias).unwrap();
    let canonical_project = project.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_project, SystemTime::now())
        .unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &ArgumentsProcessInspector {
                arguments: vec![alias.join("target/debug/server")],
                cwd: None,
            },
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_skips_canonical_project_for_symlink_spelled_out_dir_argument() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("canonical-project");
    let alias = root.path().join("project-alias");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/debug/server"), &[0; 2048]);
    symlink(&project, &alias).unwrap();
    let canonical_project = project.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_project, SystemTime::now())
        .unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &ArgumentsProcessInspector {
                arguments: vec![PathBuf::from(format!(
                    "--out-dir={}",
                    alias.join("target").display()
                ))],
                cwd: Some(root.path().to_path_buf()),
            },
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_skips_canonical_project_for_sequential_rust_path_arguments() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("canonical-project");
    let alias = root.path().join("project-alias");
    let manifest_link = root.path().join("manifest-link");
    let target_link = root.path().join("target-link");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/libdep.rlib"), &[0; 2048]);
    write_file(&project.join("target/app"), &[0; 2048]);
    symlink(&project, &alias).unwrap();
    symlink(alias.join("Cargo.toml"), &manifest_link).unwrap();
    symlink(alias.join("target"), &target_link).unwrap();
    let canonical_project = project.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_project, SystemTime::now())
        .unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();
    let argument_sets = [
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("--emit"),
            PathBuf::from(format!(
                "link={}",
                alias.join("target/future-output").display()
            )),
        ],
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("-Ldependency=target-link"),
        ],
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("--library-path=target-link"),
        ],
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("-L"),
            PathBuf::from(format!("dependency={}", alias.join("target").display())),
        ],
        vec![
            PathBuf::from("cargo"),
            PathBuf::from("--manifest-path"),
            PathBuf::from("manifest-link"),
        ],
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("--extern"),
            PathBuf::from(format!(
                "dep={}",
                alias.join("target/libdep.rlib").display()
            )),
        ],
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("--emit"),
            PathBuf::from(format!("link={}", alias.join("target/app").display())),
        ],
    ];

    for arguments in argument_sets {
        let result = daemon
            .run_cycle_with_safety(
                SafetyOptions {
                    target_quiet_period: Duration::ZERO,
                    include_managed_cache: false,
                    include_active: false,
                    force: false,
                },
                &ArgumentsProcessInspector {
                    arguments,
                    cwd: Some(root.path().to_path_buf()),
                },
            )
            .unwrap();
        assert_eq!(result.cleaned, 0);
        assert_eq!(result.skipped, 1);
    }
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_skips_non_utf8_nested_rust_argument_paths_with_outside_cwd() {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::symlink;

    fn prefixed_path(prefix: &[u8], path: &Path) -> PathBuf {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(path.as_os_str().as_bytes());
        PathBuf::from(OsString::from_vec(bytes))
    }

    let root = tempfile::tempdir().unwrap();
    let outside_cwd = tempfile::tempdir().unwrap();
    let project = root.path().join("canonical-project");
    let alias = root.path().join("project-alias");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/existing"), &[0; 2048]);
    symlink(&project, &alias).unwrap();
    let canonical_project = project.canonicalize().unwrap();
    let future = alias
        .join("target")
        .join(OsString::from_vec(b"future-output-\xff".to_vec()));
    let target = alias
        .join("target")
        .join(OsString::from_vec(b"search-path-\xfe".to_vec()));

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_project, SystemTime::now())
        .unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();
    let argument_sets = vec![
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("--extern"),
            prefixed_path(b"dep=", &future),
        ],
        vec![
            PathBuf::from("rustc"),
            prefixed_path(b"--extern=dep=", &future),
        ],
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("--emit"),
            prefixed_path(b"link=", &future),
        ],
        vec![
            PathBuf::from("rustc"),
            prefixed_path(b"--emit=link=", &future),
        ],
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("-L"),
            prefixed_path(b"dependency=", &target),
        ],
        vec![
            PathBuf::from("rustc"),
            prefixed_path(b"-Ldependency=", &target),
        ],
        vec![
            PathBuf::from("rustc"),
            PathBuf::from("--library-path"),
            prefixed_path(b"dependency=", &target),
        ],
        vec![
            PathBuf::from("rustc"),
            prefixed_path(b"--library-path=dependency=", &target),
        ],
        vec![
            PathBuf::from("rustc"),
            prefixed_path(b"--manifest-path=", &future),
        ],
        vec![
            PathBuf::from("rustc"),
            prefixed_path(b"--target-dir=", &future),
        ],
        vec![
            PathBuf::from("rustc"),
            prefixed_path(b"--out-dir=", &future),
        ],
    ];

    for arguments in argument_sets {
        let result = daemon
            .run_cycle_with_safety(
                SafetyOptions {
                    target_quiet_period: Duration::ZERO,
                    include_managed_cache: false,
                    include_active: false,
                    force: false,
                },
                &ArgumentsProcessInspector {
                    arguments,
                    cwd: Some(outside_cwd.path().to_path_buf()),
                },
            )
            .unwrap();
        assert_eq!(result.cleaned, 0);
        assert_eq!(result.skipped, 1);
    }
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[test]
fn cleaner_forces_reviewed_direct_target_dir() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_secs(60));

    let result = cleaner.clean(project.path()).unwrap();

    assert!(!result.skipped);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].dir, project.path());
    assert_eq!(
        calls[0].args,
        vec![
            "clean".to_string(),
            "--target-dir".to_string(),
            project.path().join("target").to_string_lossy().into_owned()
        ]
    );
    assert!(calls[0]
        .envs
        .iter()
        .any(|(key, value)| key == "CARGO_TARGET_DIR" && value.is_none()));
}

#[cfg(unix)]
#[test]
fn cleaner_skips_symlinked_target_without_invoking_runner() {
    use std::os::unix::fs::symlink;

    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    let external_target = tempfile::tempdir().unwrap();
    symlink(external_target.path(), project.path().join("target")).unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_secs(60));

    let result = cleaner.clean(project.path()).unwrap();

    assert!(result.skipped);
    assert!(runner.calls.lock().unwrap().is_empty());
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

    let scanner = authoritative_scanner(ScannerOptions {
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
        DaemonOptions {
            clean_interval: Duration::from_secs(24 * 60 * 60),
            scan_interval: Duration::from_secs(7 * 24 * 60 * 60),
            target_quiet_period: Duration::from_millis(1),
        },
    );

    daemon.scan_cycle().unwrap();
    assert_eq!(store.all_projects().unwrap().len(), 1);

    std::thread::sleep(Duration::from_millis(10));
    daemon.run_cycle().unwrap();
    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 1);
    assert!(run.bytes_recovered >= 2048);
}

#[test]
fn same_generation_target_identity_replacement_is_rejected_before_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let identity = Arc::new(FixedBootSystemIdentityProvider);
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_identity(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            identity,
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();

    fs::rename(project.join("target"), project.join("target-observed")).unwrap();
    write_file(&project.join("target/replacement.bin"), &[0; 2048]);
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/replacement.bin").exists());
}

fn dynamic_generation_rejects_replaced_target_with_boot_availability(initially_available: bool) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner::default();
    let identity = Arc::new(UnavailableBootIdentityProvider::new(initially_available));
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_identity(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            identity.clone(),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();

    identity.make_boot_unavailable();
    identity.replace_target();
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/original.bin").exists());
}

#[test]
fn dynamic_generation_rejects_replaced_target_when_both_boot_ids_are_unavailable() {
    dynamic_generation_rejects_replaced_target_with_boot_availability(false);
}

#[test]
fn dynamic_generation_rejects_replaced_target_when_current_boot_id_is_unavailable() {
    dynamic_generation_rejects_replaced_target_with_boot_availability(true);
}

#[test]
fn target_identity_replacement_after_review_is_revalidated_before_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(Arc::new(AdvancingClock::by(Duration::from_secs(31))));
    daemon.scan_cycle().unwrap();

    let project_for_mutation = project.clone();
    let inspector = MutatingProcessInspector::on_second_call(move || {
        fs::rename(
            project_for_mutation.join("target"),
            project_for_mutation.join("target-reviewed"),
        )
        .unwrap();
        write_file(
            &project_for_mutation.join("target/replacement.bin"),
            &[0; 2048],
        );
    });
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &inspector,
        )
        .unwrap();

    assert_eq!(inspector.calls.load(Ordering::SeqCst), 2);
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/replacement.bin").exists());
}

#[test]
fn project_identity_replacement_after_review_is_revalidated_before_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(Arc::new(AdvancingClock::by(Duration::from_secs(31))));
    daemon.scan_cycle().unwrap();

    let project_for_mutation = project.clone();
    let inspector = MutatingProcessInspector::on_second_call(move || {
        fs::rename(
            &project_for_mutation,
            project_for_mutation.with_extension("reviewed"),
        )
        .unwrap();
        write_file(&project_for_mutation.join("Cargo.toml"), b"[package]\n");
        write_file(
            &project_for_mutation.join("target/replacement.bin"),
            &[0; 2048],
        );
    });
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &inspector,
        )
        .unwrap();

    assert_eq!(inspector.calls.load(Ordering::SeqCst), 2);
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/replacement.bin").exists());
}

#[cfg(unix)]
#[test]
fn target_symlink_swap_after_review_is_rejected_before_cargo() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    let external_target = root.path().join("external-target");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    write_file(&external_target.join("external.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(Arc::new(AdvancingClock::by(Duration::from_secs(31))));
    daemon.scan_cycle().unwrap();

    let project_for_mutation = project.clone();
    let external_for_mutation = external_target.clone();
    let inspector = MutatingProcessInspector::on_second_call(move || {
        fs::rename(
            project_for_mutation.join("target"),
            project_for_mutation.join("target-reviewed"),
        )
        .unwrap();
        symlink(&external_for_mutation, project_for_mutation.join("target")).unwrap();
    });
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &inspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(external_target.join("external.bin").exists());
}

#[test]
fn cross_device_target_change_after_review_is_rejected_before_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(root.path()).unwrap()
        ),
    )
    .unwrap();
    let cfg = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&cfg, &config_path, &EmptyEnvironment).unwrap();
    let identity = Arc::new(SwitchableIdentityProvider {
        boot_phase: AtomicUsize::new(0),
        target_revision: AtomicUsize::new(0),
        cross_device: AtomicUsize::new(0),
    });

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: cfg.scan_dirs.clone(),
            project_dirs: cfg.project_dirs.clone(),
            excludes: cfg.effective_excludes(),
        })
        .with_authority(policy, identity.clone()),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(Arc::new(AdvancingClock::by(Duration::from_secs(31))));
    daemon.scan_cycle().unwrap();

    let identity_for_mutation = identity.clone();
    let inspector = MutatingProcessInspector::on_second_call(move || {
        identity_for_mutation.move_target_to_other_device();
    });
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &inspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/original.bin").exists());
}

#[test]
fn activity_that_appears_after_review_blocks_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(Arc::new(AdvancingClock::by(Duration::from_secs(31))));
    daemon.scan_cycle().unwrap();

    let inspector = ActiveOnSecondInspector {
        calls: AtomicUsize::new(0),
        project: project.canonicalize().unwrap(),
    };
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &inspector,
        )
        .unwrap();

    assert_eq!(inspector.calls.load(Ordering::SeqCst), 2);
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/original.bin").exists());
}

#[test]
fn activity_refresh_reuses_one_enumeration_for_multiple_targets_within_thirty_seconds() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    for project in [&first, &second] {
        write_file(&project.join("Cargo.toml"), b"[package]\n");
        write_file(&project.join("target/original.bin"), &[0; 2048]);
    }

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let now = SystemTime::now();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(Arc::new(FixedClock { now }));
    daemon.scan_cycle().unwrap();
    let inspector = MutatingProcessInspector {
        calls: AtomicUsize::new(0),
        mutate_on_call: usize::MAX,
        mutation: Box::new(|| {}),
    };

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &inspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 2);
    assert_eq!(inspector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 2);
}

#[test]
fn activity_refresh_clock_rollback_reenumerates_and_blocks_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(Arc::new(RollbackClock {
        initial: SystemTime::now(),
        calls: AtomicUsize::new(0),
    }));
    daemon.scan_cycle().unwrap();
    let inspector = ActiveOnSecondInspector {
        calls: AtomicUsize::new(0),
        project: project.canonicalize().unwrap(),
    };

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &inspector,
        )
        .unwrap();

    assert_eq!(inspector.calls.load(Ordering::SeqCst), 2);
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/original.bin").exists());
}

#[test]
fn recent_write_after_review_blocks_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let clock = Arc::new(HookClock::new(SystemTime::now()));
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::from_millis(50),
            ..DaemonOptions::default()
        },
    )
    .with_clock(clock.clone());
    daemon.scan_cycle().unwrap();
    thread::sleep(Duration::from_millis(75));

    let project_for_mutation = project.clone();
    clock.set_now(SystemTime::now());
    clock.on_second_next_call(move || {
        write_file(&project_for_mutation.join("target/new-write.bin"), &[0; 16]);
    });
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::from_millis(50),
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
    assert!(project.join("target/new-write.bin").exists());
}

#[test]
fn different_boot_reauthorizes_current_identity_while_project_remains_in_scope() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(root.path()).unwrap()
        ),
    )
    .unwrap();
    let cfg = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&cfg, &config_path, &EmptyEnvironment).unwrap();
    let identity = Arc::new(SwitchableIdentityProvider {
        boot_phase: AtomicUsize::new(0),
        target_revision: AtomicUsize::new(0),
        cross_device: AtomicUsize::new(0),
    });

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: cfg.scan_dirs.clone(),
            project_dirs: cfg.project_dirs.clone(),
            excludes: cfg.effective_excludes(),
        })
        .with_authority(policy, identity.clone()),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();
    identity.switch_boot();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
}

#[test]
fn cross_boot_reverification_scopes_identity_to_new_boot_for_later_cycles() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(root.path()).unwrap()
        ),
    )
    .unwrap();
    let cfg = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&cfg, &config_path, &EmptyEnvironment).unwrap();
    let identity = Arc::new(SwitchableIdentityProvider {
        boot_phase: AtomicUsize::new(0),
        target_revision: AtomicUsize::new(0),
        cross_device: AtomicUsize::new(0),
    });

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: cfg.scan_dirs.clone(),
            project_dirs: cfg.project_dirs.clone(),
            excludes: cfg.effective_excludes(),
        })
        .with_authority(policy, identity.clone()),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();
    identity.switch_boot();

    let first_boot_b = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(first_boot_b.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    runner.calls.lock().unwrap().clear();

    identity.replace_target_in_same_boot();
    let second_boot_b = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(second_boot_b.cleaned, 0);
    assert_eq!(second_boot_b.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/original.bin").exists());
}

#[test]
fn migrated_v9_cross_boot_refresh_requires_fresh_discovery_before_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(root.path()).unwrap()
        ),
    )
    .unwrap();
    let cfg = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&cfg, &config_path, &EmptyEnvironment).unwrap();
    let identity = Arc::new(SwitchableIdentityProvider {
        boot_phase: AtomicUsize::new(0),
        target_revision: AtomicUsize::new(0),
        cross_device: AtomicUsize::new(0),
    });
    let options = ScannerOptions {
        roots: cfg.scan_dirs.clone(),
        project_dirs: cfg.project_dirs.clone(),
        excludes: cfg.effective_excludes(),
    };

    let db_dir = tempfile::tempdir().unwrap();
    let database = db_dir.path().join("state.db");
    {
        let store = Store::open(&database).unwrap();
        store.migrate().unwrap();
        let refresh_runner = FakeRunner {
            delete_target: true,
            ..FakeRunner::default()
        };
        let daemon = Daemon::new(
            &store,
            Cache::new(&store),
            authoritative_scanner(options.clone()).with_authority(policy.clone(), identity.clone()),
            Cleaner::new("cargo", refresh_runner.clone(), Duration::from_secs(60)),
            DaemonOptions {
                target_quiet_period: Duration::ZERO,
                ..DaemonOptions::default()
            },
        );
        daemon.scan_cycle().unwrap();
        identity.switch_boot();
        let refreshed = daemon
            .run_cycle_with_safety(
                SafetyOptions {
                    target_quiet_period: Duration::ZERO,
                    include_managed_cache: false,
                    include_active: false,
                    force: true,
                },
                &NoopProcessInspector,
            )
            .unwrap();
        assert_eq!(refreshed.cleaned, 1);
        assert_eq!(refresh_runner.calls.lock().unwrap().len(), 1);
        assert!(store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap() >= 2048);
    }

    write_file(&project.join("target/rebuilt.bin"), &[0; 2048]);
    downgrade_runtime_database_to_version_nine(&database);
    identity.replace_target_in_same_boot();

    let store = Store::open(&database).unwrap();
    store.migrate().unwrap();
    assert!(store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap() >= 2048);
    assert_eq!(store.current_generation(policy.hash()).unwrap(), None);

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(options).with_authority(policy.clone(), identity),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    let blocked = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert!(blocked.coverage_incomplete);
    assert_eq!(blocked.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/rebuilt.bin").exists());

    let fresh = daemon.scan_cycle().unwrap();
    let generation = store.current_generation(policy.hash()).unwrap().unwrap();
    assert_eq!(generation.id, fresh.generation);
    let observations = store.authorized_observations(generation.id).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].boot_session_id.as_deref(), Some("boot-b"));

    let allowed = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(allowed.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    assert!(!project.join("target").exists());
}

#[test]
fn changed_scope_has_no_matching_generation_and_never_calls_cargo() {
    let root = tempfile::tempdir().unwrap();
    let old_scope = root.path().join("old-scope");
    let new_scope = root.path().join("new-scope");
    let project = old_scope.join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    fs::create_dir_all(&new_scope).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let scan_daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![old_scope],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );
    scan_daemon.scan_cycle().unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let narrowed = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![new_scope],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    let result = narrowed
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert!(result.coverage_incomplete);
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/original.bin").exists());
}

#[test]
fn policyless_scanner_generation_never_authorizes_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();
    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert!(result.coverage_incomplete);
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/original.bin").exists());
}

#[test]
fn removed_explicit_project_has_no_matching_generation_and_never_calls_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("explicit");
    let retained_root = root.path().join("retained");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    fs::create_dir_all(&retained_root).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let scan_daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![retained_root.clone()],
            project_dirs: vec![project.clone()],
            excludes: vec![],
        }),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );
    scan_daemon.scan_cycle().unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let removed = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![retained_root],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    let result = removed
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert!(result.coverage_incomplete);
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/original.bin").exists());
}

#[test]
fn failed_cargo_clean_is_audited_without_success_or_recovery_accounting() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/removed.bin"), &[0; 2048]);
    write_file(&project.join("target/retained.bin"), &[0; 1024]);
    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&project, SystemTime::now()).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new(
            "cargo",
            FakeRunner {
                delete_relative_path: Some(PathBuf::from("removed.bin")),
                exit_code: 7,
                stderr: "cargo metadata failed".to_string(),
                ..FakeRunner::default()
            },
            Duration::from_secs(60),
        ),
        DaemonOptions {
            target_quiet_period: Duration::from_millis(1),
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::from_millis(1),
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.bytes_recovered, 0);
    assert_eq!(result.errors, 1);
    assert_eq!(result.cargo_failures, 1);
    assert_eq!(result.measurement_failures, 0);
    assert_eq!(result.cleanup_failures, 0);
    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 0);
    assert_eq!(run.bytes_recovered, 0);
    assert_eq!(run.errors_count, 1);
    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].exit_code, 7);
    assert!(events[0].bytes_before > events[0].bytes_after);
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        0
    );
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert!(errors.iter().any(|error| {
        error.category == "clean"
            && error.message.contains("cargo clean exited 7")
            && error.message.contains("cargo metadata failed")
    }));
    assert!(store.all_projects().unwrap()[0].last_cleaned_at.is_none());
}

#[test]
fn post_cargo_measurement_failure_preserves_audit_and_continues_other_projects() {
    let root = tempfile::tempdir().unwrap();
    let unmeasurable = root.path().join("a-unmeasurable");
    let successful = root.path().join("b-successful");
    for project in [&unmeasurable, &successful] {
        write_file(&project.join("Cargo.toml"), b"[package]\n");
        write_file(&project.join("target/blob.bin"), &[0; 2048]);
    }
    let unmeasurable = unmeasurable.canonicalize().unwrap();
    let successful = successful.canonicalize().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let discovered_at = SystemTime::now();
    store.upsert_project(&unmeasurable, discovered_at).unwrap();
    store.upsert_project(&successful, discovered_at).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let runner = FakeRunner {
        delete_target: true,
        replace_target_with_file_for: Some(unmeasurable.clone()),
        exit_code: 0,
        stderr: "cargo completed with a warning".to_string(),
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::from_millis(1),
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::from_millis(1),
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(runner.calls.lock().unwrap().len(), 2);
    assert_eq!(result.cleaned, 1);
    assert_eq!(result.bytes_recovered, 2048);
    assert_eq!(result.errors, 1);
    assert_eq!(result.cargo_failures, 0);
    assert_eq!(result.measurement_failures, 1);
    assert_eq!(result.cleanup_failures, 0);
    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 1);
    assert_eq!(run.bytes_recovered, 2048);
    assert_eq!(run.errors_count, 1);

    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(events.len(), 2);
    let unmeasurable_event = events
        .iter()
        .find(|event| event.path == unmeasurable.to_string_lossy())
        .unwrap();
    assert_eq!(unmeasurable_event.exit_code, 0);
    assert_eq!(
        unmeasurable_event.stderr_excerpt,
        "cargo completed with a warning"
    );
    assert_eq!(
        unmeasurable_event.bytes_after,
        unmeasurable_event.bytes_before
    );

    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].category, "clean");
    assert_eq!(
        errors[0].path.as_deref(),
        Some(unmeasurable.to_string_lossy().as_ref())
    );
    assert!(errors[0]
        .message
        .contains("measure target after cargo clean"));

    let projects = store.all_projects().unwrap();
    assert!(projects
        .iter()
        .find(|project| project.path == unmeasurable.to_string_lossy())
        .unwrap()
        .last_cleaned_at
        .is_none());
    assert!(projects
        .iter()
        .find(|project| project.path == successful.to_string_lossy())
        .unwrap()
        .last_cleaned_at
        .is_some());
}

#[test]
fn daemon_logs_run_cycle_summary() {
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

    let log_path = db_dir.path().join("car-go-clean.log");
    let logger = Logger::with_options(
        &log_path,
        LoggerOptions {
            max_bytes: 1024,
            max_files: 2,
        },
    )
    .unwrap();
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
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        cleaner,
        DaemonOptions::default(),
    )
    .with_logger(logger);
    daemon.scan_cycle().unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    let body = fs::read_to_string(log_path).unwrap();
    let event: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
    assert_eq!(event["message"], "clean cycle complete");
    assert_eq!(event["run_id"], result.run_id);
    assert_eq!(event["cleaned"], 1);
    assert_eq!(event["skipped"], 0);
    assert!(event["bytes_recovered"].as_i64().unwrap() >= 2048);
    assert_eq!(event["errors"], 0);
}

#[test]
fn daemon_uses_persisted_overdue_clean_schedule_after_restart() {
    let _guard = shutdown_test_lock();
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
    let now = std::time::SystemTime::now();
    store
        .record_scheduler_status(
            now,
            now.checked_sub(Duration::from_secs(1)).unwrap(),
            now + Duration::from_secs(60 * 60),
        )
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_secs(60));
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        cleaner,
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::from_secs(60 * 60),
            target_quiet_period: Duration::ZERO,
        },
    );
    let shutdown = ShutdownFlag::new();
    let shutdown_for_thread = shutdown;
    let shutdown_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        shutdown_for_thread.request();
    });

    daemon.run_until_shutdown(&shutdown).unwrap();
    shutdown_thread.join().unwrap();

    assert_eq!(store.last_run().unwrap().projects_cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    let schedule = store.scheduler_status().unwrap().unwrap();
    assert!(schedule.next_clean_at > now);
}

#[test]
fn scheduler_scans_before_cleaning_when_equal_deadlines_are_overdue() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = root.path().join("linked");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("target/blob.bin"), &[0; 2048]);
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_primary, SystemTime::now())
        .unwrap();
    store
        .upsert_project(&canonical_linked, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&canonical_linked))
        .unwrap();
    let now = SystemTime::now();
    let overdue = now.checked_sub(Duration::from_secs(1)).unwrap();
    store
        .record_scheduler_status(now, overdue, overdue)
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::failure("git failed")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::from_secs(60 * 60),
            target_quiet_period: Duration::ZERO,
        },
    );
    let shutdown = ShutdownFlag::new();
    let shutdown_for_thread = shutdown;
    let shutdown_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        shutdown_for_thread.request();
    });

    daemon.run_until_shutdown(&shutdown).unwrap();
    shutdown_thread.join().unwrap();

    assert!(runner.calls.lock().unwrap().is_empty());
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_linked, canonical_primary]
    );
    assert_eq!(store.last_run().unwrap().projects_cleaned, 0);
}

#[test]
fn scheduler_reconciles_successful_exclusions_before_equal_due_clean() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let excluded = root.path().join("excluded/team/worktree");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&excluded.join("Cargo.toml"), b"[workspace]\n");
    write_file(&excluded.join("target/blob.bin"), &[0; 2048]);
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_excluded = excluded.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_primary, SystemTime::now())
        .unwrap();
    store
        .upsert_project(&canonical_excluded, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(
            &canonical_primary,
            std::slice::from_ref(&canonical_excluded),
        )
        .unwrap();
    let now = SystemTime::now();
    let overdue = now.checked_sub(Duration::from_secs(1)).unwrap();
    store
        .record_scheduler_status(now, overdue, overdue)
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec!["excluded/team".to_string()],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![canonical_excluded])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::from_secs(60 * 60),
            target_quiet_period: Duration::ZERO,
        },
    );
    let shutdown = ShutdownFlag::new();
    let shutdown_for_thread = shutdown;
    let shutdown_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        shutdown_for_thread.request();
    });

    daemon.run_until_shutdown(&shutdown).unwrap();
    shutdown_thread.join().unwrap();

    assert!(runner.calls.lock().unwrap().is_empty());
    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
        vec![canonical_primary.to_string_lossy().into_owned()]
    );
    assert_eq!(store.last_run().unwrap().projects_cleaned, 0);
}

#[cfg(unix)]
#[test]
fn scheduler_retries_initial_empty_store_scan_persistence_failure() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER reject_project_upsert
            BEFORE INSERT ON projects
            BEGIN
                SELECT RAISE(FAIL, 'injected project persistence failure');
            END;
            ",
        )
        .unwrap();

    let log_path = db_dir.path().join("car-go-clean.log");
    let logger = Logger::with_options(
        &log_path,
        LoggerOptions {
            max_bytes: 1024,
            max_files: 2,
        },
    )
    .unwrap();
    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::ZERO,
            scan_interval: Duration::ZERO,
            target_quiet_period: Duration::ZERO,
        },
    )
    .with_logger(logger);
    let started = SystemTime::now();
    let shutdown = ShutdownFlag::new();
    let shutdown_for_thread = shutdown;
    let shutdown_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        shutdown_for_thread.request();
    });

    daemon.run_until_shutdown(&shutdown).unwrap();
    shutdown_thread.join().unwrap();

    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(store.last_run().is_err());
    let schedule = store.scheduler_status().unwrap().unwrap();
    assert!(
        schedule
            .next_scan_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            > started
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
    );
    assert!(schedule.next_clean_at >= schedule.next_scan_at);
    let event: serde_json::Value =
        serde_json::from_str(fs::read_to_string(log_path).unwrap().trim()).unwrap();
    assert_eq!(event["level"], "ERROR");
    assert!(event["message"]
        .as_str()
        .unwrap()
        .contains("scan cycle failed; retry scheduled: injected project persistence failure"));
}

#[cfg(unix)]
#[test]
fn scheduler_defers_clean_and_retry_after_scan_persistence_failure() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let linked = root.path().join("linked");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("target/blob.bin"), &[0; 2048]);
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_primary, SystemTime::now())
        .unwrap();
    store
        .upsert_project(&canonical_linked, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, &[canonical_linked])
        .unwrap();
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER reject_discovery_failure
            BEFORE INSERT ON worktree_discovery_failures
            BEGIN
                SELECT RAISE(FAIL, 'injected discovery persistence failure');
            END;
            ",
        )
        .unwrap();
    let now = SystemTime::now();
    let overdue = now.checked_sub(Duration::from_secs(1)).unwrap();
    store
        .record_scheduler_status(now, overdue, overdue)
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::failure("git failed")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::ZERO,
            target_quiet_period: Duration::ZERO,
        },
    );
    let shutdown = ShutdownFlag::new();
    let shutdown_for_thread = shutdown;
    let shutdown_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        shutdown_for_thread.request();
    });

    daemon.run_until_shutdown(&shutdown).unwrap();
    shutdown_thread.join().unwrap();

    assert!(runner.calls.lock().unwrap().is_empty());
    let schedule = store.scheduler_status().unwrap().unwrap();
    assert!(schedule.next_scan_at > now);
    assert!(schedule.next_clean_at >= schedule.next_scan_at);
    assert!(store.last_run().is_err());
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
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        cleaner,
        DaemonOptions::default(),
    );

    daemon.run_cycle().unwrap();

    assert_eq!(store.last_run().unwrap().projects_cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_run_cycle_skips_symlinked_target_even_with_force_compatibility() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    let real_target = root.path().join("real-target");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&real_target.join("blob.bin"), &[0; 2048]);
    symlink(&real_target, project.join("target")).unwrap();

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
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        cleaner,
        DaemonOptions::default(),
    );

    daemon.run_cycle().unwrap();

    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[test]
fn daemon_run_cycle_reports_pathless_scan_error_as_incomplete_coverage() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&project, SystemTime::now()).unwrap();
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: SystemTime::now(),
            category: "scan".to_string(),
            path: None,
            message: "scan failed before resolving an identity".to_string(),
        })
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    assert!(result.coverage_incomplete);
}

#[test]
fn daemon_run_cycle_ignores_scan_errors_older_than_scan_interval() {
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
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: std::time::SystemTime::now()
                .checked_sub(Duration::from_secs(10))
                .unwrap(),
            category: "scan".to_string(),
            path: Some(project.join("target").to_string_lossy().into_owned()),
            message: "transient scan error".to_string(),
        })
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let cleaner = Cleaner::new("cargo", runner.clone(), Duration::from_secs(60));
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        cleaner,
        DaemonOptions {
            clean_interval: Duration::from_secs(60),
            scan_interval: Duration::from_secs(1),
            target_quiet_period: Duration::from_secs(2 * 60 * 60),
        },
    );
    daemon.scan_cycle().unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 1);
    assert_eq!(result.skipped, 0);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
}

#[test]
fn daemon_shutdown_flag_stops_forever_loop_after_initial_scan() {
    let _guard = shutdown_test_lock();
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
    let scanner = authoritative_scanner(ScannerOptions {
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
            target_quiet_period: Duration::from_secs(2 * 60 * 60),
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

    let scanner = authoritative_scanner(ScannerOptions {
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
    assert_eq!(
        errors[0].path.as_deref(),
        Some(blocked.canonicalize().unwrap().to_str().unwrap())
    );
    assert!(errors[0].message.contains("Permission denied"));
}

#[test]
fn daemon_scan_cycle_treats_a_missing_absolute_root_as_completed_and_empty() {
    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let missing_root = db_dir.path().join("missing-root");
    assert!(missing_root.is_absolute());

    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: vec![missing_root.clone()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );

    let result = daemon.scan_cycle().unwrap();
    assert_eq!(result.errors, 0);
    assert_eq!(result.origins.len(), 1);
    assert!(result.origins[0].completed);
    assert!(result.origins[0].canonical_path.is_none());
    assert!(result.origins[0].projects.is_empty());
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert!(errors.is_empty());
}

#[cfg(unix)]
#[test]
fn reconciliation_uncertainty_aborts_before_cargo_without_mutation() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let physical_root = tempfile::tempdir().unwrap();
    let alias_parent = tempfile::tempdir().unwrap();
    let alias_root = alias_parent.path().join("scan-root-alias");
    symlink(physical_root.path(), &alias_root).unwrap();
    let blocked = physical_root.path().join("blocked");
    let project = blocked.join("cached-project");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);
    let canonical_project = project.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_project, SystemTime::now())
        .unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![alias_root],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    let scan = daemon.scan_cycle().unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(scan.errors, 1);
    assert!(!scan.origins[0].completed);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/blob.bin").exists());
    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| PathBuf::from(project.path))
            .collect::<Vec<_>>(),
        vec![canonical_project]
    );
}

#[test]
fn daemon_blocks_cached_linked_worktree_after_discovery_failure_until_success() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = primary.join(".worktrees/feature");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon_options = DaemonOptions {
        target_quiet_period: Duration::ZERO,
        ..DaemonOptions::default()
    };
    let scanner_options = ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    };

    let successful_scan = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options.clone(),
            Arc::new(FakeWorktreeResolver::paths(vec![linked.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );
    successful_scan.scan_cycle().unwrap();

    let failed_scan = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options.clone(),
            Arc::new(FakeWorktreeResolver::failure("git failed")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );
    failed_scan.scan_cycle().unwrap();
    let scan_errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(scan_errors.len(), 1);
    assert_eq!(scan_errors[0].category, "worktree_discovery");
    assert_eq!(scan_errors[0].message, "git failed");
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![
            primary.canonicalize().unwrap(),
            linked.canonicalize().unwrap()
        ]
    );
    let result = failed_scan
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());

    let successful_scan = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options,
            Arc::new(FakeWorktreeResolver::paths(vec![linked])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );
    successful_scan.scan_cycle().unwrap();
    let result = successful_scan
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn daemon_excludes_canonical_git_candidate_beneath_multi_component_exclusion() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let excluded = root.path().join("Library/Caches/team/worktree");
    let alias = root.path().join("worktree-alias");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&excluded.join("Cargo.toml"), b"[workspace]\n");
    write_file(&excluded.join("target/blob.bin"), &[0; 2048]);
    symlink(&excluded, &alias).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&excluded, SystemTime::now()).unwrap();
    store
        .replace_linked_worktrees(
            &primary.canonicalize().unwrap(),
            std::slice::from_ref(&excluded),
        )
        .unwrap();
    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec!["Library/Caches".to_string()],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![excluded.clone(), alias])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
        vec![primary
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned()]
    );

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: true,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn successful_canonical_discovery_clears_alias_keyed_failure_and_stale_provenance() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let alias = root.path().join("legacy-router-alias");
    let stale = root.path().join("stale-linked");
    let current = root.path().join("current-linked");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&stale.join("Cargo.toml"), b"[workspace]\n");
    write_file(&current.join("Cargo.toml"), b"[workspace]\n");
    write_file(&current.join("target/blob.bin"), &[0; 2048]);
    symlink(&primary, &alias).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&alias, SystemTime::now()).unwrap();
    store
        .replace_linked_worktrees(&alias, std::slice::from_ref(&stale))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&alias, SystemTime::now(), "legacy failure")
        .unwrap();
    fs::remove_file(&alias).unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![current.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();

    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
    assert!(!store
        .all_projects()
        .unwrap()
        .iter()
        .any(|project| project.path == alias.to_string_lossy()));

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    assert_eq!(
        runner.calls.lock().unwrap()[0].dir,
        current.canonicalize().unwrap()
    );

    let canonical_primary = primary.canonicalize().unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "new failure")
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![current.canonicalize().unwrap(), canonical_primary]
    );
}

#[cfg(unix)]
#[test]
fn daemon_success_at_retargeted_primary_alias_destination_preserves_original_failure() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    let replacement = root.path().join("replacement");
    let alias = root.path().join("primary-alias");
    let child = root.path().join("original-child");
    for primary in [&original, &replacement] {
        fs::create_dir_all(primary.join(".git")).unwrap();
        write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    }
    write_file(&child.join("Cargo.toml"), b"[workspace]\n");
    write_file(&child.join("target/blob.bin"), &[0; 2048]);
    symlink(&original, &alias).unwrap();
    let canonical_original = original.canonicalize().unwrap();
    let canonical_replacement = replacement.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_child, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&alias, std::slice::from_ref(&canonical_child))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&alias, SystemTime::now(), "original failure")
        .unwrap();
    fs::remove_file(&alias).unwrap();
    symlink(&replacement, &alias).unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![],
                project_dirs: vec![canonical_replacement.clone()],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    let mut expected_blocks = vec![canonical_child.clone(), canonical_original];
    expected_blocks.sort();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        expected_blocks
    );
    assert!(store
        .all_projects()
        .unwrap()
        .iter()
        .any(|project| project.path == canonical_child.to_string_lossy()));

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_migrated_broken_primary_alias_stays_fail_closed_after_primary_success() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let alias = root.path().join("legacy-primary-alias");
    let child = root.path().join("child");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&child.join("Cargo.toml"), b"[workspace]\n");
    write_file(&child.join("target/blob.bin"), &[0; 2048]);
    symlink(&primary, &alias).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    {
        let store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
        store
            .upsert_project(&canonical_child, SystemTime::now())
            .unwrap();
        store
            .replace_linked_worktrees(&alias, std::slice::from_ref(&canonical_child))
            .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "
            DROP TABLE worktree_discovery_failures;
            CREATE TABLE worktree_discovery_failures (
                primary_path TEXT PRIMARY KEY,
                failed_at INTEGER NOT NULL,
                message TEXT NOT NULL
            );
            DELETE FROM schema_version WHERE version >= 5;
            ",
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
    }
    fs::remove_file(&alias).unwrap();

    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![],
                project_dirs: vec![canonical_primary.clone()],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    let blocked = store.blocked_worktree_discovery_paths().unwrap();
    assert!(blocked.contains(&canonical_primary));
    assert!(blocked.contains(&canonical_child));

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
fn assert_daemon_blocks_v4_alias_association_after_fresh_failure(retarget: bool) {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let replacement = root.path().join("replacement");
    let alias = root.path().join("legacy-primary-alias");
    let child = root.path().join("child");
    for checkout in [&primary, &replacement] {
        fs::create_dir_all(checkout.join(".git")).unwrap();
        write_file(&checkout.join("Cargo.toml"), b"[workspace]\n");
    }
    write_file(&child.join("Cargo.toml"), b"[workspace]\n");
    write_file(&child.join("target/blob.bin"), &[0; 2048]);
    symlink(&primary, &alias).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    {
        let store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
        store
            .upsert_project(&canonical_child, SystemTime::now())
            .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "
            DROP TABLE linked_worktrees;
            CREATE TABLE linked_worktrees (
                primary_path TEXT NOT NULL,
                linked_path TEXT NOT NULL,
                PRIMARY KEY (primary_path, linked_path)
            );
            CREATE INDEX idx_linked_worktrees_linked
                ON linked_worktrees(linked_path);
            DROP TABLE worktree_discovery_failures;
            CREATE TABLE worktree_discovery_failures (
                primary_path TEXT PRIMARY KEY,
                failed_at INTEGER NOT NULL,
                message TEXT NOT NULL
            );
            DELETE FROM schema_version WHERE version >= 5;
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
            rusqlite::params![alias.to_str().unwrap(), canonical_child.to_str().unwrap()],
        )
        .unwrap();
    }

    fs::remove_file(&alias).unwrap();
    if retarget {
        symlink(&replacement, &alias).unwrap();
    }

    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    store
        .mark_worktree_discovery_failed(
            &canonical_primary,
            SystemTime::now(),
            "fresh canonical failure",
        )
        .unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: vec![],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(child.join("target/blob.bin").exists());
    assert!(store
        .blocked_worktree_discovery_paths()
        .unwrap()
        .contains(&canonical_child));
}

#[cfg(unix)]
#[test]
fn daemon_blocks_v4_broken_primary_association_after_fresh_failure() {
    assert_daemon_blocks_v4_alias_association_after_fresh_failure(false);
}

#[cfg(unix)]
#[test]
fn daemon_blocks_v4_retargeted_primary_association_after_fresh_failure() {
    assert_daemon_blocks_v4_alias_association_after_fresh_failure(true);
}

#[cfg(unix)]
#[test]
fn failed_discovery_blocks_canonical_child_from_alias_only_provenance() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = root.path().join("linked");
    let linked_alias = root.path().join("linked-alias");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("target/blob.bin"), &[0; 2048]);
    symlink(&linked, &linked_alias).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_linked, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&linked_alias))
        .unwrap();

    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::failure("git failed")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();
    let mut expected_blocks = vec![canonical_linked, canonical_primary, linked_alias];
    expected_blocks.sort();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        expected_blocks
    );

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn broken_alias_in_active_provenance_blocks_cleanup_until_successful_discovery() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = root.path().join("linked");
    let linked_alias = root.path().join("linked-alias");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("target/blob.bin"), &[0; 2048]);
    symlink(&linked, &linked_alias).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_linked, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&linked_alias))
        .unwrap();
    store
        .mark_worktree_discovery_failed(
            &canonical_primary,
            SystemTime::now(),
            "active legacy failure",
        )
        .unwrap();
    fs::remove_file(&linked_alias).unwrap();

    let runner = FakeRunner::default();
    let daemon_options = DaemonOptions {
        target_quiet_period: Duration::ZERO,
        ..DaemonOptions::default()
    };
    let scanner_options = ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    };
    let failed_state = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options.clone(),
            Arc::new(FakeWorktreeResolver::failure("still failing")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );

    let result = failed_state
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 0);
    assert!(runner.calls.lock().unwrap().is_empty());

    let forced = failed_state
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(forced.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    runner.calls.lock().unwrap().clear();

    symlink(&linked, &linked_alias).unwrap();
    let repaired_state = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options,
            Arc::new(FakeWorktreeResolver::paths(vec![linked.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );
    repaired_state.scan_cycle().unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());

    let repaired = repaired_state
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(repaired.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn retargeted_alias_in_active_provenance_blocks_cleanup_until_successful_discovery() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let unrelated_root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = root.path().join("linked");
    let unrelated = unrelated_root.path().join("unrelated");
    let linked_alias = root.path().join("linked-alias");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("target/blob.bin"), &[0; 2048]);
    write_file(&unrelated.join("Cargo.toml"), b"[workspace]\n");
    symlink(&linked, &linked_alias).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_linked, SystemTime::now())
        .unwrap();
    store
        .upsert_project(&linked_alias, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&linked_alias))
        .unwrap();
    store
        .mark_worktree_discovery_failed(
            &canonical_primary,
            SystemTime::now(),
            "active legacy failure",
        )
        .unwrap();
    fs::remove_file(&linked_alias).unwrap();
    symlink(&unrelated, &linked_alias).unwrap();

    let runner = FakeRunner::default();
    let daemon_options = DaemonOptions {
        target_quiet_period: Duration::ZERO,
        ..DaemonOptions::default()
    };
    let scanner_options = ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    };
    let failed_state = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options.clone(),
            Arc::new(FakeWorktreeResolver::failure("still failing")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );

    let result = failed_state
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    let blocked = store.blocked_worktree_discovery_paths().unwrap();
    assert!(blocked.contains(&linked_alias));
    assert!(!blocked.contains(&unrelated.canonicalize().unwrap()));

    let repaired_state = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options,
            Arc::new(FakeWorktreeResolver::paths(vec![linked.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );
    repaired_state.scan_cycle().unwrap();
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());

    let repaired = repaired_state
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(repaired.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
}

#[test]
fn daemon_durably_blocks_exact_primary_and_saved_linked_paths_without_recent_scan_error() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = primary.join(".worktrees/feature");
    for project in [&primary, &linked] {
        write_file(&project.join("Cargo.toml"), b"[workspace]\n");
        write_file(&project.join("target/blob.bin"), &[0; 2048]);
    }
    fs::create_dir_all(primary.join(".git")).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon_options = DaemonOptions {
        target_quiet_period: Duration::ZERO,
        ..DaemonOptions::default()
    };
    let scanner_options = ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options,
            Arc::new(FakeWorktreeResolver::paths(vec![linked.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );
    daemon.scan_cycle().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "git failed")
        .unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn daemon_non_utf8_discovery_failure_preserves_prior_association_and_block() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let saved = primary.join(".worktrees/saved");
    let non_utf8 = primary
        .join(".worktrees")
        .join(OsString::from_vec(b"\xff".to_vec()));
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&saved.join("Cargo.toml"), b"[workspace]\n");

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let scanner_options = ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    };
    let successful = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options.clone(),
            Arc::new(FakeWorktreeResolver::paths(vec![saved.clone()])),
        ),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );
    successful.scan_cycle().unwrap();

    let rejected = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            scanner_options,
            Arc::new(FakeWorktreeResolver::paths(vec![non_utf8])),
        ),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );
    rejected.scan_cycle().unwrap();

    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![
            primary.canonicalize().unwrap(),
            saved.canonicalize().unwrap()
        ]
    );
}

#[cfg(unix)]
#[test]
fn malformed_successful_git_output_preserves_prior_association_and_failure() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = primary.join(".worktrees/saved");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let options = ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    };
    let successful = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            options.clone(),
            Arc::new(FakeWorktreeResolver::paths(vec![linked.clone()])),
        ),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );
    successful.scan_cycle().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "prior failure")
        .unwrap();

    let mut malformed = b"worktree ".to_vec();
    malformed.extend_from_slice(canonical_primary.as_os_str().as_encoded_bytes());
    malformed.extend_from_slice(b"\0\0");
    let malformed_scan = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            options,
            Arc::new(SuccessfulOutputResolver { stdout: malformed }),
        ),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );

    malformed_scan.scan_cycle().unwrap();

    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_primary, canonical_linked]
    );
}

#[test]
fn daemon_cache_sync_does_not_clear_failed_association_when_primary_disappears() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = root.path().join("feature");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), b"[workspace]\n");
    write_file(&root.path().join(".gitignore"), b"feature/\n");
    write_file(&linked.join("Cargo.toml"), b"[workspace]\n");
    write_file(&linked.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![linked.clone()])),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "git failed")
        .unwrap();
    fs::remove_dir_all(&primary).unwrap();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_linked, canonical_primary]
    );
}

#[test]
fn daemon_defaults_to_daily_scans() {
    assert_eq!(
        DaemonOptions::default().scan_interval,
        Duration::from_secs(24 * 60 * 60)
    );
}

#[test]
fn successful_scan_prunes_cached_excluded_candidates_and_failures() {
    let root = tempfile::tempdir().unwrap();
    let excluded_root = root.path().join("OrbStack");
    let excluded = excluded_root.join("docker/volumes/copied-crate");
    let kept = root.path().join("src/kept");
    fs::create_dir_all(excluded.join(".git")).unwrap();
    write_file(&excluded.join("Cargo.toml"), b"[package]\nname='copied'\n");
    write_file(&kept.join("Cargo.toml"), b"[package]\nname='kept'\n");
    write_file(&kept.join("target/blob.bin"), &[0; 2048]);
    let excluded = excluded.canonicalize().unwrap();
    let kept = kept.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    store.upsert_project(&excluded, now).unwrap();
    store
        .mark_worktree_discovery_failed(&excluded, now, "old dubious ownership")
        .unwrap();
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "worktree_discovery".to_string(),
            path: Some(excluded.to_string_lossy().into_owned()),
            message: "historical dubious ownership".to_string(),
        })
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![excluded_root.to_string_lossy().into_owned()],
            },
            Arc::new(FakeWorktreeResolver::failure(
                "excluded Git repository was inspected",
            )),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    daemon.scan_cycle().unwrap();

    assert_eq!(store.all_projects().unwrap().len(), 2);
    assert!(!store.blocked_worktree_discovery_paths().unwrap().is_empty());
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 1);

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();
    assert_eq!(result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    assert_eq!(runner.calls.lock().unwrap()[0].dir, kept);
}

#[cfg(unix)]
#[test]
fn successful_scan_reconciles_removed_exclusion_through_symlinked_home() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let physical_home = root.path().join("physical-home");
    let symlinked_home = root.path().join("home");
    let physical_excluded_root = physical_home.join("OrbStack");
    let excluded_primary = physical_excluded_root.join("primary");
    let excluded_linked = physical_excluded_root.join("linked");
    fs::create_dir_all(&excluded_primary).unwrap();
    fs::create_dir_all(&excluded_linked).unwrap();
    symlink(&physical_home, &symlinked_home).unwrap();
    let canonical_primary = excluded_primary.canonicalize().unwrap();
    let canonical_linked = excluded_linked.canonicalize().unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    store.upsert_project(&canonical_primary, now).unwrap();
    store.upsert_project(&canonical_linked, now).unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&canonical_linked))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, now, "stale failure")
        .unwrap();

    fs::remove_dir_all(&physical_excluded_root).unwrap();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![physical_home],
            project_dirs: vec![],
            excludes: vec![symlinked_home
                .join("OrbStack")
                .to_string_lossy()
                .into_owned()],
        }),
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions::default(),
    );
    fs::remove_file(&symlinked_home).unwrap();

    daemon.scan_cycle().unwrap();

    assert_eq!(store.all_projects().unwrap().len(), 2);
    assert!(store
        .is_active_worktree_discovery_identity(&canonical_primary)
        .unwrap());
    assert!(store
        .is_active_worktree_discovery_identity(&canonical_linked)
        .unwrap());
    assert!(!store.blocked_worktree_discovery_paths().unwrap().is_empty());
}

#[test]
fn daemon_clamps_only_legacy_scan_deadlines_beyond_the_current_interval() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let interval = Duration::from_secs(24 * 60 * 60);
    let old_deadline = now + Duration::from_secs(6 * 24 * 60 * 60);
    let earlier_deadline = now + Duration::from_secs(60 * 60);

    assert_eq!(
        clamp_next_scan_at(old_deadline, now, interval),
        now + interval
    );
    assert_eq!(
        clamp_next_scan_at(earlier_deadline, now, interval),
        earlier_deadline
    );
}

#[test]
fn forced_scan_overrides_a_distant_deadline_and_records_the_attempt() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(50_000);
    store
        .record_scheduler_status(
            now,
            now + Duration::from_secs(60 * 60),
            now + Duration::from_secs(60 * 60),
        )
        .unwrap();
    let scanner = authoritative_scanner(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        scanner,
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::from_secs(60 * 60),
            target_quiet_period: Duration::ZERO,
        },
    )
    .with_clock(Arc::new(FixedClock { now }));
    let shutdown = ShutdownFlag::new();

    daemon.run_until_shutdown(&shutdown).unwrap();

    assert_eq!(store.all_projects().unwrap().len(), 1);
    assert_eq!(store.last_forced_scan_at().unwrap(), Some(now));
}

#[test]
fn forced_scan_restart_inside_five_minutes_does_not_scan_again() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(50_000);
    let last_attempt = now - Duration::from_secs(60);
    store
        .record_scheduler_status(
            now,
            now + Duration::from_secs(60 * 60),
            now + Duration::from_secs(60 * 60),
        )
        .unwrap();
    store.record_forced_scan_at(last_attempt).unwrap();
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER reject_project_upsert
            BEFORE INSERT ON projects
            BEGIN
                SELECT RAISE(FAIL, 'unexpected forced scan');
            END;
            ",
        )
        .unwrap();
    let scanner = authoritative_scanner(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        scanner,
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::from_secs(60 * 60),
            target_quiet_period: Duration::ZERO,
        },
    )
    .with_clock(Arc::new(FixedClock { now }));
    let shutdown = ShutdownFlag::new();

    daemon.run_until_shutdown(&shutdown).unwrap();

    assert!(store
        .errors_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .is_empty());
    assert_eq!(store.last_forced_scan_at().unwrap(), Some(last_attempt));
    assert_eq!(
        store.scheduler_status().unwrap().unwrap().next_scan_at,
        last_attempt + Duration::from_secs(5 * 60)
    );
}

#[test]
fn forced_scan_clock_rollback_does_not_bypass_the_persisted_guard() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(50_000);
    let future_attempt = now + Duration::from_secs(60);
    store
        .record_scheduler_status(
            now,
            now + Duration::from_secs(60 * 60),
            now + Duration::from_secs(60 * 60),
        )
        .unwrap();
    store.record_forced_scan_at(future_attempt).unwrap();
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER reject_project_upsert_after_rollback
            BEFORE INSERT ON projects
            BEGIN
                SELECT RAISE(FAIL, 'clock rollback bypassed the guard');
            END;
            ",
        )
        .unwrap();
    let scanner = authoritative_scanner(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        scanner,
        Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::from_secs(60 * 60),
            target_quiet_period: Duration::ZERO,
        },
    )
    .with_clock(Arc::new(FixedClock { now }));
    let shutdown = ShutdownFlag::new();

    daemon.run_until_shutdown(&shutdown).unwrap();

    assert!(store
        .errors_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .is_empty());
    assert_eq!(store.last_forced_scan_at().unwrap(), Some(future_attempt));
    assert_eq!(
        store.scheduler_status().unwrap().unwrap().next_scan_at,
        future_attempt + Duration::from_secs(5 * 60)
    );
}

#[test]
fn forced_scan_rate_limit_keeps_missing_generation_cleanup_incomplete() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(50_000);
    let last_attempt = now - Duration::from_secs(60);
    store
        .record_scheduler_status(
            now,
            now - Duration::from_secs(1),
            now + Duration::from_secs(60 * 60),
        )
        .unwrap();
    store.record_forced_scan_at(last_attempt).unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let scanner = authoritative_scanner(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        scanner,
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::from_secs(60 * 60),
            target_quiet_period: Duration::ZERO,
        },
    )
    .with_clock(Arc::new(FixedClock { now }));
    let shutdown = ShutdownFlag::new();

    daemon.run_until_shutdown(&shutdown).unwrap();

    assert_eq!(store.last_run().unwrap().projects_cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/blob.bin").exists());
    assert_eq!(
        store.scheduler_status().unwrap().unwrap().next_scan_at,
        last_attempt + Duration::from_secs(5 * 60)
    );
}

#[test]
fn forced_scan_failure_backoff_survives_the_five_minute_guard() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("state.db");
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER reject_forced_scan_project_upsert
            BEFORE INSERT ON projects
            BEGIN
                SELECT RAISE(FAIL, 'injected forced scan failure');
            END;
            ",
        )
        .unwrap();
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(50_000);
    store
        .record_scheduler_status(
            started,
            started + Duration::from_secs(60 * 60),
            started + Duration::from_secs(60 * 60),
        )
        .unwrap();
    let run_at = |now| {
        let daemon = Daemon::new(
            &store,
            Cache::new(&store),
            authoritative_scanner(ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            }),
            Cleaner::new("cargo", FakeRunner::default(), Duration::from_secs(60)),
            DaemonOptions {
                clean_interval: Duration::from_secs(60 * 60),
                scan_interval: Duration::from_secs(60 * 60),
                target_quiet_period: Duration::ZERO,
            },
        )
        .with_clock(Arc::new(FixedClock { now }));
        daemon.run_until_shutdown(&ShutdownFlag::new()).unwrap();
    };

    run_at(started);
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 1);
    assert_eq!(store.last_forced_scan_at().unwrap(), Some(started));
    let failure_deadline = started + Duration::from_secs(60 * 60);
    assert_eq!(
        store.scheduler_status().unwrap().unwrap().next_scan_at,
        failure_deadline
    );

    run_at(started + Duration::from_secs(5 * 60));
    assert_eq!(
        store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(),
        1,
        "the five-minute guard must not shorten the one-hour failure backoff"
    );
    assert_eq!(store.last_forced_scan_at().unwrap(), Some(started));
    assert_eq!(
        store.scheduler_status().unwrap().unwrap().next_scan_at,
        failure_deadline
    );

    run_at(failure_deadline);
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 2);
    assert_eq!(store.last_forced_scan_at().unwrap(), Some(failure_deadline));
    assert_eq!(
        store.scheduler_status().unwrap().unwrap().next_scan_at,
        failure_deadline + Duration::from_secs(60 * 60)
    );
}

#[cfg(unix)]
#[test]
fn cleanup_boundary_prunes_alias_of_excluded_library_before_cargo() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let library = root.path().join("Library");
    let physical = library.join("copied-crate");
    let alias = root.path().join("legacy-crate");
    write_file(&physical.join("Cargo.toml"), b"[workspace]\n");
    write_file(&physical.join("target/blob.bin"), &[0; 2048]);
    symlink(&physical, &alias).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&alias, SystemTime::now()).unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: vec![],
            project_dirs: vec![],
            excludes: vec![library.to_string_lossy().into_owned()],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(physical.join("target/blob.bin").exists());
    assert_eq!(store.all_projects().unwrap().len(), 1);
}

#[test]
fn upgraded_nonempty_cache_prunes_exclusions_when_clean_is_due_before_scan() {
    let _guard = shutdown_test_lock();
    let root = tempfile::tempdir().unwrap();
    let library_project = root.path().join("Library/Caches/copied-crate");
    let orbstack_project = root.path().join("OrbStack/docker/copied-crate");
    let ordinary_project = root.path().join("src/ordinary");
    for project in [&library_project, &orbstack_project, &ordinary_project] {
        write_file(&project.join("Cargo.toml"), b"[workspace]\n");
        write_file(&project.join("target/blob.bin"), &[0; 2048]);
    }

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    for project in [&library_project, &orbstack_project, &ordinary_project] {
        store.upsert_project(project, SystemTime::now()).unwrap();
    }
    let now = SystemTime::now();
    store
        .record_scheduler_status(
            now,
            now.checked_sub(Duration::from_secs(1)).unwrap(),
            now + Duration::from_secs(60 * 60),
        )
        .unwrap();

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![
                root.path().join("Library").to_string_lossy().into_owned(),
                root.path().join("OrbStack").to_string_lossy().into_owned(),
            ],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            clean_interval: Duration::from_secs(60 * 60),
            scan_interval: Duration::from_secs(60 * 60),
            target_quiet_period: Duration::ZERO,
        },
    );
    let shutdown = ShutdownFlag::new();
    let shutdown_for_thread = shutdown;
    let shutdown_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        shutdown_for_thread.request();
    });

    daemon.run_until_shutdown(&shutdown).unwrap();
    shutdown_thread.join().unwrap();

    assert_eq!(
        runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.dir.clone())
            .collect::<Vec<_>>(),
        vec![ordinary_project.canonicalize().unwrap()]
    );
    assert!(library_project.join("target/blob.bin").exists());
    assert!(orbstack_project.join("target/blob.bin").exists());
    assert!(!ordinary_project.join("target").exists());
    assert_eq!(store.all_projects().unwrap().len(), 3);
    assert_eq!(store.last_run().unwrap().projects_cleaned, 1);
}

#[test]
fn daemon_reloads_scope_for_each_cycle_and_never_uses_obsolete_authority() {
    let work = tempfile::tempdir().unwrap();
    let old_scope = work.path().join("old-scope");
    let new_scope = work.path().join("new-scope");
    let project = old_scope.join("project");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);
    fs::create_dir_all(&new_scope).unwrap();
    let config_path = work.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(&old_scope).unwrap()
        ),
    )
    .unwrap();

    let initial = FileCycleFactory {
        config_path: config_path.clone(),
    }
    .snapshot()
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        initial.scanner().clone(),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        initial.options(),
    )
    .with_cycle_factory(Arc::new(FileCycleFactory {
        config_path: config_path.clone(),
    }));

    daemon.scan_cycle().unwrap();
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(&new_scope).unwrap()
        ),
    )
    .unwrap();

    let second_cycle = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert!(second_cycle.coverage_incomplete);
    assert_eq!(second_cycle.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/blob.bin").exists());
}

#[cfg(unix)]
#[test]
fn daemon_reloads_absolute_exclusion_aliases_for_each_cycle() {
    use std::os::unix::fs::symlink;

    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let initially_excluded = work.path().join("initially-excluded");
    let newly_excluded = root.join("newly-excluded");
    let project = newly_excluded.join("project");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);
    fs::create_dir_all(&initially_excluded).unwrap();
    let exclusion_alias = work.path().join("excluded-alias");
    symlink(&initially_excluded, &exclusion_alias).unwrap();
    let config_path = work.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = [{}]\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(&root).unwrap(),
            serde_json::to_string(&exclusion_alias.to_string_lossy()).unwrap()
        ),
    )
    .unwrap();

    let initial = FileCycleFactory {
        config_path: config_path.clone(),
    }
    .snapshot()
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        initial.scanner().clone(),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        initial.options(),
    )
    .with_cycle_factory(Arc::new(FileCycleFactory {
        config_path: config_path.clone(),
    }));

    daemon.scan_cycle().unwrap();
    fs::remove_file(&exclusion_alias).unwrap();
    symlink(&newly_excluded, &exclusion_alias).unwrap();

    let second_cycle = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert!(second_cycle.coverage_incomplete);
    assert_eq!(second_cycle.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/blob.bin").exists());
}

#[test]
fn persisted_incomplete_origin_outlives_transient_scan_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let scan_root = root.path().join("scan-root");
    fs::create_dir_all(&scan_root).unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    let clock = Arc::new(HookClock::new(now));
    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![scan_root.clone()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            scan_interval: Duration::from_secs(60),
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(clock.clone());

    fs::remove_dir_all(&scan_root).unwrap();
    let scan = daemon.scan_cycle().unwrap();
    assert!(scan.origins.iter().any(|origin| !origin.completed));
    clock.set_now(now + Duration::from_secs(10 * 60));

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert!(result.coverage_incomplete);
    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn cross_boot_ancestor_symlink_retarget_is_never_reauthorized() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let scope = root.path().join("scope");
    let ancestor = scope.join("ancestor");
    let project = ancestor.join("project");
    let outside = root.path().join("outside");
    let outside_project = outside.join("project");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    write_file(&outside_project.join("Cargo.toml"), b"[package]\n");
    write_file(&outside_project.join("target/outside.bin"), &[0; 2048]);
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(&scope).unwrap()
        ),
    )
    .unwrap();
    let cfg = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&cfg, &config_path, &EmptyEnvironment).unwrap();
    let identity = Arc::new(SwitchableIdentityProvider {
        boot_phase: AtomicUsize::new(0),
        target_revision: AtomicUsize::new(0),
        cross_device: AtomicUsize::new(0),
    });
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: cfg.scan_dirs.clone(),
            project_dirs: vec![],
            excludes: vec![],
        })
        .with_authority(policy, identity.clone()),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();

    fs::rename(&ancestor, scope.join("ancestor-boot-a")).unwrap();
    symlink(&outside, &ancestor).unwrap();
    identity.switch_boot();

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(outside_project.join("target/outside.bin").exists());
}

#[cfg(unix)]
#[test]
fn ancestor_symlink_mutation_after_review_is_rejected_before_cargo() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let scope = root.path().join("scope");
    let ancestor = scope.join("ancestor");
    let project = ancestor.join("project");
    let outside = root.path().join("outside");
    let outside_project = outside.join("project");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/original.bin"), &[0; 2048]);
    write_file(&outside_project.join("Cargo.toml"), b"[package]\n");
    write_file(&outside_project.join("target/outside.bin"), &[0; 2048]);
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(&scope).unwrap()
        ),
    )
    .unwrap();
    let cfg = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&cfg, &config_path, &EmptyEnvironment).unwrap();
    let identity = Arc::new(SwitchableIdentityProvider {
        boot_phase: AtomicUsize::new(0),
        target_revision: AtomicUsize::new(0),
        cross_device: AtomicUsize::new(0),
    });
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: cfg.scan_dirs.clone(),
            project_dirs: vec![],
            excludes: vec![],
        })
        .with_authority(policy, identity),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_clock(Arc::new(AdvancingClock::by(Duration::from_secs(31))));
    daemon.scan_cycle().unwrap();

    let ancestor_for_mutation = ancestor.clone();
    let scope_for_mutation = scope.clone();
    let outside_for_mutation = outside.clone();
    let inspector = MutatingProcessInspector::on_second_call(move || {
        fs::rename(
            &ancestor_for_mutation,
            scope_for_mutation.join("ancestor-reviewed"),
        )
        .unwrap();
        symlink(&outside_for_mutation, &ancestor_for_mutation).unwrap();
    });

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &inspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(outside_project.join("target/outside.bin").exists());
}

#[derive(Clone)]
struct SequencedRunner {
    events: Arc<Mutex<Vec<String>>>,
    exit_code: i32,
}

impl CommandRunner for SequencedRunner {
    fn run(&self, dir: &Path, _cmd: &mut Command) -> anyhow::Result<CleanOutcome> {
        self.events
            .lock()
            .unwrap()
            .push(format!("cargo:{}", dir.display()));
        Ok(CleanOutcome {
            exit_code: self.exit_code,
            stderr: if self.exit_code != 0 {
                "synthetic cargo failure"
            } else {
                ""
            }
            .to_string(),
        })
    }
}

fn cleanable_review(path: &Path) -> ProjectReview {
    let safety = SafetyOptions {
        target_quiet_period: Duration::ZERO,
        include_managed_cache: false,
        include_active: false,
        force: false,
    };
    let review = review_project_with_identity_provider(
        path,
        &[],
        &[],
        &[],
        SystemTime::now(),
        &safety,
        &SystemIdentityProvider,
    )
    .unwrap();
    assert_eq!(review.decision, CleanDecision::Cleanable);
    review
}

#[test]
fn exact_review_execution_never_appends_scanner_candidates() {
    let root = tempfile::tempdir().unwrap();
    let included = root.path().join("included");
    let not_in_plan = root.path().join("not-in-plan");
    for project in [&included, &not_in_plan] {
        write_file(&project.join("Cargo.toml"), b"[workspace]\n");
        write_file(&project.join("target/blob.bin"), &[0; 2048]);
    }
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    daemon.scan_cycle().unwrap();
    let review = cleanable_review(&included.canonicalize().unwrap());

    let result = daemon
        .execute_reviews_with_safety(
            vec![review],
            false,
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
            RunSource::Reviewed,
        )
        .unwrap();

    assert_eq!(result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    assert_eq!(
        runner.calls.lock().unwrap()[0].dir,
        included.canonicalize().unwrap()
    );
}

#[test]
fn persisted_review_reauthorizes_exact_path_across_boot_with_fresh_identity() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);
    let project = project.canonicalize().unwrap();
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(root.path()).unwrap()
        ),
    )
    .unwrap();
    let cfg = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&cfg, &config_path, &EmptyEnvironment).unwrap();
    let identity = Arc::new(SwitchableIdentityProvider {
        boot_phase: AtomicUsize::new(0),
        target_revision: AtomicUsize::new(0),
        cross_device: AtomicUsize::new(0),
    });
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: cfg.scan_dirs.clone(),
            project_dirs: vec![],
            excludes: vec![],
        })
        .with_authority(policy.clone(), identity.clone()),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    let generation = daemon.scan_cycle().unwrap();
    let persisted = review_project_with_identity_provider(
        &project,
        &[],
        &[],
        &[],
        SystemTime::now(),
        &SafetyOptions {
            target_quiet_period: Duration::ZERO,
            include_managed_cache: false,
            include_active: false,
            force: false,
        },
        identity.as_ref(),
    )
    .unwrap();
    let plan = store
        .create_review_plan(
            SystemTime::now(),
            policy.hash(),
            generation.generation,
            false,
            persisted.target_bytes as i64,
            &[persisted],
        )
        .unwrap();
    let loaded = store
        .load_review_plan(
            plan.id,
            SystemTime::now(),
            policy.hash(),
            generation.generation,
        )
        .unwrap();
    assert_eq!(
        loaded.targets[0]
            .review
            .reviewed_identity
            .as_ref()
            .unwrap()
            .boot_session,
        Some(BootSessionId("boot-a".to_string()))
    );
    identity.switch_boot();

    let result = daemon
        .execute_reviews_with_safety(
            loaded
                .targets
                .into_iter()
                .map(|target| target.review)
                .collect(),
            false,
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
            RunSource::Reviewed,
        )
        .unwrap();

    assert_eq!(result.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    assert!(!project.join("target").exists());
}

#[test]
fn persisted_review_rejects_same_boot_identity_replacement_without_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);
    let project = project.canonicalize().unwrap();
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            serde_json::to_string(root.path()).unwrap()
        ),
    )
    .unwrap();
    let cfg = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&cfg, &config_path, &EmptyEnvironment).unwrap();
    let identity = Arc::new(SwitchableIdentityProvider {
        boot_phase: AtomicUsize::new(0),
        target_revision: AtomicUsize::new(0),
        cross_device: AtomicUsize::new(0),
    });
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: cfg.scan_dirs.clone(),
            project_dirs: vec![],
            excludes: vec![],
        })
        .with_authority(policy.clone(), identity.clone()),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );
    let generation = daemon.scan_cycle().unwrap();
    let persisted = review_project_with_identity_provider(
        &project,
        &[],
        &[],
        &[],
        SystemTime::now(),
        &SafetyOptions {
            target_quiet_period: Duration::ZERO,
            include_managed_cache: false,
            include_active: false,
            force: false,
        },
        identity.as_ref(),
    )
    .unwrap();
    let plan = store
        .create_review_plan(
            SystemTime::now(),
            policy.hash(),
            generation.generation,
            false,
            persisted.target_bytes as i64,
            &[persisted],
        )
        .unwrap();
    let loaded = store
        .load_review_plan(
            plan.id,
            SystemTime::now(),
            policy.hash(),
            generation.generation,
        )
        .unwrap();
    identity.replace_target_in_same_boot();

    let result = daemon
        .execute_reviews_with_safety(
            loaded
                .targets
                .into_iter()
                .map(|target| target.review)
                .collect(),
            false,
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
            RunSource::Reviewed,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/blob.bin").exists());
}

#[test]
fn execution_reports_target_before_cargo_and_continues_after_failure() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    for project in [&first, &second] {
        write_file(&project.join("Cargo.toml"), b"[workspace]\n");
        write_file(&project.join("target/blob.bin"), &[0; 2048]);
    }
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let runner = SequencedRunner {
        events: events.clone(),
        exit_code: 7,
    };
    let reporter_events = events.clone();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new("cargo", runner, Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    )
    .with_target_reporter(move |review| {
        reporter_events
            .lock()
            .unwrap()
            .push(format!("target:{}", review.path.display()));
    });
    let first = first.canonicalize().unwrap();
    let second = second.canonicalize().unwrap();
    let reviews = vec![cleanable_review(&first), cleanable_review(&second)];

    let result = daemon
        .execute_reviews_with_safety(
            reviews,
            false,
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
            RunSource::Reviewed,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.errors, 2);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            format!("target:{}", first.display()),
            format!("cargo:{}", first.display()),
            format!("target:{}", second.display()),
            format!("cargo:{}", second.display()),
        ]
    );
}

#[test]
fn exact_review_execution_removes_replaced_target_without_cargo() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);
    let project = project.canonicalize().unwrap();
    let identity = Arc::new(FixedBootSystemIdentityProvider);
    let review = review_project_with_identity_provider(
        &project,
        &[],
        &[],
        &[],
        SystemTime::now(),
        &SafetyOptions {
            target_quiet_period: Duration::ZERO,
            include_managed_cache: false,
            include_active: false,
            force: false,
        },
        identity.as_ref(),
    )
    .unwrap();
    assert_eq!(review.decision, CleanDecision::Cleanable);
    fs::rename(project.join("target"), project.join("target-reviewed")).unwrap();
    write_file(&project.join("target/replacement.bin"), &[0; 2048]);

    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_identity(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            identity,
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    let result = daemon
        .execute_reviews_with_safety(
            vec![review],
            false,
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
            RunSource::Reviewed,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/replacement.bin").exists());
}

fn exact_review_rejects_replaced_target_with_boot_availability(initially_available: bool) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    write_file(&project.join("Cargo.toml"), b"[workspace]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);
    let project = project.canonicalize().unwrap();
    let identity = Arc::new(UnavailableBootIdentityProvider::new(initially_available));
    let review = review_project_with_identity_provider(
        &project,
        &[],
        &[],
        &[],
        SystemTime::now(),
        &SafetyOptions {
            target_quiet_period: Duration::ZERO,
            include_managed_cache: false,
            include_active: false,
            force: false,
        },
        identity.as_ref(),
    )
    .unwrap();
    assert_eq!(review.decision, CleanDecision::Cleanable);

    identity.make_boot_unavailable();
    identity.replace_target();
    let state = tempfile::tempdir().unwrap();
    let store = Store::open(state.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        authoritative_scanner_with_identity(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec![],
            },
            identity,
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        DaemonOptions {
            target_quiet_period: Duration::ZERO,
            ..DaemonOptions::default()
        },
    );

    let result = daemon
        .execute_reviews_with_safety(
            vec![review],
            false,
            SafetyOptions {
                target_quiet_period: Duration::ZERO,
                include_managed_cache: false,
                include_active: false,
                force: false,
            },
            &NoopProcessInspector,
            RunSource::Reviewed,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.skipped, 1);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(project.join("target/blob.bin").exists());
}

#[test]
fn exact_review_rejects_replaced_target_when_both_boot_ids_are_unavailable() {
    exact_review_rejects_replaced_target_with_boot_availability(false);
}

#[test]
fn exact_review_rejects_replaced_target_when_current_boot_id_is_unavailable() {
    exact_review_rejects_replaced_target_with_boot_availability(true);
}
