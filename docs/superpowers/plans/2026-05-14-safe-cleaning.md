# Safe Cleaning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `car-go-clean` safe to run against `~` by cleaning only direct, stale, explainably safe `target/` directories, while giving users clear status, project review, and log surfaces.

**Architecture:** Add a safety review layer between the cached project list and `cargo clean`. The layer classifies project paths, measures direct target directories, detects active process signals with `sysinfo`, applies explicit override flags, and returns structured review records that are reused by `run`, `daemon`, `status`, and the new `projects` command.

**Tech Stack:** Rust 2021, Rust 1.95.0, `clap`, `serde`, `serde_json`, `humantime-serde`, `rusqlite`, `sysinfo 0.39.1`, stdlib filesystem/process path APIs, and integration tests using `tempfile`, `assert_cmd`, and fake runners/inspectors.

---

## Design Decisions

- Default scan root remains `$HOME`; `Config::default().scan_dirs` already does this.
- Default cleanability requires `project/target` to exist directly under the cached project path.
- Default target quiet period is 2 hours. If any non-symlink file under `target/` was modified less than 2 hours ago, the project is skipped as `active_recent_write`.
- Managed caches and container storage are skipped by default and require `--include-managed-cache`.
- Active process signals are skipped by default and require `--include-active`.
- `--force` bypasses `active_recent_write`, `active_process`, `managed_cache`, `container_storage`, and `scan_error`, but still requires a direct `target/` directory. Force never makes a missing target cleanable.
- Scan errors block only related projects: a scan error applies when the error path is equal to the project path, inside the project path, or inside `project/target`. Scan errors elsewhere in `$HOME` stay visible in logs but do not block unrelated projects.
- The same review engine powers all user-facing surfaces:
  - `status`: aggregate summary.
  - `projects`: detailed/actionable project list.
  - `logs`: raw diagnostics from the database/log file.
  - `run --dry-run`: exact cleaning plan without invoking `cargo clean`.
- Process detection is cross-platform baseline first. `sysinfo` provides `Process::cmd()` and `Process::cwd()` on supported platforms, and unsupported/missing data produces no active signal rather than failing the run.

## File Structure

- Modify `Cargo.toml`: add `sysinfo = "0.39.1"`.
- Modify `src/lib.rs`: export new modules.
- Modify `src/config.rs`: add `target_quiet_period` with a 2 hour default and validation.
- Create `src/activity.rs`: process activity types, `ProcessInspector` trait, `SysinfoProcessInspector`, and test fake support.
- Create `src/safety.rs`: project classification, target measurement, scan-error attachment, cleanability decisions, summary aggregation, and formatting helpers.
- Modify `src/daemon.rs`: add safe run options and route `run_cycle` through the safety review layer.
- Modify `src/cli.rs`: add `run --dry-run`, `run --include-managed-cache`, `run --include-active`, `run --force`, and `projects` subcommand; upgrade `status` output.
- Modify `src/store.rs`: add helpers for scan errors by path/category if needed by the review layer.
- Modify `tests/config.rs`: config coverage for target quiet period.
- Create `tests/safety.rs`: focused safety policy tests.
- Modify `tests/cache_cleaner_daemon.rs`: daemon skips unsafe projects and cleans safe projects.
- Modify `tests/cli.rs`: CLI behavior for dry-run, projects output, status aggregate, and override flags.
- Modify `README.md`: document safe defaults, commands, overrides, and fresh install validation.

## Task 1: Add Configurable Target Quiet Period

**Files:**
- Modify: `src/config.rs`
- Modify: `tests/config.rs`

- [ ] **Step 1: Write failing config tests**

Add these assertions to `tests/config.rs`.

```rust
#[test]
fn default_target_quiet_period_is_two_hours() {
    let cfg = Config::default();

    assert_eq!(cfg.target_quiet_period, Duration::from_secs(2 * 60 * 60));
}

#[test]
fn load_file_overlays_target_quiet_period() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
target_quiet_period = "30m"
"#,
    )
    .unwrap();

    let cfg = load(&path).unwrap();

    assert_eq!(cfg.target_quiet_period, Duration::from_secs(30 * 60));
}
```

Also add this validation case inside `validate_rejects_bad_intervals_and_log_levels`.

```rust
let cfg = Config {
    target_quiet_period: Duration::ZERO,
    ..Default::default()
};
assert!(cfg.validate().is_err());
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test config target_quiet_period
```

Expected: FAIL because `Config` has no `target_quiet_period` field.

- [ ] **Step 3: Implement config field**

In `src/config.rs`, add the field to `Config`.

```rust
#[serde(default = "default_target_quiet_period", with = "humantime_serde")]
pub target_quiet_period: Duration,
```

Add it to `Default::default()`.

```rust
target_quiet_period: default_target_quiet_period(),
```

Add validation in `Config::validate`.

```rust
if self.target_quiet_period.is_zero() {
    return Err(anyhow!("target_quiet_period must be positive"));
}
```

Add the default helper.

```rust
fn default_target_quiet_period() -> Duration {
    Duration::from_secs(2 * 60 * 60)
}
```

- [ ] **Step 4: Run tests to verify pass**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test config
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs tests/config.rs
git commit -m "Add target quiet period config"
```

## Task 2: Add Safety Policy Types and Target Measurement

**Files:**
- Modify: `src/lib.rs`
- Create: `src/safety.rs`
- Create: `tests/safety.rs`

- [ ] **Step 1: Write failing safety tests**

Create `tests/safety.rs` with these tests and helpers.

```rust
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use car_go_clean::activity::ActivitySignal;
use car_go_clean::safety::{
    classify_project, review_project, CleanDecision, ProjectClass, SafetyOptions, SkipReason,
};

fn write_file(path: &Path, body: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn options() -> SafetyOptions {
    SafetyOptions {
        target_quiet_period: Duration::from_secs(2 * 60 * 60),
        include_managed_cache: false,
        include_active: false,
        force: false,
    }
}

#[test]
fn classifies_known_managed_cache_paths() {
    assert_eq!(
        classify_project(Path::new("/Users/me/.bun/install/cache/pkg/node_modules/crate")),
        ProjectClass::ManagedCache
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/go/pkg/mod/example.com/crate")),
        ProjectClass::ManagedCache
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/.cargo/registry/src/index/crate")),
        ProjectClass::ManagedCache
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/OrbStack/docker/containers/crate")),
        ProjectClass::ContainerStorage
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/src/workspace/crate")),
        ProjectClass::Workspace
    );
}

#[test]
fn missing_direct_target_is_skipped_even_with_force() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");

    let mut opts = options();
    opts.force = true;
    let review = review_project(
        project.path(),
        &[],
        &[],
        SystemTime::now(),
        &opts,
    )
    .unwrap();

    assert_eq!(review.decision, CleanDecision::Skipped(SkipReason::NoTarget));
}

#[test]
fn old_direct_target_is_cleanable() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);

    let now = SystemTime::now() + Duration::from_secs(3 * 60 * 60);
    let review = review_project(project.path(), &[], &[], now, &options()).unwrap();

    assert_eq!(review.decision, CleanDecision::Cleanable);
    assert!(review.target_bytes >= 4096);
}

#[test]
fn recent_target_write_is_skipped() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);

    let review = review_project(project.path(), &[], &[], SystemTime::now(), &options()).unwrap();

    assert!(matches!(
        review.decision,
        CleanDecision::Skipped(SkipReason::ActiveRecentWrite { .. })
    ));
}

#[test]
fn related_scan_error_is_skipped_but_unrelated_error_is_not() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);
    let now = SystemTime::now() + Duration::from_secs(3 * 60 * 60);

    let unrelated = vec![PathBuf::from("/Users/me/Pictures/Photos Library.photoslibrary")];
    let review = review_project(project.path(), &unrelated, &[], now, &options()).unwrap();
    assert_eq!(review.decision, CleanDecision::Cleanable);

    let related = vec![project.path().join("target/debug")];
    let review = review_project(project.path(), &related, &[], now, &options()).unwrap();
    assert_eq!(review.decision, CleanDecision::Skipped(SkipReason::ScanError));
}

#[test]
fn active_process_is_skipped_unless_included() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);
    let now = SystemTime::now() + Duration::from_secs(3 * 60 * 60);
    let signal = ActivitySignal {
        pid: 123,
        project_path: project.path().to_path_buf(),
        reason: "cwd inside project".to_string(),
    };

    let review = review_project(project.path(), &[], &[signal.clone()], now, &options()).unwrap();
    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::ActiveProcess)
    );

    let mut opts = options();
    opts.include_active = true;
    let review = review_project(project.path(), &[], &[signal], now, &opts).unwrap();
    assert_eq!(review.decision, CleanDecision::Cleanable);
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test safety
```

Expected: FAIL because `activity` and `safety` modules do not exist.

- [ ] **Step 3: Export modules**

In `src/lib.rs`, add:

```rust
pub mod activity;
pub mod safety;
```

- [ ] **Step 4: Create initial `src/activity.rs` types**

Create `src/activity.rs`.

```rust
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySignal {
    pub pid: u32,
    pub project_path: PathBuf,
    pub reason: String,
}

pub trait ProcessInspector {
    fn active_projects(&self, projects: &[PathBuf]) -> Result<Vec<ActivitySignal>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopProcessInspector;

impl ProcessInspector for NoopProcessInspector {
    fn active_projects(&self, _projects: &[PathBuf]) -> Result<Vec<ActivitySignal>> {
        Ok(Vec::new())
    }
}

pub fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}
```

- [ ] **Step 5: Create `src/safety.rs` policy**

Create `src/safety.rs`.

```rust
use crate::activity::{path_is_within, ActivitySignal};
use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectClass {
    Workspace,
    ManagedCache,
    ContainerStorage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanDecision {
    Cleanable,
    Skipped(SkipReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    NoTarget,
    ActiveRecentWrite { newest_age_secs: u64 },
    ActiveProcess,
    ManagedCache,
    ContainerStorage,
    ScanError,
    TargetReadError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyOptions {
    pub target_quiet_period: Duration,
    pub include_managed_cache: bool,
    pub include_active: bool,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectReview {
    pub path: PathBuf,
    pub class: ProjectClass,
    pub target_path: PathBuf,
    pub target_bytes: i64,
    pub decision: CleanDecision,
}

pub fn classify_project(path: &Path) -> ProjectClass {
    let parts = path_components(path);
    if contains_sequence(&parts, &["OrbStack", "docker"]) {
        return ProjectClass::ContainerStorage;
    }
    if contains_sequence(&parts, &[".bun", "install", "cache"])
        || contains_sequence(&parts, &["go", "pkg", "mod"])
        || contains_sequence(&parts, &[".cargo", "registry", "src"])
        || contains_sequence(&parts, &[".cargo", "git", "checkouts"])
    {
        return ProjectClass::ManagedCache;
    }
    ProjectClass::Workspace
}

pub fn review_project(
    project: &Path,
    scan_error_paths: &[PathBuf],
    activity: &[ActivitySignal],
    now: SystemTime,
    opts: &SafetyOptions,
) -> Result<ProjectReview> {
    let target_path = project.join("target");
    let class = classify_project(project);
    if !target_path.is_dir() {
        return Ok(ProjectReview {
            path: project.to_path_buf(),
            class,
            target_path,
            target_bytes: 0,
            decision: CleanDecision::Skipped(SkipReason::NoTarget),
        });
    }

    let target_bytes = match dir_size(&target_path) {
        Ok(bytes) => bytes,
        Err(_) if opts.force => 0,
        Err(_) => {
            return Ok(ProjectReview {
                path: project.to_path_buf(),
                class,
                target_path,
                target_bytes: 0,
                decision: CleanDecision::Skipped(SkipReason::TargetReadError),
            });
        }
    };
    let mut decision = CleanDecision::Cleanable;
    if !opts.force {
        decision = first_skip_reason(project, &target_path, class, scan_error_paths, activity, now, opts)?;
    }

    Ok(ProjectReview {
        path: project.to_path_buf(),
        class,
        target_path,
        target_bytes,
        decision,
    })
}

fn first_skip_reason(
    project: &Path,
    target_path: &Path,
    class: ProjectClass,
    scan_error_paths: &[PathBuf],
    activity: &[ActivitySignal],
    now: SystemTime,
    opts: &SafetyOptions,
) -> Result<CleanDecision> {
    match class {
        ProjectClass::ManagedCache if !opts.include_managed_cache => {
            return Ok(CleanDecision::Skipped(SkipReason::ManagedCache));
        }
        ProjectClass::ContainerStorage if !opts.include_managed_cache => {
            return Ok(CleanDecision::Skipped(SkipReason::ContainerStorage));
        }
        _ => {}
    }

    if scan_error_paths
        .iter()
        .any(|error_path| path_is_within(error_path, project) || path_is_within(error_path, target_path))
    {
        return Ok(CleanDecision::Skipped(SkipReason::ScanError));
    }

    if !opts.include_active
        && activity
            .iter()
            .any(|signal| signal.project_path == project || path_is_within(&signal.project_path, project))
    {
        return Ok(CleanDecision::Skipped(SkipReason::ActiveProcess));
    }

    match newest_mtime(target_path) {
        Ok(Some(mtime)) => {
            let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
            if age < opts.target_quiet_period {
                return Ok(CleanDecision::Skipped(SkipReason::ActiveRecentWrite {
                    newest_age_secs: age.as_secs(),
                }));
            }
        }
        Ok(None) => {}
        Err(_) => return Ok(CleanDecision::Skipped(SkipReason::TargetReadError)),
    }

    Ok(CleanDecision::Cleanable)
}

fn newest_mtime(root: &Path) -> Result<Option<SystemTime>> {
    let mut newest = None;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if let Some(child) = newest_mtime(&path)? {
                newest = Some(newest.map_or(child, |current| current.max(child)));
            }
        } else if meta.is_file() {
            let modified = meta.modified()?;
            newest = Some(newest.map_or(modified, |current| current.max(modified)));
        }
    }
    Ok(newest)
}

fn dir_size(root: &Path) -> Result<i64> {
    let mut total = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            total += dir_size(&path)?;
        } else if meta.is_file() {
            total += meta.len() as i64;
        }
    }
    Ok(total)
}

fn path_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn contains_sequence(parts: &[String], needle: &[&str]) -> bool {
    parts
        .windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}
```

- [ ] **Step 6: Run safety tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test safety
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs src/activity.rs src/safety.rs tests/safety.rs
git commit -m "Add clean safety review policy"
```

## Task 3: Add Cross-Platform Process Activity Detection

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/activity.rs`
- Modify: `tests/safety.rs`

- [ ] **Step 1: Add dependency**

Modify `Cargo.toml`.

```toml
sysinfo = "0.39.1"
```

- [ ] **Step 2: Add focused path matching tests**

Append these tests to `tests/safety.rs`.

```rust
use car_go_clean::activity::{path_is_within, process_matches_project};

#[test]
fn path_matching_treats_project_and_target_descendants_as_active() {
    let project = Path::new("/Users/me/src/app");
    assert!(path_is_within(Path::new("/Users/me/src/app"), project));
    assert!(path_is_within(Path::new("/Users/me/src/app/target/debug/app"), project));
    assert!(!path_is_within(Path::new("/Users/me/src/application"), project));
}

#[test]
fn process_command_arguments_can_match_project_or_target_paths() {
    let project = Path::new("/Users/me/src/app");
    let args = vec![
        PathBuf::from("/Users/me/.cargo/bin/cargo"),
        PathBuf::from("/Users/me/src/app"),
    ];

    assert!(process_matches_project(Some(Path::new("/tmp")), &args, project));

    let args = vec![PathBuf::from("/Users/me/src/app/target/debug/server")];
    assert!(process_matches_project(None, &args, project));

    let args = vec![PathBuf::from("/Users/me/src/application")];
    assert!(!process_matches_project(None, &args, project));
}
```

- [ ] **Step 3: Implement matching helpers and sysinfo inspector**

Extend `src/activity.rs`.

```rust
use sysinfo::System;

#[derive(Debug, Clone, Copy, Default)]
pub struct SysinfoProcessInspector;

impl ProcessInspector for SysinfoProcessInspector {
    fn active_projects(&self, projects: &[PathBuf]) -> Result<Vec<ActivitySignal>> {
        let system = System::new_all();
        let mut signals = Vec::new();
        for (pid, process) in system.processes() {
            let cwd = process.cwd();
            let args: Vec<PathBuf> = process
                .cmd()
                .iter()
                .map(|arg| PathBuf::from(arg.as_os_str()))
                .collect();
            for project in projects {
                if process_matches_project(cwd, &args, project) {
                    signals.push(ActivitySignal {
                        pid: pid.as_u32(),
                        project_path: project.clone(),
                        reason: "cwd or command references project".to_string(),
                    });
                    break;
                }
            }
        }
        Ok(signals)
    }
}

pub fn process_matches_project(
    cwd: Option<&Path>,
    args: &[PathBuf],
    project: &Path,
) -> bool {
    if cwd.is_some_and(|cwd| path_is_within(cwd, project)) {
        return true;
    }
    let target = project.join("target");
    args.iter()
        .any(|arg| path_is_within(arg, project) || path_is_within(arg, &target))
}
```

- [ ] **Step 4: Run activity tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test safety path_matching_treats_project_and_target_descendants_as_active
mise exec rust@1.95.0 -- cargo test --test safety process_command_arguments_can_match_project_or_target_paths
```

Expected: PASS.

- [ ] **Step 5: Run full compile check**

Run:

```bash
mise exec rust@1.95.0 -- cargo check
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/activity.rs tests/safety.rs
git commit -m "Detect active project processes"
```

## Task 4: Add Review Aggregation and Store Scan Error Helper

**Files:**
- Modify: `src/safety.rs`
- Modify: `src/store.rs`
- Modify: `tests/store.rs`
- Modify: `tests/safety.rs`

- [ ] **Step 1: Write failing store helper test**

Append to `tests/store.rs`.

```rust
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
        store.scan_error_paths_since(std::time::SystemTime::UNIX_EPOCH).unwrap(),
        vec![std::path::PathBuf::from("/tmp/blocked")]
    );
}
```

- [ ] **Step 2: Write failing aggregate summary test**

Append to `tests/safety.rs`.

```rust
use car_go_clean::safety::{review_summary, ReviewSummary};

#[test]
fn review_summary_counts_cleanable_and_skip_reasons() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);
    let old = SystemTime::now() + Duration::from_secs(3 * 60 * 60);
    let cleanable = review_project(project.path(), &[], &[], old, &options()).unwrap();

    let missing = tempfile::tempdir().unwrap();
    let skipped = review_project(missing.path(), &[], &[], old, &options()).unwrap();

    let summary = review_summary(&[cleanable, skipped]);

    assert_eq!(
        summary,
        ReviewSummary {
            total_projects: 2,
            cleanable_projects: 1,
            skipped_projects: 1,
            cleanable_bytes: 4096,
            active_recent_write: 0,
            active_process: 0,
            managed_cache: 0,
            container_storage: 0,
            scan_error: 0,
            no_target: 1,
            target_read_error: 0,
        }
    );
}
```

- [ ] **Step 3: Run tests to verify failure**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test store scan_error_paths_since_returns_only_scan_paths
mise exec rust@1.95.0 -- cargo test --test safety review_summary_counts_cleanable_and_skip_reasons
```

Expected: FAIL because helper and summary types do not exist.

- [ ] **Step 4: Implement store helper**

In `src/store.rs`, add:

```rust
pub fn scan_error_paths_since(&self, since: SystemTime) -> Result<Vec<PathBuf>> {
    let mut stmt = self.conn.prepare(
        "
        SELECT path FROM errors
        WHERE ts >= ?1 AND category = 'scan' AND path IS NOT NULL
        ORDER BY path
        ",
    )?;
    let rows = stmt.query_map([to_epoch(since)?], |row| {
        let path: String = row.get(0)?;
        Ok(PathBuf::from(path))
    })?;
    collect_rows(rows)
}
```

- [ ] **Step 5: Implement summary types**

In `src/safety.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewSummary {
    pub total_projects: usize,
    pub cleanable_projects: usize,
    pub skipped_projects: usize,
    pub cleanable_bytes: i64,
    pub active_recent_write: usize,
    pub active_process: usize,
    pub managed_cache: usize,
    pub container_storage: usize,
    pub scan_error: usize,
    pub no_target: usize,
    pub target_read_error: usize,
}

pub fn review_summary(reviews: &[ProjectReview]) -> ReviewSummary {
    let mut summary = ReviewSummary {
        total_projects: reviews.len(),
        cleanable_projects: 0,
        skipped_projects: 0,
        cleanable_bytes: 0,
        active_recent_write: 0,
        active_process: 0,
        managed_cache: 0,
        container_storage: 0,
        scan_error: 0,
        no_target: 0,
        target_read_error: 0,
    };

    for review in reviews {
        match &review.decision {
            CleanDecision::Cleanable => {
                summary.cleanable_projects += 1;
                summary.cleanable_bytes += review.target_bytes;
            }
            CleanDecision::Skipped(reason) => {
                summary.skipped_projects += 1;
                match reason {
                    SkipReason::NoTarget => summary.no_target += 1,
                    SkipReason::ActiveRecentWrite { .. } => summary.active_recent_write += 1,
                    SkipReason::ActiveProcess => summary.active_process += 1,
                    SkipReason::ManagedCache => summary.managed_cache += 1,
                    SkipReason::ContainerStorage => summary.container_storage += 1,
                    SkipReason::ScanError => summary.scan_error += 1,
                    SkipReason::TargetReadError => summary.target_read_error += 1,
                }
            }
        }
    }

    summary
}
```

- [ ] **Step 6: Run tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test store
mise exec rust@1.95.0 -- cargo test --test safety
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/store.rs src/safety.rs tests/store.rs tests/safety.rs
git commit -m "Summarize safe clean reviews"
```

## Task 5: Integrate Safety Review into Daemon Run Cycle

**Files:**
- Modify: `src/daemon.rs`
- Modify: `tests/cache_cleaner_daemon.rs`

- [ ] **Step 1: Write failing daemon safety test**

Append to `tests/cache_cleaner_daemon.rs`.

```rust
use car_go_clean::activity::NoopProcessInspector;
use car_go_clean::safety::SafetyOptions;

#[test]
fn daemon_run_cycle_skips_recent_targets_by_default() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/blob.bin"), &[0; 2048]);

    let db_dir = tempfile::tempdir().unwrap();
    let store = Store::open(db_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&project, std::time::SystemTime::now()).unwrap();

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
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon daemon_run_cycle_skips_recent_targets_by_default
```

Expected: FAIL because `run_cycle_with_safety` does not exist.

- [ ] **Step 3: Add daemon result and safe run method**

In `src/daemon.rs`, import:

```rust
use crate::activity::ProcessInspector;
use crate::safety::{review_project, review_summary, CleanDecision, SafetyOptions};
```

Add this type:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCycleResult {
    pub run_id: i64,
    pub cleaned: i64,
    pub skipped: i64,
    pub bytes_recovered: i64,
    pub errors: i64,
}
```

Add a safe method and make `run_cycle` call it with existing behavior-compatible defaults.

```rust
pub fn run_cycle(&self) -> Result<()> {
    let opts = SafetyOptions {
        target_quiet_period: Duration::ZERO,
        include_managed_cache: true,
        include_active: true,
        force: true,
    };
    self.run_cycle_with_safety(opts, &crate::activity::NoopProcessInspector)?;
    Ok(())
}

pub fn run_cycle_with_safety(
    &self,
    safety: SafetyOptions,
    inspector: &impl ProcessInspector,
) -> Result<RunCycleResult> {
    self.cache.sync_on_disk()?;
    let started = SystemTime::now();
    let run_id = self.store.start_run(started)?;
    let projects = self.store.all_projects()?;
    let project_paths: Vec<_> = projects
        .iter()
        .map(|project| PathBuf::from(project.path.as_str()))
        .collect();
    let scan_errors = self.store.scan_error_paths_since(SystemTime::UNIX_EPOCH)?;
    let activity = inspector.active_projects(&project_paths)?;

    let mut reviews = Vec::new();
    let mut projects_cleaned = 0;
    let mut bytes_recovered = 0;
    let mut errors_count = 0;

    for project in projects {
        let path = PathBuf::from(project.path.as_str());
        let review = review_project(&path, &scan_errors, &activity, started, &safety)?;
        if review.decision != CleanDecision::Cleanable {
            reviews.push(review);
            continue;
        }
        match self.cleaner.clean(&project.path) {
            Ok(result) if result.skipped => {}
            Ok(result) => {
                projects_cleaned += 1;
                bytes_recovered += (result.bytes_before - result.bytes_after).max(0);
                let now = SystemTime::now();
                self.store.record_clean_event(&CleanEvent {
                    id: 0,
                    run_id,
                    ts: now,
                    path: project.path.clone(),
                    bytes_before: result.bytes_before,
                    bytes_after: result.bytes_after,
                    duration_ms: result.duration.as_millis() as i64,
                    exit_code: result.exit_code,
                    stderr_excerpt: result.stderr_excerpt,
                })?;
                self.store.mark_project_cleaned(&project.path, now)?;
            }
            Err(err) => {
                errors_count += 1;
                self.store.record_error(&ErrorRecord {
                    id: 0,
                    ts: SystemTime::now(),
                    category: "clean".to_string(),
                    path: Some(project.path.clone()),
                    message: err.to_string(),
                })?;
            }
        }
        reviews.push(review);
    }

    self.store.finish_run(
        run_id,
        SystemTime::now(),
        projects_cleaned,
        bytes_recovered,
        errors_count,
    )?;
    let summary = review_summary(&reviews);
    Ok(RunCycleResult {
        run_id,
        cleaned: projects_cleaned,
        skipped: summary.skipped_projects as i64,
        bytes_recovered,
        errors: errors_count,
    })
}
```

Add `use std::path::PathBuf;` to `src/daemon.rs`.

- [ ] **Step 4: Run daemon tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/daemon.rs tests/cache_cleaner_daemon.rs
git commit -m "Gate daemon cleaning through safety review"
```

## Task 6: Add Dry-Run and Override Flags to `run`

**Files:**
- Modify: `src/cli.rs`
- Modify: `tests/cli.rs`

- [ ] **Step 1: Write failing CLI dry-run test**

Add `use std::time::Duration;` at the top of `tests/cli.rs`, then add this test.

```rust
#[test]
fn run_dry_run_reports_without_invoking_cargo_clean() {
    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\necho cargo should not run >&2\nexit 2\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\nname='x'\nversion='0.1.0'\n").unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .arg("--dry-run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(contains("Dry run"))
        .stdout(contains("Cleanable projects: 1"));

    assert!(project.join("target/debug/blob.bin").exists());
}
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cli run_dry_run_reports_without_invoking_cargo_clean
```

Expected: FAIL because `--dry-run` is not defined.

- [ ] **Step 3: Add run flags and dry-run review path**

In `src/cli.rs`, change the `Run` command variant.

```rust
Run {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    include_managed_cache: bool,
    #[arg(long)]
    include_active: bool,
    #[arg(long)]
    force: bool,
},
```

Update the match arm.

```rust
Commands::Run {
    config,
    state_dir,
    dry_run,
    include_managed_cache,
    include_active,
    force,
} => run_once(config, state_dir, dry_run, include_managed_cache, include_active, force),
```

Change `run_once` to build `SafetyOptions` from config and flags.

```rust
fn run_once(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    dry_run: bool,
    include_managed_cache: bool,
    include_active: bool,
    force: bool,
) -> Result<()> {
    let path_set = paths_for(state_dir.as_deref());
    let _lock = lockfile::try_acquire(&path_set.lock_path)
        .context("another car-go-clean process is running")?;
    let cfg = load_config(config_path)?;
    let store = open_store_at(&path_set)?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache,
        include_active,
        force,
    };
    if dry_run {
        let reviews = project_reviews(&store, &safety)?;
        print_review_summary("Dry run", &reviews);
        return Ok(());
    }
    let cargo = resolve_cargo_bin(&default_cargo_candidates())?;
    let daemon = daemon_for_clean(&store, &cfg, cargo);
    let result = daemon.run_cycle_with_safety(safety, &crate::activity::SysinfoProcessInspector)?;
    println!(
        "Run complete: cleaned={} skipped={} recovered={} errors={}",
        result.cleaned, result.skipped, result.bytes_recovered, result.errors
    );
    Ok(())
}
```

Add helper functions in `src/cli.rs`.

```rust
fn project_reviews(store: &Store, safety: &SafetyOptions) -> Result<Vec<ProjectReview>> {
    let projects = store.all_projects()?;
    let paths: Vec<PathBuf> = projects
        .iter()
        .map(|project| PathBuf::from(project.path.as_str()))
        .collect();
    let scan_errors = store.scan_error_paths_since(SystemTime::UNIX_EPOCH)?;
    let activity = crate::activity::SysinfoProcessInspector.active_projects(&paths)?;
    projects
        .iter()
        .map(|project| {
            review_project(
                Path::new(&project.path),
                &scan_errors,
                &activity,
                SystemTime::now(),
                safety,
            )
        })
        .collect()
}

fn print_review_summary(label: &str, reviews: &[ProjectReview]) {
    let summary = review_summary(reviews);
    println!("{label}");
    println!("Cached projects: {}", summary.total_projects);
    println!("Cleanable projects: {}", summary.cleanable_projects);
    println!("Skipped projects: {}", summary.skipped_projects);
    println!("Cleanable bytes: {}", summary.cleanable_bytes);
}
```

Add imports:

```rust
use crate::activity::ProcessInspector;
use crate::safety::{review_project, review_summary, ProjectReview, SafetyOptions};
```

- [ ] **Step 4: Run CLI dry-run test**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cli run_dry_run_reports_without_invoking_cargo_clean
```

Expected: PASS.

- [ ] **Step 5: Run all CLI tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cli
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs tests/cli.rs
git commit -m "Add dry-run safe clean command"
```

## Task 7: Add `projects` Command and Upgrade `status`

**Files:**
- Modify: `src/cli.rs`
- Modify: `tests/cli.rs`

- [ ] **Step 1: Write failing `projects` command test**

Add to `tests/cli.rs`.

```rust
#[test]
fn projects_lists_cleanability_and_supports_json() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\nname='x'\nversion='0.1.0'\n").unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("projects")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("cleanable"))
        .stdout(contains(project.display().to_string()));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("projects")
        .arg("--json")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("\"decision\""))
        .stdout(contains("\"cleanable\""));
}
```

- [ ] **Step 2: Write failing status aggregate test**

Add to `tests/cli.rs`.

```rust
#[test]
fn status_prints_safe_cleaning_summary() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\nname='x'\nversion='0.1.0'\n").unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("status")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Cleanable projects: 1"))
        .stdout(contains("Cleanable bytes:"));
}
```

- [ ] **Step 3: Run tests to verify failure**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cli projects_lists_cleanability_and_supports_json
mise exec rust@1.95.0 -- cargo test --test cli status_prints_safe_cleaning_summary
```

Expected: FAIL because `projects` does not exist and `status` has no config-aware safety summary.

- [ ] **Step 4: Add command definitions**

In `src/cli.rs`, change `Status` and add `Projects`.

```rust
Status {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    state_dir: Option<PathBuf>,
},
Projects {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[arg(long)]
    risky: bool,
    #[arg(long)]
    active: bool,
    #[arg(long)]
    json: bool,
},
```

Update match arms.

```rust
Commands::Status { config, state_dir } => status(config, state_dir),
Commands::Projects {
    config,
    state_dir,
    risky,
    active,
    json,
} => projects(config, state_dir, risky, active, json),
```

- [ ] **Step 5: Implement status and projects output**

Replace `status` with:

```rust
fn status(config_path: Option<PathBuf>, state_dir: Option<PathBuf>) -> Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(state_dir.as_deref())?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache: false,
        include_active: false,
        force: false,
    };
    let reviews = project_reviews(&store, &safety)?;
    print_review_summary("Status", &reviews);
    let total = store.total_bytes_recovered(SystemTime::UNIX_EPOCH)?;
    println!("Total bytes recovered (all time): {total}");
    match store.last_run() {
        Ok(run) => println!(
            "Last run: id={} cleaned={} recovered={} errors={}",
            run.id, run.projects_cleaned, run.bytes_recovered, run.errors_count
        ),
        Err(_) => println!("Last run: <none>"),
    }
    Ok(())
}
```

Add:

```rust
fn projects(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    risky: bool,
    active: bool,
    json: bool,
) -> Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(state_dir.as_deref())?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache: risky,
        include_active: active,
        force: false,
    };
    let reviews = project_reviews(&store, &safety)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reviews)?);
        return Ok(());
    }
    for review in reviews {
        println!(
            "{}\t{}\t{}\t{}",
            decision_label(&review.decision),
            class_label(review.class),
            review.target_bytes,
            review.path.display()
        );
    }
    Ok(())
}

fn decision_label(decision: &CleanDecision) -> &'static str {
    match decision {
        CleanDecision::Cleanable => "cleanable",
        CleanDecision::Skipped(SkipReason::NoTarget) => "skipped:no_target",
        CleanDecision::Skipped(SkipReason::ActiveRecentWrite { .. }) => {
            "skipped:active_recent_write"
        }
        CleanDecision::Skipped(SkipReason::ActiveProcess) => "skipped:active_process",
        CleanDecision::Skipped(SkipReason::ManagedCache) => "skipped:managed_cache",
        CleanDecision::Skipped(SkipReason::ContainerStorage) => "skipped:container_storage",
        CleanDecision::Skipped(SkipReason::ScanError) => "skipped:scan_error",
        CleanDecision::Skipped(SkipReason::TargetReadError) => "skipped:target_read_error",
    }
}

fn class_label(class: ProjectClass) -> &'static str {
    match class {
        ProjectClass::Workspace => "workspace",
        ProjectClass::ManagedCache => "managed_cache",
        ProjectClass::ContainerStorage => "container_storage",
    }
}
```

Add imports:

```rust
use crate::safety::{CleanDecision, ProjectClass, SkipReason};
```

- [ ] **Step 6: Run CLI tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cli
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs tests/cli.rs
git commit -m "Add safe projects and status views"
```

## Task 8: Make Daemon Use Safe Defaults

**Files:**
- Modify: `src/daemon.rs`
- Modify: `src/cli.rs`
- Modify: `tests/cache_cleaner_daemon.rs`

- [ ] **Step 1: Write daemon default behavior test**

Update `daemon_scan_and_run_cycle_record_state` so it uses an old-enough target when testing direct `run_cycle`. Replace the `DaemonOptions::default()` argument in that test with:

```rust
DaemonOptions {
    clean_interval: Duration::from_secs(24 * 60 * 60),
    scan_interval: Duration::from_secs(7 * 24 * 60 * 60),
    target_quiet_period: Duration::from_millis(1),
}
```

Add this before `daemon.run_cycle().unwrap();`.

```rust
std::thread::sleep(Duration::from_millis(10));
```

Then change the cleaner safety default test to assert `daemon.run_cycle()` skips a recent target:

```rust
daemon.run_cycle().unwrap();
assert!(runner.calls.lock().unwrap().is_empty());
```

- [ ] **Step 2: Run daemon tests to verify behavior needs update**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon
```

Expected: FAIL until `run_cycle` uses safe defaults instead of force-compatible defaults.

- [ ] **Step 3: Update daemon options**

Add `target_quiet_period` to `DaemonOptions`.

```rust
pub target_quiet_period: Duration,
```

Set its default:

```rust
target_quiet_period: Duration::from_secs(2 * 60 * 60),
```

Change `run_cycle` to:

```rust
pub fn run_cycle(&self) -> Result<()> {
    let opts = SafetyOptions {
        target_quiet_period: self.opts.target_quiet_period,
        include_managed_cache: false,
        include_active: false,
        force: false,
    };
    self.run_cycle_with_safety(opts, &crate::activity::SysinfoProcessInspector)?;
    Ok(())
}
```

In `src/cli.rs`, set daemon options from config in both `daemon_for_scan` and `daemon_for_clean`.

```rust
DaemonOptions {
    clean_interval: cfg.clean_interval,
    scan_interval: cfg.scan_interval,
    target_quiet_period: cfg.target_quiet_period,
}
```

Update existing `DaemonOptions` literals in tests to include `target_quiet_period`. For the shutdown test, use `Duration::from_secs(2 * 60 * 60)` because cleaning should not run before shutdown.

- [ ] **Step 4: Run daemon tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/daemon.rs src/cli.rs tests/cache_cleaner_daemon.rs
git commit -m "Use safe defaults for daemon cleaning"
```

## Task 9: Document Safe Defaults and Fresh Install Validation

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update command documentation**

Add a "Safe cleaning model" section to `README.md`.

```markdown
## Safe cleaning model

By default, `car-go-clean` is designed to be safe against a broad `~` scan.
It only runs `cargo clean` for cached projects that pass all gates:

- `project/target` exists directly under the cached project path.
- The newest non-symlink file under `target/` is at least `target_quiet_period` old.
- The project is not under a known managed cache or container storage path.
- The latest scan did not record a related unreadable path for the project.
- No running process has a current directory or command argument inside the project or its target directory.

The default `target_quiet_period` is `2h`.

Overrides:

- `car-go-clean run --dry-run` prints the exact plan without running `cargo clean`.
- `car-go-clean run --include-managed-cache` allows known managed cache/container paths.
- `car-go-clean run --include-active` allows active process matches.
- `car-go-clean run --force` bypasses safety gates except the direct `target/` requirement.
- `car-go-clean projects` lists cached projects and decisions.
- `car-go-clean projects --risky` previews decisions with managed cache paths allowed.
- `car-go-clean projects --active` previews decisions with active process paths allowed.
- `car-go-clean projects --json` emits structured project review data.
- `car-go-clean logs --errors-only` shows scan and clean diagnostics, including unreadable directories.
```

Add a config snippet.

```toml
# ~/.config/car-go-clean/config.toml
scan_dirs = ["~"]
target_quiet_period = "2h"
clean_interval = "24h"
scan_interval = "7d"
```

- [ ] **Step 2: Add fresh install test instructions**

Add:

```markdown
## Fresh install validation

```bash
mise exec rust@1.95.0 -- cargo install --path . --force
car-go-clean health --skip-cargo
car-go-clean scan
car-go-clean status
car-go-clean projects | head -50
car-go-clean projects --json > /tmp/car-go-clean-projects.json
car-go-clean run --dry-run
car-go-clean logs --errors-only
```

Validation points:

- `status` should show cached project count, cleanable project count, skipped project count, and cleanable bytes.
- `projects` should show why each cached project is cleanable or skipped.
- Unreadable directories such as protected macOS library folders should appear in `logs --errors-only`.
- `run --dry-run` should not delete any `target/` directories.
- A real `run` should clean only rows reported as `cleanable` by the same review policy.
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Document safe cleaning workflow"
```

## Task 10: Full Verification and Push

**Files:**
- Verify all changed files.

- [ ] **Step 1: Format check**

Run:

```bash
mise exec rust@1.95.0 -- cargo fmt -- --check
```

Expected: PASS.

- [ ] **Step 2: Test suite**

Run:

```bash
mise exec rust@1.95.0 -- cargo test
```

Expected: PASS.

- [ ] **Step 3: Clippy**

Run:

```bash
mise exec rust@1.95.0 -- cargo clippy --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Build**

Run:

```bash
mise exec rust@1.95.0 -- cargo build
```

Expected: PASS.

- [ ] **Step 5: Fresh install smoke validation**

Run:

```bash
mise exec rust@1.95.0 -- cargo install --path . --force
car-go-clean health --skip-cargo
car-go-clean scan
car-go-clean status
car-go-clean projects | head -50
car-go-clean run --dry-run
car-go-clean logs --errors-only
```

Expected:

- `health --skip-cargo` prints `OK`.
- `scan` completes without aborting on unreadable folders.
- `status` reports safe cleanability counts.
- `projects` shows per-project decisions.
- `run --dry-run` reports the plan and leaves target directories intact.
- `logs --errors-only` includes scan errors for unreadable folders when the operating system denies access.

- [ ] **Step 6: Push**

```bash
git push
```

Expected: branch is pushed to `git@github.com:dcchuck/car-go-clean.git`.

## Self-Review Checklist

- [ ] The plan covers default `$HOME` scanning, direct target validation, 2 hour quiet period, managed cache/container skips, active process skips, scan error visibility, dry-run, override flags, `projects`, `status`, docs, tests, fresh install validation, and push.
- [ ] All new behavior is test-first.
- [ ] All user-visible skip reasons are explicit and reusable across commands.
- [ ] `--force` still requires direct `project/target`.
- [ ] Unreadable folders remain log-visible without aborting scans.
- [ ] `cargo clean` is invoked only after the safety review returns `CleanDecision::Cleanable`.
