# Platform-Aware Default Exclusions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep broad `$HOME` discovery while pruning operating-system, package-manager, container, and VM storage before traversal, then remove already cached state that has become excluded.

**Architecture:** Build editable, platform-aware exclusion profiles in `config`, continue using the scanner's existing pre-traversal matcher, and expose that matcher to the daemon for state reconciliation. Add one transactional `Store` operation that removes only explicitly excluded project and worktree-discovery state while retaining historical diagnostics.

**Tech Stack:** Rust 2021, Rust 1.95.0 toolchain and minimum, `rusqlite`, existing scanner/config/store abstractions, Cargo integration tests.

## Global Constraints

- Keep the default scan root as `$HOME`; do not replace it with source-directory allowlists.
- Add no dependencies.
- Represent operating-system and manager roots as absolute `$HOME`-anchored paths; do not use broad relative names such as `Library` or `OrbStack`.
- Keep `.git` and `node_modules` as editable component defaults.
- Keep `target` as a hard, non-configurable scanner exclusion.
- An explicit `excludes` array replaces the compiled defaults; do not merge defaults back into it.
- Keep managed-cache and container-storage classification as defense in depth.
- Reconciliation may remove only application state that explicitly matches an active exclusion.
- Never delete project files, `target/` directories, container data, clean-event history, or historical error records during reconciliation.
- Preserve the cleaner command as `cargo clean --target-dir <project>/target` with `CARGO_TARGET_DIR` removed.
- The current installation has no config file, so no config-file migration or edit is part of implementation.
- Do not bump the package version or publish a release in this plan.

---

### Task 1: Build Platform-Aware Default Profiles

**Files:**

- Modify: `src/config.rs:24-38`
- Modify: `src/config.rs:177-190`
- Modify: `tests/config.rs:1-22`
- Test: `src/config.rs` inline unit-test module
- Test: `tests/scanner.rs`

**Interfaces:**

- Consumes: existing `home_dir() -> PathBuf` and `Config::default()`.
- Produces: private `HostPlatform`, `HostPlatform::current()`, and `default_excludes_for(home: &Path, platform: HostPlatform) -> Vec<String>`.
- Preserves: `Config.excludes: Vec<String>` and explicit-config replacement semantics.

- [ ] **Step 1: Add failing profile tests for macOS and Linux**

Append an inline test module to `src/config.rs` so both profiles are verified
on every CI host:

```rust
#[cfg(test)]
mod default_exclude_tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn macos_defaults_anchor_managed_and_platform_paths_to_home() {
        let excludes =
            default_excludes_for(Path::new("/Users/tester"), HostPlatform::MacOs);

        assert_eq!(
            excludes,
            strings(&[
                ".git",
                "node_modules",
                "/Users/tester/.cargo",
                "/Users/tester/.rustup",
                "/Users/tester/.cache",
                "/Users/tester/.bun/install/cache",
                "/Users/tester/go/pkg/mod",
                "/Users/tester/.colima",
                "/Users/tester/.lima",
                "/Users/tester/.local/share/containers",
                "/Users/tester/Library",
                "/Users/tester/.Trash",
                "/Users/tester/OrbStack",
            ])
        );
        assert!(!excludes.iter().any(|entry| entry == "target"));
    }

    #[test]
    fn linux_defaults_cover_rootless_container_and_desktop_vm_storage() {
        let excludes =
            default_excludes_for(Path::new("/home/tester"), HostPlatform::Linux);

        assert_eq!(
            excludes,
            strings(&[
                ".git",
                "node_modules",
                "/home/tester/.cargo",
                "/home/tester/.rustup",
                "/home/tester/.cache",
                "/home/tester/.bun/install/cache",
                "/home/tester/go/pkg/mod",
                "/home/tester/.colima",
                "/home/tester/.lima",
                "/home/tester/.local/share/containers",
                "/home/tester/.local/share/docker",
                "/home/tester/.docker/desktop",
                "/home/tester/.local/share/rancher-desktop",
                "/home/tester/.local/share/Trash",
            ])
        );
        assert!(!excludes.iter().any(|entry| entry == "target"));
    }

    #[test]
    fn missing_or_relative_home_never_creates_unanchored_manager_patterns() {
        assert_eq!(
            default_excludes_for(Path::new(""), HostPlatform::MacOs),
            strings(&[".git", "node_modules"])
        );
        assert_eq!(
            default_excludes_for(Path::new("relative-home"), HostPlatform::Linux),
            strings(&[".git", "node_modules"])
        );
    }
}
```

- [ ] **Step 2: Run the new unit tests and verify they fail**

Run:

```bash
cargo test config::default_exclude_tests --lib
```

Expected: compilation fails because `HostPlatform` and
`default_excludes_for` do not exist.

- [ ] **Step 3: Implement the platform profile builder**

Replace the existing `default_excludes` implementation in `src/config.rs`
with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPlatform {
    MacOs,
    Linux,
    Other,
}

impl HostPlatform {
    fn current() -> Self {
        match env::consts::OS {
            "macos" => Self::MacOs,
            "linux" => Self::Linux,
            _ => Self::Other,
        }
    }
}

fn default_excludes() -> Vec<String> {
    default_excludes_for(&home_dir(), HostPlatform::current())
}

fn default_excludes_for(home: &Path, platform: HostPlatform) -> Vec<String> {
    let mut excludes = vec![".git".to_string(), "node_modules".to_string()];

    if home.is_absolute() {
        for relative in [
            ".cargo",
            ".rustup",
            ".cache",
            ".bun/install/cache",
            "go/pkg/mod",
            ".colima",
            ".lima",
            ".local/share/containers",
        ] {
            excludes.push(home.join(relative).to_string_lossy().into_owned());
        }

        let platform_paths: &[&str] = match platform {
            HostPlatform::MacOs => &["Library", ".Trash", "OrbStack"],
            HostPlatform::Linux => &[
                ".local/share/docker",
                ".docker/desktop",
                ".local/share/rancher-desktop",
                ".local/share/Trash",
            ],
            HostPlatform::Other => &[],
        };
        excludes.extend(
            platform_paths
                .iter()
                .map(|relative| home.join(relative).to_string_lossy().into_owned()),
        );
    }

    excludes
}
```

Do not include `target`; `Scanner::should_skip` already enforces it
independently of configuration.

- [ ] **Step 4: Update the host-level default-config test**

Replace the final assertion in
`tests/config.rs::default_config_scans_home_and_has_intervals` with:

```rust
    assert!(cfg.excludes.contains(&".git".to_string()));
    assert!(cfg.excludes.contains(&"node_modules".to_string()));
    assert!(cfg
        .excludes
        .contains(&PathBuf::from(&home).join(".cargo").to_string_lossy().into_owned()));
    assert!(cfg
        .excludes
        .contains(&PathBuf::from(&home).join(".rustup").to_string_lossy().into_owned()));
    assert!(!cfg.excludes.contains(&"target".to_string()));

    match std::env::consts::OS {
        "macos" => {
            assert!(cfg.excludes.contains(
                &PathBuf::from(&home)
                    .join("OrbStack")
                    .to_string_lossy()
                    .into_owned()
            ));
        }
        "linux" => {
            assert!(cfg.excludes.contains(
                &PathBuf::from(&home)
                    .join(".local/share/rancher-desktop")
                    .to_string_lossy()
                    .into_owned()
            ));
        }
        _ => {}
    }
```

- [ ] **Step 5: Run the configuration tests**

Run:

```bash
cargo test --test config
cargo test config::default_exclude_tests --lib
```

Expected: both commands pass.

- [ ] **Step 6: Add a scanner characterization test for anchored pre-traversal pruning**

Add to `tests/scanner.rs`:

```rust
#[test]
fn absolute_home_exclusion_prunes_before_manifest_and_git_discovery() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let excluded = home.join("Library");
    let legitimate = home.join("code/Library/project");
    write_file(
        &excluded.join("container-copy/Cargo.toml"),
        "[package]\nname='container-copy'\nversion='0.1.0'\n",
    );
    fs::create_dir_all(excluded.join("container-copy/.git")).unwrap();
    write_file(
        &legitimate.join("Cargo.toml"),
        "[package]\nname='project'\nversion='0.1.0'\n",
    );
    let resolver = FakeResolver::failure("excluded Git repository was inspected");
    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![home],
            project_dirs: vec![],
            excludes: vec![excluded.to_string_lossy().into_owned()],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();

    assert_eq!(
        report.projects,
        vec![legitimate.canonicalize().unwrap()]
    );
    assert!(report.errors.is_empty());
    assert!(resolver.calls().is_empty());
}
```

- [ ] **Step 7: Run the scanner characterization test**

Run:

```bash
cargo test --test scanner absolute_home_exclusion_prunes_before_manifest_and_git_discovery
```

Expected: PASS, demonstrating that the existing scanner matcher already
prunes an absolute home root before manifest or Git discovery.

- [ ] **Step 8: Format, verify the task, and commit**

Run:

```bash
cargo fmt --all
cargo test --test config
cargo test --test scanner
cargo test --lib
git add src/config.rs tests/config.rs tests/scanner.rs
git commit -m "feat: add platform-aware scan exclusions"
```

Expected: all tests pass and the commit contains only Task 1 files.

---

### Task 2: Add Transactional Excluded-State Reconciliation

**Files:**

- Modify: `src/store.rs:342-363`
- Test: `tests/store.rs`

**Interfaces:**

- Consumes: `Store` tables `projects`, `linked_worktrees`, and
  `worktree_discovery_failures`.
- Produces:
  `Store::reconcile_excluded_discovery_state<F>(&self, is_excluded: F) -> Result<()>`
  where `F: FnMut(&Path) -> bool`.
- Preserves: `clean_events`, `errors`, `runs`, review status, scheduler state,
  and every discovery row for which the predicate returns `false`.

- [ ] **Step 1: Write the failing transactional reconciliation test**

Add to `tests/store.rs`:

```rust
#[test]
fn reconcile_excluded_discovery_state_prunes_only_matching_active_state() {
    let root = tempfile::tempdir().unwrap();
    let excluded_root = root.path().join("OrbStack");
    let excluded_primary = excluded_root.join("docker/primary");
    let excluded_linked = excluded_root.join("docker/linked");
    let kept_primary = root.path().join("src/primary");
    let kept_linked = root.path().join("src/linked");
    for path in [
        &excluded_primary,
        &excluded_linked,
        &kept_primary,
        &kept_linked,
    ] {
        fs::create_dir_all(path).unwrap();
    }

    let excluded_root = excluded_root.canonicalize().unwrap();
    let excluded_primary = excluded_primary.canonicalize().unwrap();
    let excluded_linked = excluded_linked.canonicalize().unwrap();
    let kept_primary = kept_primary.canonicalize().unwrap();
    let kept_linked = kept_linked.canonicalize().unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let store = test_store(&tempfile::tempdir().unwrap().path().join("state.db"));

    for path in [
        &excluded_primary,
        &excluded_linked,
        &kept_primary,
        &kept_linked,
    ] {
        store.upsert_project(path, now).unwrap();
    }
    store
        .replace_linked_worktrees(
            &excluded_primary,
            std::slice::from_ref(&excluded_linked),
        )
        .unwrap();
    store
        .mark_worktree_discovery_failed(&excluded_primary, now, "excluded failure")
        .unwrap();
    store
        .replace_linked_worktrees(
            &kept_primary,
            &[kept_linked.clone(), excluded_linked.clone()],
        )
        .unwrap();
    store
        .mark_worktree_discovery_failed(&kept_primary, now, "kept failure")
        .unwrap();
    store
        .record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "worktree_discovery".to_string(),
            path: Some(excluded_primary.to_string_lossy().into_owned()),
            message: "historical error".to_string(),
        })
        .unwrap();
    let run_id = store.start_run(now).unwrap();
    store
        .record_clean_event(&CleanEvent {
            id: 0,
            run_id,
            ts: now,
            path: excluded_primary.to_string_lossy().into_owned(),
            bytes_before: 1024,
            bytes_after: 0,
            duration_ms: 10,
            exit_code: 0,
            stderr_excerpt: String::new(),
        })
        .unwrap();

    store
        .reconcile_excluded_discovery_state(|path| path.starts_with(&excluded_root))
        .unwrap();

    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| PathBuf::from(project.path))
            .collect::<Vec<_>>(),
        vec![kept_linked.clone(), kept_primary.clone()]
    );
    assert!(!store
        .is_active_worktree_discovery_identity(&excluded_primary)
        .unwrap());
    assert!(!store
        .is_active_worktree_discovery_identity(&excluded_linked)
        .unwrap());
    assert!(store
        .is_active_worktree_discovery_identity(&kept_primary)
        .unwrap());
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 1);
    assert_eq!(
        store
            .clean_events_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .len(),
        1
    );
}
```

- [ ] **Step 2: Run the new store test and verify it fails**

Run:

```bash
cargo test --test store reconcile_excluded_discovery_state_prunes_only_matching_active_state
```

Expected: compilation fails because
`Store::reconcile_excluded_discovery_state` does not exist.

- [ ] **Step 3: Implement transactional reconciliation**

Add this method to `impl Store` near `remove_project`:

```rust
    pub fn reconcile_excluded_discovery_state<F>(
        &self,
        mut is_excluded: F,
    ) -> Result<()>
    where
        F: FnMut(&Path) -> bool,
    {
        let tx = self.conn.unchecked_transaction()?;

        let projects = {
            let mut stmt = tx.prepare("SELECT path FROM projects")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            collect_rows(rows)?
        };
        for path in projects {
            if is_excluded(Path::new(&path)) {
                tx.execute("DELETE FROM projects WHERE path=?1", [&path])?;
            }
        }

        let linked = {
            let mut stmt = tx.prepare(
                "
                SELECT primary_path, linked_path, canonical_primary_path
                FROM linked_worktrees
                ",
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
        for (primary, linked, canonical_primary) in linked {
            let remove = is_excluded(Path::new(&primary))
                || is_excluded(Path::new(&linked))
                || canonical_primary
                    .as_deref()
                    .is_some_and(|path| is_excluded(Path::new(path)));
            if remove {
                tx.execute(
                    "
                    DELETE FROM linked_worktrees
                    WHERE primary_path=?1 AND linked_path=?2
                    ",
                    params![primary, linked],
                )?;
            }
        }

        let failures = {
            let mut stmt = tx.prepare(
                "
                SELECT primary_path, canonical_primary_path
                FROM worktree_discovery_failures
                ",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })?;
            collect_rows(rows)?
        };
        for (primary, canonical_primary) in failures {
            let remove = is_excluded(Path::new(&primary))
                || canonical_primary
                    .as_deref()
                    .is_some_and(|path| is_excluded(Path::new(path)));
            if remove {
                tx.execute(
                    "DELETE FROM worktree_discovery_failures WHERE primary_path=?1",
                    [&primary],
                )?;
            }
        }

        tx.commit()?;
        Ok(())
    }
```

Keep all reads and deletes inside the same SQLite transaction. Do not call
`remove_project`, because that existing method intentionally preserves
worktree provenance for ordinary on-disk cache eviction.

- [ ] **Step 4: Run store tests**

Run:

```bash
cargo fmt --all
cargo test --test store reconcile_excluded_discovery_state_prunes_only_matching_active_state
cargo test --test store
```

Expected: the focused test and full store suite pass.

- [ ] **Step 5: Commit the store boundary**

Run:

```bash
git add src/store.rs tests/store.rs
git commit -m "feat: reconcile excluded discovery state"
```

Expected: the commit contains only the transactional store behavior and its
tests.

---

### Task 3: Reconcile Exclusions During Every Successful Scan

**Files:**

- Modify: `src/scanner.rs:364-373`
- Modify: `src/daemon.rs:104-151`
- Test: `tests/cache_cleaner_daemon.rs`

**Interfaces:**

- Consumes:
  `Store::reconcile_excluded_discovery_state<F>(&self, F) -> Result<()>`.
- Produces:
  `Scanner::is_excluded(&self, path: &Path) -> bool` with crate visibility.
- Guarantees: reconciliation runs only after `scan_with_errors` returns a
  report and alias normalization succeeds, and before current discoveries are
  persisted.

- [ ] **Step 1: Write the failing daemon regression test**

Add to `tests/cache_cleaner_daemon.rs`:

```rust
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
        Scanner::with_worktree_resolver(
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

    assert_eq!(
        store
            .all_projects()
            .unwrap()
            .into_iter()
            .map(|project| PathBuf::from(project.path))
            .collect::<Vec<_>>(),
        vec![kept.clone()]
    );
    assert!(store.blocked_worktree_discovery_paths().unwrap().is_empty());
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
```

- [ ] **Step 2: Run the daemon regression and verify it fails**

Run:

```bash
cargo test --test cache_cleaner_daemon successful_scan_prunes_cached_excluded_candidates_and_failures
```

Expected: FAIL because the existing excluded cached row and active failure
remain after `scan_cycle`.

- [ ] **Step 3: Expose the scanner's existing exclusion decision**

Add to `impl Scanner` immediately before private `should_skip`:

```rust
    pub(crate) fn is_excluded(&self, path: &Path) -> bool {
        self.should_skip(path)
    }
```

Do not duplicate path-matching logic in the daemon or store.

- [ ] **Step 4: Wire reconciliation into `Daemon::scan_cycle`**

In `src/daemon.rs`, keep error recording first, then change the state section
to:

```rust
        self.store.normalize_resolvable_project_aliases()?;
        self.store
            .reconcile_excluded_discovery_state(|path| self.scanner.is_excluded(path))?;
        for discovery in report.worktree_discoveries {
```

This ordering canonicalizes stored aliases before matching, removes old
excluded state, and then applies the current scan's worktree-discovery
results and project upserts.

- [ ] **Step 5: Run focused and full daemon/scanner tests**

Run:

```bash
cargo fmt --all
cargo test --test cache_cleaner_daemon successful_scan_prunes_cached_excluded_candidates_and_failures
cargo test --test cache_cleaner_daemon
cargo test --test scanner
```

Expected: all commands pass. The regression must retain one historical error,
remove the active failure, and invoke the cleaner only for the kept project.

- [ ] **Step 6: Commit scan-cycle reconciliation**

Run:

```bash
git add src/scanner.rs src/daemon.rs tests/cache_cleaner_daemon.rs
git commit -m "feat: prune excluded cached projects after scans"
```

Expected: the commit contains the scanner interface, daemon wiring, and
end-to-end regression.

---

### Task 4: Document Defaults and Run Full Verification

**Files:**

- Modify: `README.md:83-110`
- Modify: `docs/configuration.md:1-36`

**Interfaces:**

- Consumes: the exact profile lists implemented in Task 1.
- Produces: concise README guidance and the complete configuration reference.
- Preserves: README as an overview; detailed lists remain in
  `docs/configuration.md`.

- [ ] **Step 1: Confirm the current docs lack the new contract**

Run:

```bash
rg -n '\$HOME/OrbStack|discovery candidate|platform-aware' README.md docs/configuration.md
```

Expected: no matches and exit status 1.

- [ ] **Step 2: Add the full defaults section to the configuration reference**

Insert after the introductory defaults paragraph in
`docs/configuration.md`:

```markdown
### Default exclusions

The scanner always prunes `target` because it is build output. The editable
component defaults `.git` and `node_modules` apply wherever those directory
names occur.

The following editable defaults are anchored to `$HOME`:

- All supported hosts: `.cargo`, `.rustup`, `.cache`,
  `.bun/install/cache`, `go/pkg/mod`, `.colima`, `.lima`, and
  `.local/share/containers`.
- macOS: `Library`, `.Trash`, and `OrbStack`.
- Linux: `.local/share/docker`, `.docker/desktop`,
  `.local/share/rancher-desktop`, and `.local/share/Trash`.

Docker Desktop and Rancher Desktop data on macOS are covered by `Library`.
System-wide Docker Engine data on Linux normally lives outside `$HOME`.

An explicit `excludes` array replaces these editable defaults. Excluded
trees are pruned before filesystem or Git inspection. After a successful
scan, cached discovery candidates and active worktree-discovery state that
now match an exclusion are removed; project files and historical diagnostics
are retained.
```

Also add this clarification to the Scan Scope section:

```markdown
A discovery candidate is any directory containing `Cargo.toml`. It becomes a
valid cleanup target only when its direct, non-symlink `target/` exists and
all safety gates pass.
```

- [ ] **Step 3: Add one concise README bullet**

Add beneath the existing `scan_dirs` bullet in `README.md`:

```markdown
- Platform-aware defaults prune operating-system, package-manager, container,
  and VM storage before traversal; see the configuration reference for the
  exact macOS and Linux lists.
```

Do not duplicate the full lists in the README.

- [ ] **Step 4: Verify documentation content**

Run:

```bash
rg -n '\$HOME/OrbStack|discovery candidate|platform-aware|rancher-desktop' README.md docs/configuration.md
git diff --check
```

Expected: the new terms appear in the intended files and `git diff --check`
reports no whitespace errors.

- [ ] **Step 5: Run the complete repository verification**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
make test-installer
```

Expected:

- formatting exits 0;
- Clippy exits 0 with no warnings;
- every locked Rust test passes;
- the shell installer test passes.

- [ ] **Step 6: Review the final diff against the approved design**

Run:

```bash
git status --short
git diff --stat HEAD~3
git diff HEAD~3 -- src/config.rs src/scanner.rs src/store.rs src/daemon.rs README.md docs/configuration.md
```

Confirm:

- no version or dependency files changed;
- macOS and Linux paths are home-anchored;
- explicit configuration still replaces defaults;
- historical `errors` and `clean_events` are never deleted;
- the scanner and daemon share one exclusion matcher;
- all new behavior has focused tests.

- [ ] **Step 7: Commit documentation**

Run:

```bash
git add README.md docs/configuration.md
git commit -m "docs: explain platform-aware scan defaults"
```

Expected: the worktree is clean after the documentation commit.

---

## Post-Release Acceptance on the Current Mac

Run these steps only after a release containing this implementation has been
installed through Homebrew. They are operational acceptance checks, not part
of the implementation commits.

- [ ] Restart the installed daemon and run one successful scan:

```bash
car-go-clean service restart
car-go-clean scan
```

- [ ] Confirm OrbStack has zero cached discovery candidates and zero active
  worktree failures:

```bash
sqlite3 -readonly "$HOME/.local/state/car-go-clean/state.db" \
  "SELECT COUNT(*) FROM projects WHERE path LIKE '$HOME/OrbStack/%';"
sqlite3 -readonly "$HOME/.local/state/car-go-clean/state.db" \
  "SELECT COUNT(*) FROM worktree_discovery_failures
   WHERE primary_path LIKE '$HOME/OrbStack/%'
      OR canonical_primary_path LIKE '$HOME/OrbStack/%';"
```

Expected: both queries print `0`.

- [ ] Confirm ordinary projects remain visible and the daemon is healthy:

```bash
car-go-clean status
car-go-clean run --dry-run
car-go-clean health
```

Historical errors may remain in the 24-hour health window. Acceptance
requires no newly recorded OrbStack scan or worktree-discovery errors after
the upgraded scan.
