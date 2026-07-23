use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime};

use car_go_clean::activity::{
    activity_signals_for_process, NoopProcessInspector, ProcessInspector,
};
use car_go_clean::cache::Cache;
use car_go_clean::cleaner::{CleanOutcome, Cleaner, CommandRunner};
use car_go_clean::daemon::{clamp_next_scan_at, Daemon, DaemonOptions, ShutdownFlag};
use car_go_clean::logging::{Logger, LoggerOptions};
use car_go_clean::safety::SafetyOptions;
use car_go_clean::scanner::{
    GitWorktreeError, GitWorktreeResolver, Scanner, ScannerOptions, SystemGitWorktreeResolver,
};
use car_go_clean::store::{ErrorRecord, Store};

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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
            ScannerOptions {
                roots: vec![root.path().to_path_buf()],
                project_dirs: vec![],
                excludes: vec!["excluded/team".to_string()],
            },
            Arc::new(FakeWorktreeResolver::paths(vec![excluded])),
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
fn successful_exclusion_reconciliation_preserves_explicit_project_dir() {
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
        Scanner::with_worktree_resolver(
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
    assert!(store
        .all_projects()
        .unwrap()
        .iter()
        .any(|project| project.path == canonical_explicit.to_string_lossy()));
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "git failed")
        .unwrap();
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_explicit.clone(), canonical_primary]
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
        Scanner::new(ScannerOptions {
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

#[derive(Clone, Default)]
struct FakeRunner {
    calls: Arc<Mutex<Vec<FakeCall>>>,
    delete_target: bool,
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
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/libdep.rlib"), &[0; 2048]);
    write_file(&project.join("target/app"), &[0; 2048]);
    symlink(&project, &alias).unwrap();
    symlink(alias.join("Cargo.toml"), &manifest_link).unwrap();
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
    let argument_sets = [
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
        Scanner::new(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        cleaner,
        DaemonOptions::default(),
    )
    .with_logger(logger);

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
        Scanner::new(ScannerOptions {
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::new(ScannerOptions {
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
        Scanner::new(ScannerOptions {
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
        Scanner::new(ScannerOptions {
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
            scanner_options.clone(),
            Arc::new(FakeWorktreeResolver::failure("git failed")),
        ),
        Cleaner::new("cargo", runner.clone(), Duration::from_secs(60)),
        daemon_options,
    );
    failed_scan.scan_cycle().unwrap();
    let scan_errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(scan_errors.len(), 1);
    assert_eq!(scan_errors[0].category, "scan");
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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

    let runner = FakeRunner {
        delete_target: true,
        ..FakeRunner::default()
    };
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::with_worktree_resolver(
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
        .replace_linked_worktrees(&canonical_primary, &[linked_alias])
        .unwrap();

    let runner = FakeRunner::default();
    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::with_worktree_resolver(
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
    assert_eq!(
        store.blocked_worktree_discovery_paths().unwrap(),
        vec![canonical_linked, canonical_primary]
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
        Scanner::with_worktree_resolver(
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
    assert_eq!(result.skipped, 1);
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
    assert_eq!(forced.cleaned, 1);
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    runner.calls.lock().unwrap().clear();

    symlink(&linked, &linked_alias).unwrap();
    let repaired_state = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
        Scanner::with_worktree_resolver(
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
