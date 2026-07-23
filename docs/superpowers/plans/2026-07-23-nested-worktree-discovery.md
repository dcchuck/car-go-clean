# Nested Git Worktree Discovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Discover and safely clean eligible Rust targets in linked Git worktrees, and reduce the default discovery delay from seven days to one day.

**Architecture:** The scanner keeps normal Cargo-root traversal unchanged, then asks an injectable Git resolver for linked worktrees only when that root is a primary checkout. Canonical in-scope paths flow into a persistent primary-to-linked-worktree association; failed Git discovery blocks every previously associated linked project until a successful scan replaces the association. The daemon combines those persistent blocks with its existing safety review and clamps legacy scheduler deadlines to the effective scan interval.

**Tech Stack:** Rust 2024, `anyhow`, `rusqlite`, `ignore`, `tempfile`, Git porcelain v1 output, `mise` Rust toolchain.

## Global Constraints

- Invoke Git with `std::process::Command`; never use a shell or parse line-delimited output.
- Store and review canonical paths for Git-discovered worktrees.
- Discover only linked worktrees physically inside canonical configured scan roots.
- Do not relax direct-target, quiet-period, active-process, managed-cache, or container-storage safeguards.
- A Git discovery failure must block all linked worktrees previously associated with that primary checkout until a successful replacement discovery.
- Keep explicitly configured `scan_interval` values unchanged; only defaults become one day.

## File Map

- `src/scanner.rs`: Git resolver boundary, NUL-delimited porcelain parser, primary-checkout detection, canonical in-scope linked-worktree discovery, and scan-report outcomes.
- `src/store.rs`: schema migration and persistent primary-to-linked-worktree / discovery-failure records.
- `src/daemon.rs`: persist discovery outcomes, add persistent worktree blocks to the review inputs, and clamp legacy scheduler state.
- `src/config.rs`: one-day default scan interval.
- `tests/scanner.rs`: resolver-driven discovery, canonical scope, ignores, and parsing tests.
- `tests/store.rs`: migration and persistent discovery-block contract tests.
- `tests/cache_cleaner_daemon.rs`: daemon safety and persisted-schedule integration tests.
- `tests/config.rs`: one-day default and explicit interval retention tests.
- `README.md`: daily default and linked-worktree discovery behavior.

---

### Task 1: Persist linked-worktree provenance and failure blocks

**Files:**

- Modify: `src/store.rs`
- Test: `tests/store.rs`

**Interfaces:**

- Produces: `Store::replace_linked_worktrees(&self, primary: &Path, linked: &[PathBuf]) -> Result<()>`
- Produces: `Store::mark_worktree_discovery_failed(&self, primary: &Path, now: SystemTime, message: &str) -> Result<()>`
- Produces: `Store::blocked_linked_worktrees(&self) -> Result<Vec<PathBuf>>`
- Produces: `Store::remove_project(&self, path: impl AsRef<Path>) -> Result<()>` that also removes relations where `path` is either primary or linked.

- [ ] **Step 1: Write failing storage-contract tests**

  Add these tests to `tests/store.rs`:

  ```rust
  #[test]
  fn linked_worktree_failure_blocks_cached_children_until_success() {
      let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
      let primary = Path::new("/workspace/main");
      let linked = PathBuf::from("/workspace/main/.worktrees/feature");
      let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);

      store.replace_linked_worktrees(primary, &[linked.clone()]).unwrap();
      store
          .mark_worktree_discovery_failed(primary, now, "git failed")
          .unwrap();
      assert_eq!(store.blocked_linked_worktrees().unwrap(), vec![linked.clone()]);

      store.replace_linked_worktrees(primary, &[linked]).unwrap();
      assert!(store.blocked_linked_worktrees().unwrap().is_empty());
  }

  #[test]
  fn removing_project_removes_linked_worktree_provenance() {
      let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));
      let primary = Path::new("/workspace/main");
      let linked = PathBuf::from("/workspace/main/.worktrees/feature");
      store.replace_linked_worktrees(primary, &[linked.clone()]).unwrap();
      store.remove_project(primary).unwrap();
      assert!(store.blocked_linked_worktrees().unwrap().is_empty());
  }
  ```

  Extend `open_creates_file_and_migrations_create_tables` to assert the two
  new tables exist.

- [ ] **Step 2: Run the storage test to verify it fails**

  Run: `mise exec rust@1.95.0 -- cargo test --test store`

  Expected: FAIL because the new `Store` methods and migration tables do not
  exist.

- [ ] **Step 3: Add migration version 4 and storage methods**

  In `Store::migrate`, add version 4 after the scheduler-state migration:

  ```sql
  CREATE TABLE IF NOT EXISTS linked_worktrees (
      primary_path TEXT NOT NULL,
      linked_path TEXT NOT NULL,
      PRIMARY KEY (primary_path, linked_path)
  );
  CREATE INDEX IF NOT EXISTS idx_linked_worktrees_linked
      ON linked_worktrees(linked_path);
  CREATE TABLE IF NOT EXISTS worktree_discovery_failures (
      primary_path TEXT PRIMARY KEY,
      failed_at INTEGER NOT NULL,
      message TEXT NOT NULL
  );
  ```

  Implement `replace_linked_worktrees` as one transaction: delete the
  primary's existing rows, insert the deduplicated replacement rows, then
  delete that primary's failure row. Implement
  `mark_worktree_discovery_failed` as an upsert and
  `blocked_linked_worktrees` as the ordered join of `linked_worktrees` with
  `worktree_discovery_failures`. In `remove_project`, delete matching primary
  and linked rows plus a matching failure before deleting the project.

- [ ] **Step 4: Run the storage test to verify it passes**

  Run: `mise exec rust@1.95.0 -- cargo test --test store`

  Expected: PASS.

- [ ] **Step 5: Commit the persistence layer**

  ```bash
  git add src/store.rs tests/store.rs
  git commit -m "feat: persist linked worktree discovery state"
  ```

### Task 2: Discover canonical linked Git worktrees without recursive Cargo traversal

**Files:**

- Modify: `src/scanner.rs`
- Test: `tests/scanner.rs`

**Interfaces:**

- Produces: `pub trait GitWorktreeResolver { fn linked_worktrees(&self, primary: &Path) -> Result<Vec<PathBuf>, GitWorktreeError>; }`
- Produces: `SystemGitWorktreeResolver`, which invokes `git -C <primary> worktree list --porcelain -z` without a shell.
- Produces: `Scanner::with_worktree_resolver(opts: ScannerOptions, resolver: Arc<dyn GitWorktreeResolver>) -> Scanner`.
- Produces: `ScanReport.worktree_discoveries: Vec<WorktreeDiscovery>` where each outcome is `Success { primary, linked }` or `Failure { primary, message }`.

- [ ] **Step 1: Write failing scanner tests with a fake resolver**

  Add a cloneable fake resolver in `tests/scanner.rs` that returns configured
  paths or an error and records its primary-checkout calls. Add these tests:

  ```rust
  #[test]
  fn scan_discovers_ignored_in_scope_linked_worktree_once() {
      let root = tempfile::tempdir().unwrap();
      let primary = root.path().join("router");
      let linked = primary.join(".worktrees/feature");
      fs::create_dir_all(primary.join(".git")).unwrap();
      write_file(&primary.join("Cargo.toml"), "[workspace]\n");
      write_file(&primary.join(".gitignore"), ".worktrees/\n");
      write_file(&linked.join("Cargo.toml"), "[workspace]\n");

      let scanner = Scanner::with_worktree_resolver(
          ScannerOptions { roots: vec![root.path().to_path_buf()], project_dirs: vec![], excludes: vec![] },
          Arc::new(FakeResolver::paths(vec![linked.clone(), linked.clone()])),
      );
      let report = scanner.scan_with_errors().unwrap();
      assert_eq!(report.projects, vec![primary.canonicalize().unwrap(), linked.canonicalize().unwrap()]);
      assert!(matches!(&report.worktree_discoveries[0], WorktreeDiscovery::Success { primary: got, linked: found } if got == &primary.canonicalize().unwrap() && found == &vec![linked.canonicalize().unwrap()]));
  }
  ```

  Add separate tests that reject a fake resolver path outside the canonical
  root, reject a configured-excluded linked path, reject a Unix symlink that
  resolves outside the root, skip a linked path without direct `Cargo.toml`,
  and record a resolver failure while retaining the primary project. Add a
  primary-checkout test proving a `.git` file does not invoke the resolver.

- [ ] **Step 2: Run scanner tests to verify they fail**

  Run: `mise exec rust@1.95.0 -- cargo test --test scanner`

  Expected: FAIL because the resolver boundary, report outcomes, and
  canonical-worktree discovery are absent.

- [ ] **Step 3: Implement resolver-backed worktree discovery**

  Refactor `Scanner` to retain its existing `new` constructor with a
  `SystemGitWorktreeResolver` default and add the injectable constructor for
  tests. Keep the existing `walk` early return after adding a normal Cargo
  root; before returning, call a helper that:

  ```rust
  fn discover_linked_worktrees(
      &self,
      primary: &Path,
      canonical_roots: &[PathBuf],
      found: &mut BTreeSet<PathBuf>,
      outcomes: &mut Vec<WorktreeDiscovery>,
      errors: &mut Vec<ScanError>,
  )
  ```

  The helper runs only when `primary/.git` is a directory. On success, resolve
  each candidate with `fs::canonicalize`, discard candidates outside every
  canonical scan root, candidates rejected by `should_skip`, and candidates
  without direct `Cargo.toml`; then add the remaining canonical paths to both
  the `BTreeSet` and a sorted `Success` outcome. On resolver failure, append a
  `Failure` outcome and a `ScanError` at the canonical primary path.

  Implement the system resolver with `Command::new("git")`, `-C`, and
  `worktree list --porcelain -z`; fail on a nonzero exit or a malformed
  `worktree ` record. Parse NUL-separated records, so paths with whitespace
  and embedded newlines are not split incorrectly. Do not follow raw
  symlinks: all later operations receive the canonical candidate.

- [ ] **Step 4: Run scanner tests to verify they pass**

  Run: `mise exec rust@1.95.0 -- cargo test --test scanner`

  Expected: PASS.

- [ ] **Step 5: Commit scanner discovery**

  ```bash
  git add src/scanner.rs tests/scanner.rs
  git commit -m "feat: discover linked git worktrees"
  ```

### Task 3: Connect discovery state to daemon safety and migrate legacy schedules

**Files:**

- Modify: `src/daemon.rs`
- Modify: `tests/cache_cleaner_daemon.rs`

**Interfaces:**

- Consumes: `ScanReport.worktree_discoveries`, `Store::replace_linked_worktrees`, `Store::mark_worktree_discovery_failed`, and `Store::blocked_linked_worktrees`.
- Produces: `fn clamp_next_scan_at(persisted: SystemTime, now: SystemTime, interval: Duration) -> SystemTime`.
- Produces: `DaemonOptions::default().scan_interval == Duration::from_secs(24 * 60 * 60)`.

- [ ] **Step 1: Write failing daemon tests**

  Add a daemon integration test that scans a primary project and linked target
  through the fake resolver, records a later resolver failure, then runs with
  `NoopProcessInspector` and `FakeRunner { delete_target: true, .. }`:

  ```rust
  assert_eq!(result.cleaned, 0);
  assert!(runner.calls.lock().unwrap().is_empty());
  ```

  Run a subsequent successful scan with the same linked worktree and assert a
  safety review with `target_quiet_period: Duration::ZERO` cleans it. Add a
  focused scheduler test using fixed times:

  ```rust
  let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
  let old_deadline = now + Duration::from_secs(6 * 24 * 60 * 60);
  assert_eq!(
      clamp_next_scan_at(old_deadline, now, Duration::from_secs(24 * 60 * 60)),
      now + Duration::from_secs(24 * 60 * 60),
  );
  ```

  Also assert an earlier persisted deadline remains unchanged and that
  `DaemonOptions::default()` uses one day.

- [ ] **Step 2: Run daemon tests to verify they fail**

  Run: `mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon`

  Expected: FAIL because scan outcomes are not persisted, blocked worktrees
  are not supplied to `review_project`, and no clamp helper exists.

- [ ] **Step 3: Implement outcome persistence, review blocking, and schedule clamping**

  In `Daemon::scan_cycle`, process every `WorktreeDiscovery` after recording
  normal scan errors: success calls `replace_linked_worktrees`; failure calls
  `mark_worktree_discovery_failed` and records the corresponding scan error.
  Continue upserting every discovered canonical project path.

  In `run_cycle_with_safety`, retain the existing recent ordinary scan-error
  query, then append `self.store.blocked_linked_worktrees()?` before calling
  `review_project`. This leaves the existing direct-target review rules intact
  while making every cached child of a failed primary appear as a related scan
  error until the next successful discovery.

  Set `DaemonOptions::default().scan_interval` to one day. Extract the pure
  `clamp_next_scan_at` helper and use it in `scheduler_status_or_initialize`:
  when a persisted scheduler status exists, replace only a deadline later than
  `now + self.opts.scan_interval`, persist the adjusted status, and otherwise
  return the persisted status unchanged.

- [ ] **Step 4: Run daemon tests to verify they pass**

  Run: `mise exec rust@1.95.0 -- cargo test --test cache_cleaner_daemon`

  Expected: PASS.

- [ ] **Step 5: Commit daemon safety and scheduling**

  ```bash
  git add src/daemon.rs tests/cache_cleaner_daemon.rs
  git commit -m "feat: protect linked worktrees after discovery errors"
  ```

### Task 4: Expose the daily default and document behavior

**Files:**

- Modify: `src/config.rs`
- Modify: `tests/config.rs`
- Modify: `README.md`

**Interfaces:**

- Produces: `Config::default().scan_interval == Duration::from_secs(24 * 60 * 60)`.
- Preserves: `load` returns an explicitly configured `scan_interval` unchanged.

- [ ] **Step 1: Write failing configuration tests**

  In `tests/config.rs`, change the default interval assertion to one day and
  add an explicit-retention assertion to `load_file_overlays_defaults_and_expands_paths`:

  ```rust
  assert_eq!(cfg.scan_interval, Duration::from_secs(2 * 60 * 60));
  ```

  Keep the configuration fixture's `scan_interval = "2h"` unchanged.

- [ ] **Step 2: Run configuration tests to verify they fail**

  Run: `mise exec rust@1.95.0 -- cargo test --test config`

  Expected: FAIL because the default remains seven days.

- [ ] **Step 3: Implement the default and README updates**

  Change `default_scan_interval` in `src/config.rs` to
  `Duration::from_secs(24 * 60 * 60)`. In `README.md`, change the sample to
  `scan_interval = "1d"`, state that a primary Git checkout discovers its
  in-scope linked Rust worktrees, and document that Git discovery failures
  temporarily block those previously discovered linked worktrees from cleanup.

- [ ] **Step 4: Run configuration and documentation-adjacent tests**

  Run: `mise exec rust@1.95.0 -- cargo test --test config`

  Expected: PASS.

- [ ] **Step 5: Run formatting and the complete test suite**

  Run: `mise exec rust@1.95.0 -- cargo fmt --check`

  Expected: PASS with no formatting changes.

  Run: `mise exec rust@1.95.0 -- cargo test`

  Expected: PASS with all unit and integration tests green.

- [ ] **Step 6: Commit configuration and documentation**

  ```bash
  git add src/config.rs tests/config.rs README.md
  git commit -m "docs: describe daily linked worktree scans"
  ```

