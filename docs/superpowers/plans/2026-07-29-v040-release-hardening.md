# v0.4.0 Release Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make platform exclusions a cleanup-boundary invariant, provide a safe active-service upgrade flow, and close every v0.4.0 release-review finding before tagging.

**Architecture:** A new internal storage-profile module is the single source of truth for home-anchored managed/container roots. Cached-state reconciliation checks persisted and physical paths before every review or cleanup, while the scanner rejects excluded paths before filesystem access. Service start/stop and versioned release-note composition complete the operational upgrade path without auto-enabling, restarting, publishing, or merging anything.

**Tech Stack:** Rust 2021, SQLite/rusqlite, Clap, macOS launchd, Linux systemd user services, POSIX shell, GitHub Actions, cargo-dist 0.32.

## Global Constraints

- Work directly on `main`; do not create a feature worktree.
- Use Rust `1.95.0` for formatting, Clippy, and tests.
- Preserve the existing SQLite schema and all recovery history.
- Exclusions control discovery; cleanup classification independently protects managed/container storage.
- Managed/container cleanup requires `--include-managed-cache` even when custom configuration omits a protected root from `excludes`.
- Excluded lexical paths must be rejected before filesystem access and checked again after canonicalization.
- Any reconciliation uncertainty must abort before Cargo.
- New tests must assert executable behavior or parsed structure, not whole-paragraph prose.
- Do not create or push `v0.4.0`, publish a release, merge a tap pull request, upgrade Homebrew, or restart the installed daemon.

---

### Task 1: Centralize platform storage profiles and cleanup classification

**Files:**
- Create: `src/storage.rs`
- Modify: `src/lib.rs`
- Modify: `src/config.rs`
- Modify: `src/safety.rs`
- Modify: `src/cli.rs`
- Modify: `docs/configuration.md`
- Modify: `tests/config.rs`
- Modify: `tests/safety.rs`
- Modify: `tests/cli.rs`

**Interfaces:**
- Produces: `storage::HostPlatform`, `storage::ProtectedKind`, `storage::ProtectedRoot`, `storage::current_home_dir()`, `storage::protected_roots_for(home, platform)`, and `storage::classify_protected_path_for(path, home, platform)`.
- Consumes: Existing `ProjectClass::{ManagedCache, ContainerStorage, Workspace}`.
- Later tasks rely on `config::default_excludes` and `safety::classify_project` sharing the same profile.

- [ ] **Step 1: Add failing profile-wide tests**

Create `src/storage.rs` with only the test module below and add `mod storage;`
to `src/lib.rs` so the RED build includes it. Add unit tests for exact root and
class mapping:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn macos_profile_maps_every_protected_root() {
        let home = Path::new("/Users/tester");
        let roots = protected_roots_for(home, HostPlatform::MacOs);

        assert_eq!(
            roots,
            vec![
                protected(home, ".cargo", ProtectedKind::ManagedCache),
                protected(home, ".rustup", ProtectedKind::ManagedCache),
                protected(home, ".cache", ProtectedKind::ManagedCache),
                protected(home, ".bun/install/cache", ProtectedKind::ManagedCache),
                protected(home, "go/pkg/mod", ProtectedKind::ManagedCache),
                protected(home, ".colima", ProtectedKind::ContainerStorage),
                protected(home, ".lima", ProtectedKind::ContainerStorage),
                protected(
                    home,
                    ".local/share/containers",
                    ProtectedKind::ContainerStorage,
                ),
                protected(home, "Library", ProtectedKind::ManagedCache),
                protected(home, ".Trash", ProtectedKind::ManagedCache),
                protected(home, "OrbStack", ProtectedKind::ContainerStorage),
            ]
        );
    }
}
```

Add the Linux equivalent for `.local/share/docker`, `.docker/desktop`,
`.local/share/rancher-desktop`, and `.local/share/Trash`. Add tests that a
relative or missing home returns no anchored roots and that similarly named
paths outside home remain `None`.

In `tests/safety.rs`, add public-behavior coverage using the process home:

```rust
#[test]
fn default_cleanup_classification_protects_every_current_platform_root() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let mut relatives = vec![
        ".cargo",
        ".rustup",
        ".cache",
        ".bun/install/cache",
        "go/pkg/mod",
        ".colima",
        ".lima",
        ".local/share/containers",
    ];
    if cfg!(target_os = "macos") {
        relatives.extend(["Library", ".Trash", "OrbStack"]);
    } else if cfg!(target_os = "linux") {
        relatives.extend([
            ".local/share/docker",
            ".docker/desktop",
            ".local/share/rancher-desktop",
            ".local/share/Trash",
        ]);
    }

    for relative in relatives {
        let class = classify_project(&home.join(relative).join("copied-crate"));
        assert_ne!(class, ProjectClass::Workspace, "{relative}");
    }
}
```

Add an authorization regression showing that `--force` is not an alias for
`--include-managed-cache`:

```rust
#[test]
fn force_does_not_authorize_managed_storage() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join(".cargo/registry/src/copied-crate");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 4096]);

    let mut opts = options();
    opts.force = true;
    let review = review_project(&project, &[], &[], SystemTime::now(), &opts).unwrap();
    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::ManagedCache)
    );

    opts.include_managed_cache = true;
    let review = review_project(&project, &[], &[], SystemTime::now(), &opts).unwrap();
    assert_eq!(review.decision, CleanDecision::Cleanable);
}
```

- [ ] **Step 2: Run the focused tests and capture RED**

Run:

```sh
mise exec rust@1.95.0 -- cargo test storage --lib -- --nocapture
mise exec rust@1.95.0 -- cargo test --test safety default_cleanup_classification_protects_every_current_platform_root -- --exact --nocapture
mise exec rust@1.95.0 -- cargo test --test safety force_does_not_authorize_managed_storage -- --exact --nocapture
```

Expected: compilation fails because `src/storage.rs` and the profile interfaces
do not exist, the classification test reports currently unprotected roots, or
the force-authorization test reports `Cleanable`.

- [ ] **Step 3: Implement the shared storage profile**

Create `src/storage.rs` with these production types:

```rust
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostPlatform {
    MacOs,
    Linux,
    Other,
}

impl HostPlatform {
    pub(crate) fn current() -> Self {
        match env::consts::OS {
            "macos" => Self::MacOs,
            "linux" => Self::Linux,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtectedKind {
    ManagedCache,
    ContainerStorage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtectedRoot {
    pub(crate) path: PathBuf,
    pub(crate) kind: ProtectedKind,
}

pub(crate) fn current_home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn protected(home: &Path, relative: &str, kind: ProtectedKind) -> ProtectedRoot {
    ProtectedRoot {
        path: home.join(relative),
        kind,
    }
}

pub(crate) fn protected_roots_for(
    home: &Path,
    platform: HostPlatform,
) -> Vec<ProtectedRoot> {
    if !home.is_absolute() {
        return Vec::new();
    }

    let mut roots = vec![
        protected(home, ".cargo", ProtectedKind::ManagedCache),
        protected(home, ".rustup", ProtectedKind::ManagedCache),
        protected(home, ".cache", ProtectedKind::ManagedCache),
        protected(home, ".bun/install/cache", ProtectedKind::ManagedCache),
        protected(home, "go/pkg/mod", ProtectedKind::ManagedCache),
        protected(home, ".colima", ProtectedKind::ContainerStorage),
        protected(home, ".lima", ProtectedKind::ContainerStorage),
        protected(
            home,
            ".local/share/containers",
            ProtectedKind::ContainerStorage,
        ),
    ];
    match platform {
        HostPlatform::MacOs => roots.extend([
            protected(home, "Library", ProtectedKind::ManagedCache),
            protected(home, ".Trash", ProtectedKind::ManagedCache),
            protected(home, "OrbStack", ProtectedKind::ContainerStorage),
        ]),
        HostPlatform::Linux => roots.extend([
            protected(
                home,
                ".local/share/docker",
                ProtectedKind::ContainerStorage,
            ),
            protected(
                home,
                ".docker/desktop",
                ProtectedKind::ContainerStorage,
            ),
            protected(
                home,
                ".local/share/rancher-desktop",
                ProtectedKind::ContainerStorage,
            ),
            protected(home, ".local/share/Trash", ProtectedKind::ManagedCache),
        ]),
        HostPlatform::Other => {}
    }
    roots
}

pub(crate) fn classify_protected_path_for(
    path: &Path,
    home: &Path,
    platform: HostPlatform,
) -> Option<ProtectedKind> {
    fn within(path: &Path, root: &Path) -> bool {
        path == root || path.starts_with(root)
    }

    let physical_path = fs::canonicalize(path).ok();
    protected_roots_for(home, platform)
        .into_iter()
        .find(|root| {
            within(path, &root.path)
                || physical_path.as_deref().is_some_and(|physical| {
                    within(physical, &root.path)
                        || fs::canonicalize(&root.path)
                            .ok()
                            .is_some_and(|physical_root| within(physical, &physical_root))
                })
        })
        .map(|root| root.kind)
}
```

Move `HostPlatform`, home lookup, and platform root construction out of
`src/config.rs`. Keep `.git` and `node_modules` as relative discovery
exclusions, then append every `ProtectedRoot.path`.

In `src/safety.rs`, check the shared profile before the existing exact
component-sequence fallbacks:

```rust
let protected = classify_protected_path_for(
    path,
    &current_home_dir(),
    HostPlatform::current(),
);
match protected {
    Some(ProtectedKind::ManagedCache) => ProjectClass::ManagedCache,
    Some(ProtectedKind::ContainerStorage) => ProjectClass::ContainerStorage,
    None => classify_legacy_component_patterns(path),
}
```

Retain the existing Cargo/Bun/Go/`Library/Caches`/`OrbStack/docker` fallbacks
for compatibility when an absolute home profile is unavailable.

Make managed/container authorization independent of `force` by changing:

```rust
if !opts.force && !opts.include_managed_cache {
```

to:

```rust
if !opts.include_managed_cache {
```

Keep `force` behavior unchanged for scan-error, active-process, recent-write,
and readable direct-target gates.

Update the `run --force` help in `src/cli.rs` to:

```rust
/// Bypass scan-error, activity, and quiet-period gates; managed storage still
/// requires --include-managed-cache.
```

Update `run_help_explains_default_scan_and_safety_flags` in `tests/cli.rs` to
assert both halves of that contract. In `docs/configuration.md`, replace the
claim that force bypasses every policy gate with: “`run --force` bypasses
scan-error, activity, and quiet-period gates; it does not bypass the direct
readable-target requirement or managed-storage authorization.”

- [ ] **Step 4: Run focused and surrounding tests**

Run:

```sh
mise exec rust@1.95.0 -- cargo test storage --lib
mise exec rust@1.95.0 -- cargo test --test config
mise exec rust@1.95.0 -- cargo test --test safety
mise exec rust@1.95.0 -- cargo test --test cli run_help_explains_default_scan_and_safety_flags -- --exact
```

Expected: all profile, configuration, and safety tests pass.

- [ ] **Step 5: Run formatting and strict Clippy**

Run:

```sh
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
```

Expected: both commands exit 0 with no warnings.

- [ ] **Step 6: Commit**

```sh
git add src/storage.rs src/lib.rs src/config.rs src/safety.rs src/cli.rs docs/configuration.md tests/config.rs tests/safety.rs tests/cli.rs
git commit -m "fix: unify protected storage policy"
```

---

### Task 2: Enforce exclusion reconciliation before every review and cleanup

**Files:**
- Modify: `src/store.rs`
- Modify: `src/cache.rs`
- Modify: `src/daemon.rs`
- Modify: `src/cli.rs`
- Modify: `tests/store.rs`
- Modify: `tests/cache_cleaner_daemon.rs`
- Modify: `tests/cli.rs`

**Interfaces:**
- Produces: `Cache::reconcile_for_review<F>(&self, is_excluded: F) -> Result<Vec<PathBuf>>`.
- Produces: `Daemon::reconcile_cached_state(&self) -> Result<Vec<PathBuf>>`.
- Consumes: `Scanner::is_excluded(&Path) -> bool`.
- All real and dry review paths must call reconciliation before loading projects.

- [ ] **Step 1: Change the canonical-alias store test to the safe expectation**

Rename
`reconcile_excluded_discovery_state_matches_canonical_primary_path_only` to
`reconcile_excluded_discovery_state_removes_physically_excluded_project` and
change the project assertion:

```rust
store
    .reconcile_excluded_discovery_state(|path| path.starts_with(&excluded_root))
    .unwrap();

assert!(store.all_projects().unwrap().is_empty());
assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
```

Add a separate unreadable/canonicalization-failure case that expects an error
and leaves the project row intact. Use a symlink loop so the failure is
deterministic without relying on the test user's permissions:

```rust
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
```

- [ ] **Step 2: Add end-to-end cleanup-boundary regressions**

In `tests/cache_cleaner_daemon.rs`, add:

```rust
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
    assert!(store.all_projects().unwrap().is_empty());
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
        Scanner::new(ScannerOptions {
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
    assert_eq!(store.all_projects().unwrap().len(), 1);
    assert_eq!(store.last_run().unwrap().projects_cleaned, 1);
}
```

In `tests/cli.rs`, add:

```rust
#[cfg(unix)]
#[test]
fn run_no_scan_prunes_physically_excluded_cached_alias_before_review() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let work = tempfile::tempdir().unwrap();
    let library = work.path().join("Library");
    let physical = library.join("copied-crate");
    let alias = work.path().join("legacy-crate");
    fs::create_dir_all(physical.join("target")).unwrap();
    fs::write(physical.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(physical.join("target/blob.bin"), vec![0; 4096]).unwrap();
    symlink(&physical, &alias).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = []\nexcludes = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            library.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&alias, SystemTime::now()).unwrap();
    drop(store);

    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work.path().join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--no-scan", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", path)
        .assert()
        .success()
        .stdout(contains("Run complete: cleaned=0"));

    assert!(!marker.exists());
    assert!(physical.join("target/blob.bin").exists());
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert!(store.all_projects().unwrap().is_empty());
}
```

- [ ] **Step 3: Run the new tests and capture RED**

Run:

```sh
mise exec rust@1.95.0 -- cargo test --test store reconcile_excluded_discovery_state_removes_physically_excluded_project -- --exact --nocapture
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon cleanup_boundary_prunes_alias_of_excluded_library_before_cargo -- --exact --nocapture
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon upgraded_nonempty_cache_prunes_exclusions_when_clean_is_due_before_scan -- --exact --nocapture
mise exec rust@1.95.0 -- cargo test --test cli run_no_scan_prunes_physically_excluded_cached_alias_before_review -- --exact --nocapture
```

Expected: the alias row survives, or fake Cargo is invoked before the
production change.

- [ ] **Step 4: Make store reconciliation physical and transactional**

Refactor `Store::reconcile_excluded_discovery_state` to collect decisions
before opening the mutation transaction:

```rust
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
        Err(err) => Err(err).with_context(|| {
            format!("canonicalize cached project {}", path.display())
        }),
    }
}
```

Read all three identity tables from `self.conn` first, using the existing
`collect_rows` helper:

```rust
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
    let remove = should_remove_cached_path(Path::new(&primary), &mut is_excluded)?
        || should_remove_cached_path(Path::new(&linked), &mut is_excluded)?
        || match canonical_primary.as_deref() {
            Some(path) => {
                should_remove_cached_path(Path::new(path), &mut is_excluded)?
            }
            None => false,
        };
    if remove {
        remove_linked.push((primary, linked));
    }
}

let mut remove_failures = Vec::new();
for (primary, canonical_primary) in failures {
    let remove = should_remove_cached_path(Path::new(&primary), &mut is_excluded)?
        || match canonical_primary.as_deref() {
            Some(path) => {
                should_remove_cached_path(Path::new(path), &mut is_excluded)?
            }
            None => false,
        };
    if remove {
        remove_failures.push(primary);
    }
}
```

Only after all three vectors are complete, open one SQLite transaction:

```rust
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
```

Propagate the first non-`NotFound` canonicalization error before opening the
transaction. Do not update or delete errors, runs, clean events, review
status, or scheduler state.

- [ ] **Step 5: Add the cache and daemon reconciliation boundary**

In `src/cache.rs`, add:

```rust
pub fn reconcile_for_review<F>(&self, is_excluded: F) -> Result<Vec<PathBuf>>
where
    F: FnMut(&Path) -> bool,
{
    self.store.reconcile_excluded_discovery_state(is_excluded)?;
    self.sync_on_disk()
}
```

In `src/daemon.rs`, add:

```rust
pub fn reconcile_cached_state(&self) -> Result<Vec<PathBuf>> {
    self.cache
        .reconcile_for_review(|path| self.scanner.is_excluded(path))
}
```

Call `reconcile_cached_state()`:

- within `scan_cycle`, before applying fresh scan discoveries;
- at the start of `run_cycle_with_safety`, replacing direct
  `cache.sync_on_disk()`.

This keeps scheduled cleanup safe even when its deadline precedes the next
scan.

- [ ] **Step 6: Apply reconciliation to CLI review paths**

Add a focused helper in `src/cli.rs`:

```rust
fn reconcile_review_state(store: &Store, cfg: &Config) -> Result<()> {
    daemon_for_scan(store, cfg).reconcile_cached_state()?;
    Ok(())
}
```

Use it before `project_reviews` in:

- `status`: replace the call at the start of the `refresh` branch;
- `projects`: replace the call immediately after `open_store`;
- `run_once`: replace the call inside the `dry_run` branch, including when
  `no_scan` is true.

Do not perform a full scan when `--no-scan` is present.

- [ ] **Step 7: Run focused and surrounding tests**

Run:

```sh
mise exec rust@1.95.0 -- cargo test --test store
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon
mise exec rust@1.95.0 -- cargo test --test cli
mise exec rust@1.95.0 -- cargo test --test safety
```

Expected: all tests pass and every fake-Cargo marker for excluded paths remains
absent.

- [ ] **Step 8: Run formatting and strict Clippy**

```sh
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
```

Expected: both commands exit 0.

- [ ] **Step 9: Commit**

```sh
git add src/store.rs src/cache.rs src/daemon.rs src/cli.rs tests/store.rs tests/cache_cleaner_daemon.rs tests/cli.rs
git commit -m "fix: enforce exclusions at cleanup boundary"
```

---

### Task 3: Reject excluded scanner paths before filesystem access

**Files:**
- Modify: `src/scanner.rs`
- Modify: `tests/scanner.rs`

**Interfaces:**
- Preserves: `Scanner::scan_with_errors() -> Result<ScanReport>`.
- Preserves: `Scanner::is_excluded(&Path) -> bool`.
- Changes only the ordering of exclusion and filesystem operations.

- [ ] **Step 1: Add failing no-touch scanner regressions**

Add:

```rust
#[test]
fn excluded_missing_scan_root_produces_no_error() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("Library");
    let scanner = Scanner::new(ScannerOptions {
        roots: vec![missing.clone()],
        project_dirs: vec![],
        excludes: vec![missing.to_string_lossy().into_owned()],
    });

    let report = scanner.scan_with_errors().unwrap();
    assert!(report.projects.is_empty());
    assert!(report.errors.is_empty());
    assert!(report.worktree_discoveries.is_empty());
}

#[test]
fn excluded_explicit_project_does_not_resolve_worktrees() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("excluded-project");
    write_file(&project.join("Cargo.toml"), "[workspace]\n");
    fs::create_dir_all(project.join(".git")).unwrap();
    let resolver = FakeResolver::paths(vec![]);
    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![],
            project_dirs: vec![project.clone()],
            excludes: vec![project.to_string_lossy().into_owned()],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();
    assert!(report.projects.is_empty());
    assert!(report.errors.is_empty());
    assert!(report.worktree_discoveries.is_empty());
    assert!(resolver.calls().is_empty());
}

#[cfg(unix)]
#[test]
fn alias_to_excluded_root_is_rejected_after_canonicalization() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let excluded = root.path().join("Library/copied-crate");
    let alias = root.path().join("legacy-crate");
    write_file(&excluded.join("Cargo.toml"), "[workspace]\n");
    fs::create_dir_all(excluded.join(".git")).unwrap();
    symlink(&excluded, &alias).unwrap();
    let resolver = FakeResolver::paths(vec![]);
    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![alias],
            project_dirs: vec![],
            excludes: vec![root.path().join("Library").to_string_lossy().into_owned()],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();
    assert!(report.projects.is_empty());
    assert!(report.errors.is_empty());
    assert!(report.worktree_discoveries.is_empty());
    assert!(resolver.calls().is_empty());
}
```

- [ ] **Step 2: Run the focused tests and capture RED**

```sh
mise exec rust@1.95.0 -- cargo test --test scanner excluded_missing_scan_root_produces_no_error -- --exact --nocapture
mise exec rust@1.95.0 -- cargo test --test scanner excluded_explicit_project_does_not_resolve_worktrees -- --exact --nocapture
```

Expected: the missing-root test records a scan error before the ordering
change. The explicit-project and alias tests lock the empty-report and
zero-resolver contract while the implementation ordering below removes their
pre-exclusion manifest/metadata probes.

- [ ] **Step 3: Reorder scanner boundaries**

In `scan_with_errors`:

```rust
let mut canonical_roots: Vec<_> = self
    .opts
    .roots
    .iter()
    .chain(&self.opts.project_dirs)
    .filter(|path| !self.should_skip(path))
    .filter_map(|path| fs::canonicalize(path).ok())
    .filter(|path| !self.should_skip(path))
    .collect();
canonical_roots.sort();
canonical_roots.dedup();

for root in &self.opts.roots {
    if self.should_skip(root) {
        continue;
    }
    let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if self.should_skip(&canonical_root) {
        continue;
    }
    self.walk(
        &canonical_root,
        &[],
        &canonical_roots,
        &mut found,
        &mut worktree_discoveries,
        &mut errors,
    )?;
}
```

For explicit projects, use:

```rust
for project in &self.opts.project_dirs {
    if self.should_skip(project) {
        continue;
    }
    self.add_cargo_project(
        project,
        &canonical_roots,
        true,
        &mut found,
        &mut worktree_discoveries,
        &mut errors,
    );
}
```

At the first line of `walk`, return `Ok(())` when `self.should_skip(dir)`;
only then call `fs::metadata`. In the directory-entry loop, derive
`let path = entry.path()`, continue when `self.should_skip(&path)`, and only
then call `entry.file_type()`.

At the first line of `add_cargo_project`, return when `honor_excludes &&
self.should_skip(project)`. Canonicalize only surviving paths, then repeat the
same exclusion check on the canonical path before checking UTF-8, the
manifest, or `.git`.

In `discover_linked_worktrees`, process every resolver candidate in this exact
order:

```rust
if self.should_skip(&candidate) {
    excluded.insert(candidate);
    continue;
}
let Ok(candidate) = fs::canonicalize(candidate) else {
    continue;
};
if self.should_skip(&candidate) {
    excluded.insert(candidate);
    continue;
}
```

Only after those checks perform UTF-8, primary-identity, scope, manifest, and
finding logic.

- [ ] **Step 4: Run the scanner and daemon suites**

```sh
mise exec rust@1.95.0 -- cargo test --test scanner
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon
```

Expected: all scanner and daemon integration tests pass.

- [ ] **Step 5: Run formatting and strict Clippy**

```sh
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
```

Expected: both commands exit 0.

- [ ] **Step 6: Commit**

```sh
git add src/scanner.rs tests/scanner.rs
git commit -m "fix: prune exclusions before filesystem access"
```

---

### Task 4: Add supported service stop and start actions

**Files:**
- Modify: `src/service.rs`
- Modify: `src/cli.rs`
- Modify: `tests/service.rs`
- Modify: `tests/cli.rs`

**Interfaces:**
- Extends: `ServiceAction` with `Start` and `Stop`.
- Produces: `ServiceManager::start() -> Result<ServiceStatus>`.
- Produces: `ServiceManager::stop() -> Result<ServiceStatus>`.
- Extends CLI: `car-go-clean service start|stop`.

- [ ] **Step 1: Add failing platform lifecycle tests**

In `tests/service.rs`, add:

```rust
#[test]
fn mac_stop_preserves_plist_and_start_bootstraps_it() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "plist").unwrap();

    let mut stop = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    assert_eq!(
        stop.stop().unwrap(),
        ServiceStatus {
            installed: true,
            active: false,
        }
    );
    let stop_runner = stop.into_runner();
    assert_eq!(strings(&stop_runner.calls[0].1)[0], "print");
    assert_eq!(strings(&stop_runner.calls[1].1)[0], "bootout");
    assert!(plist.exists());

    let mut start = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                false,
                String::new(),
                "Could not find specified service".to_string(),
            ),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    assert_eq!(
        start.start().unwrap(),
        ServiceStatus {
            installed: true,
            active: true,
        }
    );
    let start_runner = start.into_runner();
    assert_eq!(strings(&start_runner.calls[0].1)[0], "print");
    assert_eq!(strings(&start_runner.calls[1].1)[0], "bootstrap");
    assert_eq!(strings(&start_runner.calls[2].1)[0], "kickstart");
}

#[test]
fn linux_stop_and_start_use_user_service_commands() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();

    let mut stop = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    stop.stop().unwrap();
    let stop_runner = stop.into_runner();
    assert!(stop_runner.calls.iter().any(|(_, args)| {
        strings(args) == ["--user", "stop", "car-go-clean.service"]
    }));
    assert!(!stop_runner
        .calls
        .iter()
        .any(|(program, _)| program == Path::new("sudo")));

    let mut start = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(false, "inactive\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    start.start().unwrap();
    let start_runner = start.into_runner();
    assert!(start_runner.calls.iter().any(|(_, args)| {
        strings(args) == ["--user", "start", "car-go-clean.service"]
    }));
    assert!(!start_runner
        .calls
        .iter()
        .any(|(program, _)| program == Path::new("sudo")));
}

#[test]
fn start_requires_an_installed_definition() {
    let work = tempfile::tempdir().unwrap();
    let mut manager = test_manager(
        ServicePlatform::MacOs,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );
    let error = manager.start().unwrap_err();
    assert!(error.to_string().contains("not installed"));
    assert!(manager.into_runner().calls.is_empty());
}

#[test]
fn start_and_stop_are_idempotent_for_current_state() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "plist").unwrap();

    let mut active = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([CommandOutput::new(
            true,
            String::new(),
            String::new(),
        )]),
    );
    assert!(active.start().unwrap().active);
    assert_eq!(active.into_runner().calls.len(), 1);

    let mut inactive = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([CommandOutput::new(
            false,
            String::new(),
            "Could not find specified service".to_string(),
        )]),
    );
    assert!(!inactive.stop().unwrap().active);
    assert_eq!(inactive.into_runner().calls.len(), 1);
}

#[test]
fn lifecycle_reports_unexpected_status_probe_failure() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "plist").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([CommandOutput::new(
            false,
            String::new(),
            "Operation not permitted".to_string(),
        )]),
    );

    let error = manager.stop().unwrap_err();
    assert!(error.to_string().contains("Operation not permitted"));
    assert!(plist.exists());
    assert_eq!(manager.into_runner().calls.len(), 1);
}
```

Extend `FakeRunner` with explicit queued outputs while retaining its existing
special-case fields:

```rust
use std::collections::VecDeque;

#[derive(Default)]
struct FakeRunner {
    calls: Vec<(PathBuf, Vec<OsString>)>,
    outputs: VecDeque<CommandOutput>,
    fail_systemd_environment: bool,
    disable_output: Option<CommandOutput>,
    bootout_output: Option<CommandOutput>,
}

impl FakeRunner {
    fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
        Self {
            outputs: outputs.into_iter().collect(),
            ..Self::default()
        }
    }
}
```

At the start of `CommandRunner::run`, after recording the call, return
`self.outputs.pop_front()` when present; otherwise retain the existing
command-specific behavior.

Add `ServiceStatus` to the existing `car_go_clean::service` import in this test
module.

In `tests/cli.rs`, add `.stdout(contains("start"))` and
`.stdout(contains("stop"))` to
`service_help_lists_only_explicit_lifecycle_actions`.

- [ ] **Step 2: Run the focused tests and capture RED**

```sh
mise exec rust@1.95.0 -- cargo test --test service start -- --nocapture
mise exec rust@1.95.0 -- cargo test --test service stop -- --nocapture
mise exec rust@1.95.0 -- cargo test --test service lifecycle_reports_unexpected_status_probe_failure -- --exact --nocapture
mise exec rust@1.95.0 -- cargo test --test cli service_help_lists_only_explicit_lifecycle_actions -- --exact --nocapture
```

Expected: compilation or help assertions fail because the actions do not
exist.

- [ ] **Step 3: Implement service start/stop**

Extend `ServiceAction` and `ServiceCommands`.

Make status safe enough to support idempotency without swallowing command
failures. In `status_macos`, a failed `launchctl print` is inactive only when
`is_missing_launchd_service(&output)` is true; otherwise return the same
formatted command error used by `run_checked`.

In `status_linux`, keep the definition and `systemd --user` checks, then use:

```rust
let args = [
    OsString::from("--user"),
    OsString::from("is-active"),
    OsString::from(UNIT),
];
let output = self.run(Path::new("systemctl"), &args)?;
let inactive = matches!(
    output.stdout.trim(),
    "inactive" | "failed" | "deactivating" | "unknown"
);
if !output.success && !inactive {
    return Err(anyhow!(
        "{} failed{}",
        command_description(Path::new("systemctl"), &args),
        format_command_error(&output)
    ));
}
Ok(ServiceStatus {
    installed: true,
    active: output.success,
})
```

This makes only recognized systemd inactive states idempotent; transport,
permission, and other unexpected errors remain errors.

Implement `ServiceManager::stop`:

```rust
pub fn stop(&mut self) -> Result<ServiceStatus> {
    let status = self.status()?;
    if !status.installed || !status.active {
        return Ok(status);
    }
    match self.platform {
        ServicePlatform::MacOs => self.run_checked(
            Path::new("launchctl"),
            &[
                OsString::from("bootout"),
                OsString::from(self.launchd_domain()),
                self.launchd_plist_path().into_os_string(),
            ],
        )?,
        ServicePlatform::Linux => {
            self.require_systemd_user()?;
            self.run_checked(
                Path::new("systemctl"),
                &[OsString::from("--user"), OsString::from("stop"), OsString::from(UNIT)],
            )?;
        }
    }
    Ok(ServiceStatus { installed: true, active: false })
}
```

Implement `start` symmetrically. It errors when `status.installed` is false,
returns unchanged when active, uses `launchctl bootstrap` plus `kickstart` on
macOS, and `systemctl --user start` on Linux:

```rust
pub fn start(&mut self) -> Result<ServiceStatus> {
    let status = self.status()?;
    if !status.installed {
        bail!("car-go-clean service is not installed");
    }
    if status.active {
        return Ok(status);
    }
    match self.platform {
        ServicePlatform::MacOs => {
            self.run_checked(
                Path::new("launchctl"),
                &[
                    OsString::from("bootstrap"),
                    OsString::from(self.launchd_domain()),
                    self.launchd_plist_path().into_os_string(),
                ],
            )?;
            self.run_checked(
                Path::new("launchctl"),
                &[
                    OsString::from("kickstart"),
                    OsString::from("-k"),
                    OsString::from(self.launchd_service_target()),
                ],
            )?;
        }
        ServicePlatform::Linux => self.run_checked(
            Path::new("systemctl"),
            &[
                OsString::from("--user"),
                OsString::from("start"),
                OsString::from(UNIT),
            ],
        )?,
    }
    Ok(ServiceStatus {
        installed: true,
        active: true,
    })
}
```

Add `Start` and `Stop` to `ServiceCommands` and both `ServiceAction` matches:

```rust
ServiceCommands::Start => ServiceAction::Start,
ServiceCommands::Stop => ServiceAction::Stop,
```

and:

```rust
ServiceAction::Start => manager.start()?,
ServiceAction::Stop => manager.stop()?,
```

Preserve the existing service status rendering.

- [ ] **Step 4: Run service and CLI suites**

```sh
mise exec rust@1.95.0 -- cargo test --test service
mise exec rust@1.95.0 -- cargo test --test cli
```

Expected: all lifecycle and visible CLI tests pass.

- [ ] **Step 5: Run formatting and strict Clippy**

```sh
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
```

Expected: both commands exit 0.

- [ ] **Step 6: Commit**

```sh
git add src/service.rs src/cli.rs tests/service.rs tests/cli.rs
git commit -m "feat: add service start and stop"
```

---

### Task 5: Make validation, release notes, and Homebrew completion executable

**Files:**
- Create: `docs/releases/v0.4.0.md`
- Create: `scripts/compose-release-notes.sh`
- Create: `tests/release-notes.sh`
- Modify: `docs/fresh-install-validation.md`
- Modify: `README.md`
- Modify: `docs/releasing.md`
- Modify: `.github/workflows/release.yml`
- Modify: `.github/workflows/ci.yml`
- Modify: `Makefile`
- Modify: `tests/packaging.rs`

**Interfaces:**
- Produces: `scripts/compose-release-notes.sh TAG GENERATED_BODY OUTPUT`.
- Produces: `make test-release-notes`.
- Release workflow requires `docs/releases/${TAG}.md` and composes it before
  `gh release create`.

- [ ] **Step 1: Write the failing release-note composition test**

Create `tests/release-notes.sh`:

```sh
#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM

printf 'generated install body\n' > "$work/generated.md"
"$repo_root/scripts/compose-release-notes.sh" \
  v0.4.0 "$work/generated.md" "$work/output.md"

first_line=$(sed -n '1p' "$work/output.md")
test "$first_line" = "# car-go-clean v0.4.0"
grep -F 'generated install body' "$work/output.md" >/dev/null

if "$repo_root/scripts/compose-release-notes.sh" \
  v0.4 "$work/generated.md" "$work/invalid.md"; then
  echo "invalid tag unexpectedly accepted" >&2
  exit 1
fi
```

Update `Makefile` exactly as follows:

```make
.PHONY: build test test-installer test-release-notes fmt clippy clean

test: test-installer test-release-notes
	$(CARGO) test

test-release-notes:
	sh tests/release-notes.sh
```

- [ ] **Step 2: Run the shell test and capture RED**

```sh
make test-release-notes
```

Expected: failure because the script and versioned notes do not exist.

- [ ] **Step 3: Add versioned notes and the composition script**

Create `docs/releases/v0.4.0.md` with this structure and concrete content:

````markdown
# car-go-clean v0.4.0

## What changed

- Manual `run` scans before review by default; use `--no-scan` for cached-only behavior.
- macOS and Linux defaults exclude protected manager/container storage.
- Cached excluded state is reconciled before every review or cleanup.
- `service start` and `service stop` support safe active-daemon previews.
- Existing `service restart` behavior remains available after configuration or binary changes.

## Upgrading with Homebrew

```sh
brew update
if brew list --versions car-go-clean >/dev/null 2>&1
then
  brew upgrade dcchuck/tap/car-go-clean
else
  brew install dcchuck/tap/car-go-clean
fi
car-go-clean version
```

Confirm the version command prints `0.4.0`.

If the service is installed or running, stop it without removing its
definition, preview with the upgraded binary, and resume it:

```sh
car-go-clean service stop
car-go-clean run --dry-run --all
car-go-clean service start
car-go-clean service status
```

## Custom configuration

An explicit `excludes` array replaces the discovery defaults. Cleanup
classification remains independent: protected manager and container paths
remain skipped unless `--include-managed-cache` is supplied.
````

Create `scripts/compose-release-notes.sh`:

```sh
#!/bin/sh
set -eu

test "$#" -eq 3
tag=$1
generated=$2
output=$3

case "$tag" in
  v*) ;;
  *) echo "tag must be vX.Y.Z" >&2; exit 2 ;;
esac

version=${tag#v}
if ! printf '%s\n' "$version" |
  awk '/^[0-9]+\.[0-9]+\.[0-9]+$/ { valid=1 } END { exit !valid }'
then
  echo "tag must be vX.Y.Z" >&2
  exit 2
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
versioned="$repo_root/docs/releases/$tag.md"
test -r "$versioned"
test -r "$generated"

{
  cat "$versioned"
  printf '\n\n---\n\n'
  cat "$generated"
} > "$output"
```

Extend `tests/release-notes.sh` with:

```sh
for invalid_tag in v0.4 v1..2 v1.2.3x ' v1.2.3'
do
  if "$repo_root/scripts/compose-release-notes.sh" \
    "$invalid_tag" "$work/generated.md" "$work/invalid.md"
  then
    echo "invalid tag unexpectedly accepted: $invalid_tag" >&2
    exit 1
  fi
done
```

Mark both shell scripts executable through Git.

- [ ] **Step 4: Make fresh-install validation deterministic**

Replace the invalid zero duration with:

```sh
printf 'scan_dirs = ["%s"]\ntarget_quiet_period = "1s"\n' \
  "$validation_root" > "$validation_config"
sleep 2
```

Keep the isolated state directory and no-prior-scan assertions.

- [ ] **Step 5: Document active-service and post-release flows**

In `README.md`, add an active-service branch immediately after the Quick Start
preview:

```sh
car-go-clean service stop
car-go-clean run --dry-run --all
car-go-clean service start
```

Use this exact accompanying sentence: “`service stop` preserves the installed
service definition; `service start` resumes it after you approve the preview.”

In `docs/releasing.md`, add the required post-publication sequence:

```sh
formula_pr=$(
  gh pr list \
    --repo dcchuck/homebrew-tap \
    --state open \
    --search 'car-go-clean v0.4.0 in:title' \
    --json number \
    --jq '.[0].number'
)
test -n "$formula_pr"
gh pr checks "$formula_pr" --repo dcchuck/homebrew-tap
gh pr view "$formula_pr" --repo dcchuck/homebrew-tap --web
gh pr merge "$formula_pr" --repo dcchuck/homebrew-tap --merge --delete-branch

brew update
if brew list --versions car-go-clean >/dev/null 2>&1
then
  brew upgrade dcchuck/tap/car-go-clean
else
  brew install dcchuck/tap/car-go-clean
fi
car-go-clean version
car-go-clean service stop
car-go-clean run --dry-run --all
car-go-clean service start
car-go-clean service status
```

Immediately before the snippet, require the releaser to list older open
formula PRs with:

```sh
gh pr list \
  --repo dcchuck/homebrew-tap \
  --state open \
  --search 'car-go-clean in:title' \
  --json number,title,url
```

Require each older formula PR to be inspected and either deliberately closed
as superseded or retained with a written reason. The v0.4.0 PR must be merged
only after its checks pass; the `--web` step is the human formula diff review.

- [ ] **Step 6: Compose reviewed notes in the workflow**

In `.github/workflows/release.yml`, replace the direct announcement-body write
with:

```yaml
- name: Compose reviewed release notes
  env:
    ANNOUNCEMENT_BODY: "${{ fromJson(steps.host.outputs.manifest).announcement_github_body }}"
  run: |
    printf '%s\n' "$ANNOUNCEMENT_BODY" > "$RUNNER_TEMP/generated-notes.md"
    scripts/compose-release-notes.sh \
      "${{ needs.plan.outputs.tag }}" \
      "$RUNNER_TEMP/generated-notes.md" \
      "$RUNNER_TEMP/notes.txt"
```

Keep `gh release create --draft` pointed at `$RUNNER_TEMP/notes.txt`.

In `.github/workflows/ci.yml`, add `make test-release-notes` after the installer
test.

Update `tests/packaging.rs` only for structural contracts:

- assert the release workflow contains
  `scripts/compose-release-notes.sh`;
- assert the CI workflow contains `make test-release-notes`;
- assert `Path::new("docs/releases/v0.4.0.md").is_file()` and
  `Path::new("scripts/compose-release-notes.sh").is_file()`;
- assert the release workflow does not contain
  `echo "$ANNOUNCEMENT_BODY" > $RUNNER_TEMP/notes.txt`.

Do not add whole-paragraph README/runbook string assertions.

- [ ] **Step 7: Run shell and packaging tests**

```sh
make test-release-notes
make test-installer
mise exec rust@1.95.0 -- cargo test --test packaging
```

Expected: all shell and packaging tests pass.

- [ ] **Step 8: Run formatting and strict Clippy**

```sh
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
```

Expected: both commands exit 0.

- [ ] **Step 9: Commit**

```sh
git add docs/releases/v0.4.0.md scripts/compose-release-notes.sh tests/release-notes.sh docs/fresh-install-validation.md README.md docs/releasing.md .github/workflows/release.yml .github/workflows/ci.yml Makefile tests/packaging.rs
git commit -m "docs: complete v0.4 upgrade and release flow"
```

---

### Task 6: Run the final v0.4.0 release gate

**Files:**
- Make no additional file edits. After explicit push authorization, update
  only `origin/main`; do not modify tags, releases, tap state, the local
  installation, or daemon state.

**Interfaces:**
- Consumes all prior task commits.
- Produces a release-gate report with an explicit go/no-go verdict.

- [ ] **Step 1: Run repository checks**

```sh
git diff --check
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
mise exec rust@1.95.0 -- cargo test --locked
make test-installer
make test-release-notes
```

Expected: zero formatting/lint failures, all Rust tests pass, and both shell
suites pass.

- [ ] **Step 2: Verify the release plan and visible CLI**

```sh
dist plan --tag v0.4.0 --output-format=json
mise exec rust@1.95.0 -- cargo run --locked -- run --help
mise exec rust@1.95.0 -- cargo run --locked -- service --help
mise exec rust@1.95.0 -- cargo run --locked -- version
```

Expected:

- four archive targets;
- Homebrew formula artifact;
- the known built-in Homebrew warning only;
- `run` shows auto-scan/`--no-scan`;
- service help shows install/status/start/stop/restart/uninstall;
- version prints `0.4.0`.

- [ ] **Step 3: Verify release boundaries**

```sh
git status --short --branch
git tag --list v0.4.0
git ls-remote --tags origin refs/tags/v0.4.0
```

Expected: clean `main`, no local or remote v0.4.0 tag.

- [ ] **Step 4: Request independent release review**

Dispatch one read-only reviewer over `v0.2.0..HEAD` with the approved design,
this plan, and the seven original findings. Require explicit verdicts for:

- physical alias exclusion;
- clean-before-scan upgrade scheduling;
- pre-filesystem scanner pruning;
- profile-wide defense-in-depth classification;
- executable fresh-install validation;
- formula PR merge/Homebrew verification runbook;
- composed v0.4.0 upgrade notes;
- service stop/start behavior.

The reviewer must report Critical, Important, and Minor findings and a
`Ready to release?` verdict.

- [ ] **Step 5: Resolve review findings**

Any Critical or Important finding blocks release. Use one bounded TDD fix round
and one scoped re-review. If a scoped re-review finds a new load-bearing issue,
stop and request user direction instead of creating an unbounded loop.

- [ ] **Step 6: Push the reviewed implementation and require remote CI**

After the user explicitly authorizes pushing the completed implementation:

```sh
head=$(git rev-parse HEAD)
git push origin main

run_id=
for attempt in 1 2 3 4 5 6
do
  run_id=$(
    gh run list \
      --repo dcchuck/car-go-clean \
      --workflow CI \
      --branch main \
      --commit "$head" \
      --limit 1 \
      --json databaseId \
      --jq '.[0].databaseId'
  )
  test -n "$run_id" && break
  sleep 5
done
test -n "$run_id"
gh run watch "$run_id" --repo dcchuck/car-go-clean --exit-status
```

Expected: `origin/main` points to the reviewed HEAD and that exact commit's CI
run exits successfully. Do not push a tag.

- [ ] **Step 7: Verify the existing tap state read-only**

```sh
gh pr view 2 \
  --repo dcchuck/homebrew-tap \
  --json state,mergedAt,url
gh api repos/dcchuck/homebrew-tap/contents/Formula/car-go-clean.rb \
  --jq .content |
  base64 --decode |
  rg 'v0\.3\.0'
```

Expected: tap PR 2 is merged and the default-branch formula references
`v0.3.0`. These commands must not merge, close, or edit a tap pull request.

- [ ] **Step 8: Report the gate**

Report:

- exact HEAD SHA;
- local and remote CI/test evidence;
- review verdict;
- whether the v0.3.0 tap PR is merged;
- whether the tap currently serves v0.3.0;
- confirmation that v0.4.0 remains untagged/unreleased;
- these exact next release commands, but do not run them:

```sh
git tag -a v0.4.0 -m "car-go-clean v0.4.0"
git push origin v0.4.0
```
