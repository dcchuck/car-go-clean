# One-Shot Cleanup and Agent Quick Start Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a manual `car-go-clean run` scan before reviewing or cleaning, preserve cached-only operation with `--no-scan`, and ship a concise human and agent onboarding path for v0.4.0.

**Architecture:** Keep the daemon scheduler unchanged and compose the existing `Daemon::scan_cycle` into the one-shot CLI path after the process lock, configuration, and state database have been established. Reuse one `scan_and_report` helper for the explicit `scan` command and automatic pre-run scan so fatal scan failures propagate before cleanup. Protect the user-facing behavior with CLI integration tests and protect the onboarding/release contract with repository-content tests.

**Tech Stack:** Rust 2021, clap derive, anyhow, rusqlite, assert_cmd, predicates, tempfile, Markdown, cargo-dist 0.32.0, GitHub Actions, Homebrew tap packaging

## Global Constraints

- Target release is exactly `v0.4.0`; the published `v0.3.0` tag and release remain untouched.
- `car-go-clean run` and `car-go-clean run --dry-run` scan by default.
- `car-go-clean run --no-scan` retains cached-only behavior and relaxes no safety gate.
- Installation and upgrade do not install, start, or restart the daemon.
- Scheduled scans remain controlled by `scan_interval`; scheduled cleans remain controlled by `clean_interval`; a scheduled clean does not gain an implicit scan.
- A fatal scan or scan-persistence error aborts a manual run before Cargo is resolved or invoked.
- Ordinary recorded scan errors continue to block only physically related projects through the existing fail-closed review.
- A real run has no interactive confirmation; `run --dry-run --all` is the recommended preview.
- Do not change the default quiet period, exclusions, activity detection, managed-cache policy, worktree policy, process lock, Cargo invocation, or direct-target safety requirement.
- The Agent Quick Start may authorize inspection, install or upgrade, health checks, and dry run only; it must require confirmation for cleanup, service changes, configuration changes, risky flags, and source builds.
- Normal installation sources are limited to `dcchuck/tap/car-go-clean` and the checksum-verifying installer at `https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh`.

## File Map

- Modify `src/cli.rs`: describe the run interface, parse `--no-scan`, share scan execution, and run discovery before either preview or cleanup.
- Modify `tests/cli.rs`: specify fresh-state dry and real runs, cached-only behavior, help text, fatal scan failure, output ordering, and compatibility with existing cached-state tests.
- Modify `README.md`: add the human Quick Start and exact Agent Quick Start, move optional service activation below them, and tighten the remaining user journey.
- Create `docs/fresh-install-validation.md`: hold source-checkout and released-binary validation, including a fresh-state one-shot Rust-project test.
- Modify `tests/packaging.rs`: enforce README order, canonical installation sources, consent boundaries, validation-document separation, and v0.4.0 metadata.
- Modify `Cargo.toml`: bump the package from `0.3.0` to `0.4.0`.
- Modify `Cargo.lock`: regenerate the root package entry for `0.4.0` without updating dependency versions.
- Modify `docs/releasing.md`: make the guarded release commands and examples target `v0.4.0`.

---

### Task 1: Auto-Scan Manual Runs Without Changing the Daemon

**Files:**
- Modify: `src/cli.rs:148-218`
- Modify: `src/cli.rs:257-304`
- Modify: `src/cli.rs:507-557`
- Test: `tests/cli.rs:8-190`
- Test: `tests/cli.rs:251-339`

**Interfaces:**
- Consumes: existing `daemon_for_scan(&Store, &Config) -> Daemon<'_, RealRunner>`, `Daemon::scan_cycle() -> anyhow::Result<()>`, `run_cycle_with_safety`, process locking, configuration loading, and state migration.
- Produces: `fn scan_and_report(store: &Store, cfg: &Config) -> Result<()>`; a `no_scan: bool` field on `Commands::Run`; and `fn run_once(config_path: Option<PathBuf>, state_dir: Option<PathBuf>, dry_run: bool, no_scan: bool, include_managed_cache: bool, include_active: bool, force: bool, all: bool) -> Result<()>`.

- [ ] **Step 1: Add failing CLI contract tests**

Add these tests to `tests/cli.rs`. Keep each test's state directory private so
the default-scan and cached-only assertions cannot affect each other.

```rust
#[test]
fn run_help_explains_default_scan_and_safety_flags() {
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(contains("Scan for projects, then run one cleanup review/cycle now"))
        .stdout(contains("Show what would be cleaned without invoking Cargo"))
        .stdout(contains("Use cached discovery state instead of scanning first"))
        .stdout(contains("Include projects under managed cache or container storage"))
        .stdout(contains("Include projects used by running processes"))
        .stdout(contains("Bypass policy gates except the direct readable target requirement"));
}

#[test]
fn run_dry_run_scans_fresh_state_by_default() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='fresh-dry-run'\nversion='0.1.0'\n",
    )
    .unwrap();
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
        .args(["run", "--dry-run", "--all", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Scan complete\nDry run"))
        .stdout(contains("Total projects: 1"))
        .stdout(contains("Cleanable projects: 1"))
        .stdout(contains(project.join("target").display().to_string()));

    assert!(project.join("target/debug/blob.bin").exists());
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert_eq!(store.all_projects().unwrap().len(), 1);
}

#[test]
fn run_no_scan_uses_only_cached_state() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--no-scan", "--all", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(predicate::str::contains("Scan complete").not())
        .stdout(contains("Total projects: 0"))
        .stdout(contains("Cleanable projects: 0"));

    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert!(store.all_projects().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn run_scans_fresh_state_before_real_cleanup() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();

    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='fresh-real-run'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", path)
        .assert()
        .success()
        .stdout(contains("Scan complete\nRun complete: cleaned=1"));

    assert!(!project.join("target").exists());
}

#[cfg(unix)]
#[test]
fn run_aborts_before_cargo_when_scan_persistence_fails() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let db_path = state.join("state.db");
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(project.canonicalize().unwrap(), SystemTime::now())
        .unwrap();
    drop(store);
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

    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work.path().join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            "#!/bin/sh\ntouch '{}'\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", path)
        .assert()
        .failure()
        .stderr(contains("injected discovery persistence failure"));

    assert!(!marker.exists());
    assert!(project.join("target/blob.bin").exists());
}
```

- [ ] **Step 2: Run the new tests and verify the behavior is absent**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cli run_help_explains_default_scan_and_safety_flags
mise exec rust@1.95.0 -- cargo test --test cli run_dry_run_scans_fresh_state_by_default
mise exec rust@1.95.0 -- cargo test --test cli run_no_scan_uses_only_cached_state
mise exec rust@1.95.0 -- cargo test --test cli run_scans_fresh_state_before_real_cleanup
mise exec rust@1.95.0 -- cargo test --test cli run_aborts_before_cargo_when_scan_persistence_fails
```

Expected: the help test fails because the descriptions and `--no-scan` are
absent; the default-run tests fail because no project is discovered; the
cached-only invocation is rejected as an unknown argument; and the injected
scan failure test incorrectly reaches Cargo.

- [ ] **Step 3: Describe the run command and thread `no_scan` into execution**

In `src/cli.rs`, add clap doc comments and the new field:

```rust
    /// Refresh the project cache.
    Scan {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Scan for projects, then run one cleanup review/cycle now.
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Show what would be cleaned without invoking Cargo.
        #[arg(long)]
        dry_run: bool,
        /// Use cached discovery state instead of scanning first.
        #[arg(long)]
        no_scan: bool,
        /// Include projects under managed cache or container storage.
        #[arg(long)]
        include_managed_cache: bool,
        /// Include projects used by running processes.
        #[arg(long)]
        include_active: bool,
        /// Bypass policy gates except the direct readable target requirement.
        #[arg(long)]
        force: bool,
        /// Show every cleanable target in dry-run output.
        #[arg(long)]
        all: bool,
    },
    /// Run the long-lived scan and clean scheduler.
    Daemon {
```

Update `Commands::Run` destructuring and the call:

```rust
        Commands::Run {
            config,
            state_dir,
            dry_run,
            no_scan,
            include_managed_cache,
            include_active,
            force,
            all,
        } => run_once(
            config,
            state_dir,
            dry_run,
            no_scan,
            include_managed_cache,
            include_active,
            force,
            all,
        ),
```

Add `no_scan: bool` immediately after `dry_run: bool` in `run_once`.

- [ ] **Step 4: Share the scan cycle and invoke it before either run mode**

Replace the duplicated explicit-scan body with this helper and call:

```rust
fn scan(config_path: Option<PathBuf>, state_dir: Option<PathBuf>) -> Result<()> {
    let path_set = paths_for(state_dir.as_deref());
    let _lock = lockfile::try_acquire(&path_set.lock_path)
        .context("another car-go-clean process is running")?;
    let cfg = load_config(config_path)?;
    let store = open_store_at(&path_set)?;
    scan_and_report(&store, &cfg)
}

fn scan_and_report(store: &Store, cfg: &Config) -> Result<()> {
    daemon_for_scan(store, cfg).scan_cycle()?;
    println!("Scan complete");
    Ok(())
}
```

In `run_once`, place the automatic scan after `open_store_at` and before the
`if dry_run` branch:

```rust
    let store = open_store_at(&path_set)?;

    if !no_scan {
        scan_and_report(&store, &cfg)?;
    }

    if dry_run {
```

Do not modify `Daemon::run_cycle`, `Daemon::run_cycle_with_safety`, or
`Daemon::run_until_shutdown`. This keeps automatic discovery specific to the
manual CLI.

- [ ] **Step 5: Make existing explicitly cached tests state their intent**

In `scan_run_stats_work_with_fake_cargo`, pass both `--force` and
`--no-scan` to the `run` iteration. That test will continue proving the
explicit `scan` followed by cached cleanup flow:

```rust
        if subcommand == "run" {
            cmd.args(["--force", "--no-scan"]);
        }
```

In `cli_reviews_normalize_alias_only_linked_provenance_without_a_prior_scan`,
add `--no-scan` to both run invocations because the fixture intentionally
constructs cached alias provenance without discovery:

```rust
        .args(["run", "--dry-run", "--no-scan", "--all"])
```

and:

```rust
        .args(["run", "--dry-run", "--no-scan", "--force", "--all"])
```

Do not add `--no-scan` to tests whose purpose is ordinary `run` behavior.

- [ ] **Step 6: Run the CLI and daemon regression tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test cli
mise exec rust@1.95.0 -- cargo test --test safety related_scan_error_is_skipped_but_unrelated_error_is_not
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon daemon_uses_persisted_overdue_clean_schedule_after_restart
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon scheduler_scans_before_cleaning_when_equal_deadlines_are_overdue
mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon scheduler_defers_clean_and_retry_after_scan_persistence_failure
```

Expected: all pass. The first daemon regression proves a clean-only deadline
still runs independently, the second preserves scan-before-clean when both
deadlines are due, and the third preserves failure deferral.

- [ ] **Step 7: Format and commit the CLI behavior**

Run:

```bash
mise exec rust@1.95.0 -- cargo fmt --all
mise exec rust@1.95.0 -- cargo fmt --all -- --check
git diff --check
git add src/cli.rs tests/cli.rs
git commit -m "feat: scan before manual cleanup runs"
```

---

### Task 2: Restructure the README and Add Guided Validation

**Files:**
- Modify: `README.md:9-198`
- Create: `docs/fresh-install-validation.md`
- Test: `tests/packaging.rs:35-84`

**Interfaces:**
- Consumes: Task 1's `run` auto-scan contract and `--no-scan` flag; the canonical repository and installation sources in the approved design.
- Produces: top-level `## Quick Start`, `## Agent Quick Start`, and `## Background Service (Optional)` sections; `docs/fresh-install-validation.md`; and repository-content tests that later release work must satisfy.

- [ ] **Step 1: Add failing documentation-contract tests**

Add these tests to `tests/packaging.rs`:

```rust
#[test]
fn readme_prioritizes_human_and_agent_quick_starts() {
    let readme = repo_file("README.md");
    let install = readme.find("## Install").unwrap();
    let quick_start = readme.find("## Quick Start").unwrap();
    let agent_quick_start = readme.find("## Agent Quick Start").unwrap();
    let background_service = readme.find("## Background Service (Optional)").unwrap();

    assert!(install < quick_start);
    assert!(quick_start < agent_quick_start);
    assert!(agent_quick_start < background_service);
    for value in [
        "car-go-clean health",
        "car-go-clean run --dry-run --all",
        "car-go-clean run",
        "car-go-clean stats",
        "scans automatically",
        "no interactive confirmation",
        "car-go-clean run --no-scan",
        "https://github.com/dcchuck/car-go-clean",
        "dcchuck/tap/car-go-clean",
        "releases/latest/download/car-go-clean-installer.sh",
        "Inspection, installation or upgrade, health checks, and the dry run are authorized",
        "Performing actual cleanup",
        "Installing or enabling the background service",
        "Changing configuration or exclusions",
        "Using `--force`, `--include-active`, or `--include-managed-cache`",
        "Cloning and building from source",
    ] {
        assert!(readme.contains(value), "missing {value}");
    }
    assert!(!readme.contains("## Fresh Install Validation"));
}

#[test]
fn maintainer_validation_is_separate_and_proves_fresh_one_shot_use() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = repo_file("README.md");
    let validation = repo_file("docs/fresh-install-validation.md");

    assert!(root.join("docs/fresh-install-validation.md").is_file());
    assert!(readme.contains("[Fresh install validation](docs/fresh-install-validation.md)"));
    for value in [
        "Source checkout",
        "Released binary",
        "cargo new",
        "car-go-clean health",
        "car-go-clean run --dry-run --all",
        "car-go-clean run",
        "car-go-clean scan",
        "must begin with an empty state directory",
        "must not run an explicit scan first",
    ] {
        assert!(validation.contains(value), "missing {value}");
    }
}
```

- [ ] **Step 2: Run the documentation tests and verify they fail**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test packaging readme_prioritizes_human_and_agent_quick_starts
mise exec rust@1.95.0 -- cargo test --test packaging maintainer_validation_is_separate_and_proves_fresh_one_shot_use
```

Expected: both fail because the sections and validation document do not
exist.

- [ ] **Step 3: Add the human Quick Start immediately after installation**

Keep the install methods and supported-target explanation. Replace the pinned
`v0.2.0` example with `VERSION=0.4.0` and `--version "$VERSION"` so the next
release documentation is internally consistent. Collapse the duplicated
daemon-install sentence into one sentence.

Use this pinning example:

```sh
VERSION=0.4.0
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh \
  | sh -s -- --version "$VERSION" --install-dir "$HOME/.local/bin"
```

Insert this section directly after Install:

````markdown
## Quick Start

Check the installation and preview every eligible cleanup target:

```sh
car-go-clean health
car-go-clean run --dry-run --all
```

`run` scans automatically before it reviews or cleans. The preview does not
invoke Cargo, and installation does not start the background service. The
default quiet period, active-process checks, scan-error checks, managed-storage
checks, and direct-target checks all remain in effect.

After reviewing the preview:

```sh
car-go-clean run
car-go-clean stats
```

A real run has no interactive confirmation. For advanced cached-only use,
`car-go-clean run --no-scan` skips discovery but does not relax any safety
gate.
````

- [ ] **Step 4: Add the exact copyable Agent Quick Start**

Place this section after the human Quick Start:

````markdown
## Agent Quick Start

Copy this prompt into your coding agent:

> Install and configure the latest stable release of `car-go-clean` from its
> canonical repository:
>
> https://github.com/dcchuck/car-go-clean
>
> Before acting, read the current README and latest release. Use only these
> official installation sources:
>
> - Homebrew formula: `dcchuck/tap/car-go-clean`
> - Checksum-verifying installer:
>   `https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh`
>
> Do not use a similarly named package from another repository or registry.
>
> First inspect this machine's operating system, architecture, available
> package manager, Cargo availability, existing `car-go-clean` installation,
> configuration, and service status. Recommend Homebrew or the verified shell
> installer and briefly explain why.
>
> Install or upgrade the binary, verify the installed version, and run:
>
> ```sh
> car-go-clean health
> car-go-clean run --dry-run --all
> ```
>
> Explain what would be cleaned, what would be skipped, and why. Then
> recommend either one-shot usage or the background service based on how this
> machine is used.
>
> Inspection, installation or upgrade, health checks, and the dry run are
> authorized by this prompt. Ask before:
>
> - Performing actual cleanup.
> - Installing or enabling the background service.
> - Changing configuration or exclusions.
> - Using `--force`, `--include-active`, or `--include-managed-cache`.
> - Cloning and building from source.
>
> Do not weaken safety checks, manually delete `target/` directories, or work
> around scan errors or process locks. Report blockers and final results
> clearly.
````

- [ ] **Step 5: Reorder the remaining README around the user journey**

Rename `## Explicit Service Activation` to
`## Background Service (Optional)` and place it after Agent Quick Start.
Retain the explicit install/status/restart/uninstall commands and the note
that an already-running daemon must be restarted to load a new binary.

Keep Configuration, Safe Cleaning Model, Commands, Services and Packaging,
and Development after the optional-service section. Update the Commands row
to:

```markdown
| `car-go-clean run` | Scan, then run one cleanup review/cycle now. |
```

Move `Developer Installation` into the Development section. Remove the
top-level `Fresh Install Validation` block and add this line under
Development:

```markdown
See [Fresh install validation](docs/fresh-install-validation.md) for the
source-checkout and released-binary smoke tests.
```

- [ ] **Step 6: Create the maintainer validation document**

Create `docs/fresh-install-validation.md` with this content:

````markdown
# Fresh Install Validation

Use a fresh macOS or Linux VM for a released-binary check. The one-shot test
must begin with an empty state directory and must not run an explicit scan
first.

## Source checkout

From the repository:

```sh
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
mise exec rust@1.95.0 -- cargo test --locked
mise exec rust@1.95.0 -- cargo install --path . --force
```

## Released binary

Install through one official route:

```sh
brew install dcchuck/tap/car-go-clean
```

or:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh | sh
```

Verify that installation alone did not enable the per-user background
service, then confirm the released version:

```sh
car-go-clean version
car-go-clean service status
```

## Fresh-state one-shot flow

Create and build a small Rust project:

```sh
validation_root="$HOME/car-go-clean-validation"
cargo new "$validation_root/sample"
cargo build --manifest-path "$validation_root/sample/Cargo.toml"
validation_config="$validation_root/config.toml"
validation_state="$validation_root/state"
printf 'scan_dirs = ["%s"]\ntarget_quiet_period = "0s"\n' \
  "$validation_root" > "$validation_config"
```

Do not run `car-go-clean scan`. Start with the absent
`$validation_state/state.db` and run:

```sh
car-go-clean health \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean run --dry-run --all \
  --config "$validation_config" \
  --state-dir "$validation_state"
test -d "$validation_root/sample/target"
```

The dry run must print `Scan complete`, report the sample as cleanable, and
leave its target directory intact. After reviewing that output:

```sh
car-go-clean run \
  --config "$validation_config" \
  --state-dir "$validation_state"
test ! -d "$validation_root/sample/target"
car-go-clean stats --state-dir "$validation_state"
```

The real run must print `Scan complete`, clean the sample target, and record
recovered bytes.

## Explicit discovery and diagnostics

Rebuild the sample, then validate the still-supported explicit discovery and
inspection commands:

```sh
cargo build --manifest-path "$validation_root/sample/Cargo.toml"
car-go-clean scan \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean status --state-dir "$validation_state"
car-go-clean projects --all \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean logs --errors-only --state-dir "$validation_state"
```

`status` must show cached projects and the saved review. `projects --all`
must explain every decision. `logs --errors-only` may be empty on a clean
fixture; any entry must name its category and path.
````

- [ ] **Step 7: Run the documentation tests and commit**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test packaging
git diff --check
git add README.md docs/fresh-install-validation.md tests/packaging.rs
git commit -m "docs: add human and agent quick starts"
```

Expected: all packaging tests pass and the top-level README no longer contains
the maintainer validation procedure.

---

### Task 3: Prepare the v0.4.0 Release Metadata

**Files:**
- Modify: `Cargo.toml:1-4`
- Modify: `Cargo.lock:120-123`
- Modify: `docs/releasing.md:3-32`
- Modify: `tests/packaging.rs:101-127`
- Modify: `tests/packaging.rs:214-226`

**Interfaces:**
- Consumes: Task 1's complete CLI behavior and Task 2's complete onboarding contract.
- Produces: package version `0.4.0`, matching lockfile root entry, and a release runbook whose tag, cargo-dist plan, and version-format examples all use `v0.4.0`.

- [ ] **Step 1: Change the metadata tests to require v0.4.0**

In `cargo_dist_metadata_declares_the_public_release_contract`, replace the
manifest version assertion and add a lockfile assertion:

```rust
    let manifest = repo_file("Cargo.toml");
    let lock = repo_file("Cargo.lock");
    let dist = repo_file("dist-workspace.toml");
    for value in [
        "version = \"0.4.0\"",
        "repository = \"https://github.com/dcchuck/car-go-clean\"",
        "homepage = \"https://github.com/dcchuck/car-go-clean\"",
    ] {
        assert!(manifest.contains(value), "missing {value}");
    }
    assert!(lock.contains("name = \"car-go-clean\"\nversion = \"0.4.0\""));
```

Extend `release_runbook_documents_the_guarded_draft_publication_flow`:

```rust
    assert!(runbook.contains("dist plan --tag v0.4.0"));
    assert!(runbook.contains("git tag -a v0.4.0 -m \"car-go-clean v0.4.0\""));
    assert!(runbook.contains("Inspect any older open formula pull request"));
    assert!(!runbook.contains("v0.3.0"));
```

- [ ] **Step 2: Run the metadata tests and verify they fail**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test packaging cargo_dist_metadata_declares_the_public_release_contract
mise exec rust@1.95.0 -- cargo test --test packaging release_runbook_documents_the_guarded_draft_publication_flow
```

Expected: both fail against the current `0.3.0` manifest, lockfile, and
runbook.

- [ ] **Step 3: Bump the package and regenerate only its lock entry**

Change `Cargo.toml`:

```toml
[package]
name = "car-go-clean"
version = "0.4.0"
```

Run:

```bash
mise exec rust@1.95.0 -- cargo check
```

Inspect `git diff -- Cargo.lock`. Expected: the `car-go-clean` root package
entry changes from `0.3.0` to `0.4.0`; dependency packages and checksums do
not change.

- [ ] **Step 4: Update the release runbook to v0.4.0**

In `docs/releasing.md`, change the verified release commit, dist plan, tag,
push command, and version-format example:

```markdown
verified commit that will become `v0.4.0`
```

```bash
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
mise exec rust@1.95.0 -- cargo test --locked
make test-installer
dist plan --tag v0.4.0 --output-format=json
git tag -a v0.4.0 -m "car-go-clean v0.4.0"
git push origin main v0.4.0
```

Use `0.4.0` as the valid three-component installer-version example. Do not
create or push the tag while executing this implementation plan.

Add this paragraph before the verification commands:

```markdown
Inspect any older open formula pull request in `dcchuck/homebrew-tap` before
tagging. Explicitly merge, close, or supersede it according to the version
that should remain installable; do not silently overwrite its branch or the
tap's default branch.
```

- [ ] **Step 5: Verify the version and commit release preparation**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --test packaging
mise exec rust@1.95.0 -- cargo run --locked -- version
git diff --check
```

Expected: packaging tests pass and the version command prints exactly
`0.4.0`.

Commit:

```bash
git add Cargo.toml Cargo.lock docs/releasing.md tests/packaging.rs
git commit -m "chore: prepare v0.4.0 release"
```

---

### Task 4: Run the Complete Release-Readiness Verification

**Files:**
- Verify: `src/cli.rs`
- Verify: `tests/cli.rs`
- Verify: `README.md`
- Verify: `docs/fresh-install-validation.md`
- Verify: `Cargo.toml`
- Verify: `Cargo.lock`
- Verify: `docs/releasing.md`
- Verify: release workflows and packaging templates through existing tests

**Interfaces:**
- Consumes: the committed outputs of Tasks 1 through 3.
- Produces: a clean, fully verified `main` commit sequence ready to push and later tag, without publishing a release or changing the tap repository.

- [ ] **Step 1: Verify formatting and static analysis**

Run:

```bash
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
```

Expected: both exit successfully with no warnings.

- [ ] **Step 2: Run all Rust tests**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --locked
```

Expected: every unit and integration test passes, including CLI, daemon,
safety, service, packaging, scanner, store, and logging suites.

- [ ] **Step 3: Verify the standalone installer**

Run:

```bash
make test-installer
```

Expected: installer validation passes for version parsing, checksum handling,
platform selection, and installation behavior.

- [ ] **Step 4: Verify the generated release plan**

Run:

```bash
dist plan --tag v0.4.0 --output-format=json
```

Expected: cargo-dist accepts `v0.4.0`, resolves all four macOS/Linux targets,
and plans the shell-installer and Homebrew-formula publishers.

- [ ] **Step 5: Inspect the user-visible CLI**

Run:

```bash
mise exec rust@1.95.0 -- cargo run --locked -- run --help
mise exec rust@1.95.0 -- cargo run --locked -- version
```

Expected: help says run scans by default, documents `--no-scan`, dry-run, and
the safety overrides; version prints `0.4.0`.

- [ ] **Step 6: Confirm the repository is ready but unreleased**

Run:

```bash
git diff --check
git status --short --branch
git log -5 --oneline --decorate
git tag --list v0.4.0
```

Expected: no uncommitted changes, the design/plan and three implementation
commits are visible, and `git tag --list v0.4.0` prints nothing. Do not push,
tag, publish, restart the local daemon, or modify `dcchuck/homebrew-tap`
without the user's next explicit instruction.
