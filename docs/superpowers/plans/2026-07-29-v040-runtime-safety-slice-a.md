# v0.4 Runtime Safety Slice A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make current scope, a current discovery generation, and execution-time filesystem identity mandatory authority for every cleanup.

**Architecture:** Build one immutable `ScopePolicy` per command or daemon cycle, persist discovery generations and observations bound to its deterministic hash, and select cleanup candidates only from the current matching generation. Capture project and target filesystem identity during review and revalidate it immediately before Cargo; refresh exclusion/protected-root and activity snapshots at bounded intervals rather than trusting startup state.

**Tech Stack:** Rust 1.95+, SQLite/rusqlite, serde/serde_json, SHA-256, sysinfo, macOS `sysctl`, Linux `/proc`, existing CLI/daemon/scanner modules.

## Global Constraints

- Do not create, move, or publish `v0.4.0`.
- Do not alter the real Homebrew installation, launchd service, configuration, or state on the development Mac.
- `--no-scan` skips discovery only; it never bypasses scope, generation, exclusion, identity, activity, quiet-period, or managed-storage checks.
- The policy hash is SHA-256 over the exact versioned tuple specified below; `clean_interval` and `log_level` are excluded.
- Relative component exclusions are lexical-only and never canonicalized or working-directory anchored. An absent speculative absolute exclusion is normal; every non-`NotFound` absolute-exclusion canonicalization failure blocks the cycle.
- Persisted device numbers are comparable only inside the same boot session.
- A migrated path-only database grants no current cleanup authority.
- Forced discovery for missing/mismatched policy generations is rate-limited to once per five minutes.
- The final pre-Cargo identity check narrows but does not eliminate TOCTOU.

---

### Task 1: Deterministic Scope Policy and Protected-root Provenance

**Files:**
- Create: `src/policy.rs`
- Modify: `src/lib.rs`
- Modify: `src/config.rs`
- Modify: `src/storage.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Test: `tests/policy.rs`
- Test: `tests/config.rs`
- Test: `tests/safety.rs`

**Interfaces:**
- Consumes: `Config::scan_dirs`, `Config::project_dirs`, `Config::effective_excludes`, `Config::scan_interval`, `Config::target_quiet_period`, and the effective config path.
- Produces: `ScopePolicy::build(&Config, &Path, &dyn Environment) -> Result<ScopePolicy>`, `ScopePolicy::contains_project(&Path) -> bool`, `ScopePolicy::is_excluded(&Path) -> bool`, `ScopePolicy::hash() -> &str`, and `ScopePolicy::diagnostics()`.

- [ ] **Step 1: Add failing policy tests**

Create `tests/policy.rs` with table-driven tests for:

```rust
#[test]
fn policy_hash_is_stable_across_input_order() { /* same roots in different order */ }

#[test]
fn relative_exclusions_are_lexical_only_and_do_not_depend_on_process_working_directory() {
    /* injected canonicalizer errors for relative patterns are never consulted */
}

#[test]
fn policy_hash_changes_for_each_enumerated_authority_input() {
    // Independently vary scan roots, explicit projects, lexical exclusions,
    // canonical exclusions, protected roots/kinds, quiet period, scan
    // interval, and config path.
}

#[test]
fn clean_interval_and_log_level_do_not_change_policy_hash() { /* exact equality */ }

#[test]
fn missing_configured_root_is_an_error() { /* scan root and explicit project */ }

#[test]
fn missing_speculative_exclusion_is_not_an_error() { /* ~/.colima */ }

#[test]
fn unreadable_exclusion_blocks_policy_construction() { /* injected canonicalizer */ }

#[test]
fn relocated_manager_roots_have_environment_provenance() { /* CARGO_HOME etc. */ }
```

Use an injected `Environment` and `Canonicalizer` in tests; do not mutate the process-wide environment.
Exercise `POLICY_HASH_FORMAT_VERSION` variation through a private hash helper
and an in-module unit test. Do not expose a production constructor that accepts
an arbitrary format version.

- [ ] **Step 2: Run the new tests and confirm the missing API fails**

Run:

```bash
cargo test --locked --test policy
```

Expected: compilation fails because `car_go_clean::policy` does not exist.

- [ ] **Step 3: Add the deterministic policy types**

Implement these public/internal shapes in `src/policy.rs`:

```rust
pub const POLICY_HASH_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum ProtectedRootKind {
    Cargo,
    Rustup,
    GoModule,
    Bun,
    Container,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RootProvenance {
    Default,
    Environment(String),
    ServiceDefinition,
    Structural,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ProtectedRoot {
    pub path: PathBuf,
    pub kind: ProtectedRootKind,
    pub provenance: RootProvenance,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopePolicy {
    canonical_scan_roots: Vec<PathBuf>,
    canonical_project_paths: Vec<PathBuf>,
    lexical_exclusions: Vec<String>,
    canonical_exclusions: Vec<PathBuf>,
    protected_roots: Vec<ProtectedRoot>,
    target_quiet_period: Duration,
    scan_interval: Duration,
    config_source: PathBuf,
    hash: String,
}
```

Serialize an internal `PolicyHashInput` with `serde_json::to_vec`, with fields in this exact order:

```rust
struct PolicyHashInput<'a> {
    format_version: u32,
    scan_roots: &'a [PathBuf],
    project_paths: &'a [PathBuf],
    lexical_exclusions: &'a [String],
    canonical_exclusions: &'a [PathBuf],
    protected_roots: &'a [ProtectedRoot],
    target_quiet_period_ms: u128,
    scan_interval_ms: u128,
    config_source: &'a Path,
}
```

Sort and deduplicate each list before hashing. Add `sha2 = "0.10"` and format the 32-byte digest as lowercase hex without adding another dependency.
`ScopePolicy::build` and the canonicalizer-injected test seam always use
`POLICY_HASH_FORMAT_VERSION`; no public API accepts a caller-selected version.

- [ ] **Step 4: Make protected storage provenance explicit**

Refactor `src/storage.rs` so the existing path-only classifier consumes the same protected roots returned by:

```rust
pub trait Environment {
    fn var_os(&self, name: &str) -> Option<OsString>;
}

pub struct ProcessEnvironment;

pub fn protected_roots_for(
    platform: HostPlatform,
    home: &Path,
    environment: &dyn Environment,
) -> Vec<ProtectedRoot>;
```

Cover `CARGO_HOME`, `RUSTUP_HOME`, `XDG_CACHE_HOME`, `XDG_DATA_HOME`, `GOMODCACHE`, supported Bun overrides, and directly discoverable container-data overrides. Retain structural classification as a fallback with `RootProvenance::Structural`.

- [ ] **Step 5: Build a fresh exclusion snapshot per policy**

Implement `Canonicalizer` so production uses `std::fs::canonicalize`. Relative
component/path exclusions are lexical-only: never pass them to the
canonicalizer, anchor them to the working directory, or add them to
`canonical_exclusions`. For absolute exclusions only:

```rust
if path.is_absolute() {
    match canonicalizer.canonicalize(path) {
        Ok(path) => canonical_exclusions.push(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => bail!("canonicalize exclusion {}: {error}", path.display()),
    }
}
```

Keep every lexical pattern active even when an absolute path does not exist.
Require every configured scan root and explicit project to canonicalize
successfully.

- [ ] **Step 6: Run focused and regression tests**

Run:

```bash
cargo test --locked --test policy
cargo test --locked --test config
cargo test --locked --test safety
```

Expected: all pass.

- [ ] **Step 7: Commit Task 1**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/config.rs src/storage.rs src/policy.rs tests/policy.rs tests/config.rs tests/safety.rs
git commit -m "feat: define deterministic cleanup policy"
```

### Task 2: Boot-aware Filesystem Identity

**Files:**
- Create: `src/identity.rs`
- Modify: `src/lib.rs`
- Modify: `src/safety.rs`
- Test: `tests/identity.rs`
- Test: `tests/safety.rs`

**Interfaces:**
- Consumes: direct project and target paths.
- Produces: `IdentityProvider::boot_session`, `IdentityProvider::identity`, `ReviewedIdentity`, and `IdentityComparison`.

- [ ] **Step 1: Write failing identity tests**

Create `tests/identity.rs` covering:

```rust
#[test]
fn same_boot_device_or_inode_change_is_rejected() {}

#[test]
fn different_boot_restats_and_reauthorizes_only_when_still_in_policy() {}

#[test]
fn target_symlink_is_rejected_before_identity_comparison() {}

#[test]
fn project_and_target_on_different_devices_are_rejected() {}

#[test]
fn unavailable_boot_id_treats_persisted_identity_as_stale_not_hostile() {}
```

Use a fake `IdentityProvider`; never require a mount operation in unit tests.

- [ ] **Step 2: Verify the identity tests fail**

Run:

```bash
cargo test --locked --test identity
```

Expected: compilation fails because `identity` types are missing.

- [ ] **Step 3: Implement the platform boundary**

Add:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootSessionId(pub String);

pub trait IdentityProvider {
    fn boot_session(&self) -> Result<Option<BootSessionId>>;
    fn identity(&self, path: &Path) -> Result<FilesystemIdentity>;
}
```

On Unix, use `std::os::unix::fs::MetadataExt` over `symlink_metadata`. Reject symlinks before returning identity. Read Linux boot identity from `/proc/sys/kernel/random/boot_id`; read macOS boot time with `sysctlbyname("kern.boottime")` and encode seconds/microseconds as a stable string.

- [ ] **Step 4: Implement explicit persisted-identity semantics**

Add a pure comparison function:

```rust
pub enum IdentityComparison {
    Matches,
    StaleAcrossBoot,
    Replaced,
}

pub fn compare_persisted(
    observed_boot: Option<&BootSessionId>,
    current_boot: Option<&BootSessionId>,
    observed: &FilesystemIdentity,
    current: &FilesystemIdentity,
) -> IdentityComparison;
```

Same boot requires exact device/inode. A different or unavailable boot returns `StaleAcrossBoot`; the caller must revalidate scope/exclusions and replace the observation. An inode mismatch captured and checked inside one process always returns `Replaced`.

- [ ] **Step 5: Add direct-file and same-device validation**

Extend the review boundary to require direct `Cargo.toml`, direct target directory, and project/target `device` equality. Return typed skip reasons rather than converting identity uncertainty into a generic I/O error.

- [ ] **Step 6: Run identity and safety tests**

Run:

```bash
cargo test --locked --test identity
cargo test --locked --test safety
```

Expected: all pass.

- [ ] **Step 7: Commit Task 2**

```bash
git add src/lib.rs src/identity.rs src/safety.rs tests/identity.rs tests/safety.rs
git commit -m "feat: add boot-aware filesystem identity"
```

### Task 3: Discovery-generation Schema and Atomic Reconciliation

**Files:**
- Modify: `src/store.rs`
- Test: `tests/store.rs`

**Interfaces:**
- Consumes: `ScopePolicy::hash`, `BootSessionId`, per-origin scan completion, and reviewed identities.
- Produces: schema version 9, `Store::reconcile_generation`, `Store::current_generation`, `Store::authorized_observations`, and `Store::has_matching_generation`.

- [ ] **Step 1: Add failing migration and reconciliation tests**

Add tests that open realistic schema versions 1, 4, 7, and 8 and assert:

```rust
assert!(store.current_generation()?.is_none());
assert!(store.authorized_observations(policy_hash)?.is_empty());
assert_eq!(store.all_projects()?.len(), historical_project_count);
assert_eq!(store.total_bytes_recovered(UNIX_EPOCH)?, historical_success_bytes);
```

Add atomic reconciliation tests for successful origins, failed origins, removed projects, changed policy hash, and transaction rollback after an injected failure.

- [ ] **Step 2: Run store tests and confirm failure**

Run:

```bash
cargo test --locked --test store discovery_generation
```

Expected: failure because the generation schema and APIs do not exist.

- [ ] **Step 3: Add schema version 9**

Create these tables and indexes in one migration transaction:

```sql
CREATE TABLE discovery_generations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,
    policy_hash TEXT NOT NULL,
    boot_session_id TEXT
);
CREATE INDEX idx_discovery_generations_policy_created
    ON discovery_generations(policy_hash, created_at DESC);

CREATE TABLE discovery_origins (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    generation_id INTEGER NOT NULL REFERENCES discovery_generations(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('scan_root', 'explicit_project')),
    configured_path TEXT NOT NULL,
    canonical_path TEXT,
    completed INTEGER NOT NULL CHECK(completed IN (0, 1)),
    error TEXT
);

CREATE TABLE project_observations (
    generation_id INTEGER NOT NULL REFERENCES discovery_generations(id) ON DELETE CASCADE,
    origin_id INTEGER NOT NULL REFERENCES discovery_origins(id) ON DELETE CASCADE,
    project_path TEXT NOT NULL,
    project_device INTEGER NOT NULL,
    project_inode INTEGER NOT NULL,
    target_device INTEGER,
    target_inode INTEGER,
    observed_at INTEGER NOT NULL,
    authorized INTEGER NOT NULL CHECK(authorized IN (0, 1)),
    blocked_reason TEXT,
    PRIMARY KEY(generation_id, origin_id, project_path)
);
CREATE INDEX idx_project_observations_authorized
    ON project_observations(generation_id, authorized, project_path);

ALTER TABLE scheduler_state ADD COLUMN last_forced_scan_at INTEGER;
INSERT INTO schema_version(version) VALUES (9);
```

Do not backfill a generation from `projects`; retain those rows only as history.

- [ ] **Step 4: Add typed store records**

Implement:

```rust
pub struct DiscoveryGeneration { pub id: i64, pub created_at: SystemTime, pub policy_hash: String, pub boot_session_id: Option<String> }
pub struct DiscoveryOriginRecord { /* generation, kind, configured/canonical path, completion/error */ }
pub struct ProjectObservation { /* origin, project/target identity, observed_at, authority */ }
pub struct GenerationReconciliation { pub policy_hash: String, pub boot_session_id: Option<String>, pub origins: Vec<OriginReconciliation> }
```

`Store::reconcile_generation` must insert the generation, origins, and observations and update historical `projects` in one unchecked transaction. Failed origins preserve diagnostic rows but insert no authorized observations.

- [ ] **Step 5: Add authority-selection and scheduler APIs**

Implement:

```rust
pub fn current_generation(&self, policy_hash: &str) -> Result<Option<DiscoveryGeneration>>;
pub fn has_matching_generation(&self, policy_hash: &str) -> Result<bool>;
pub fn authorized_observations(&self, generation_id: i64) -> Result<Vec<ProjectObservation>>;
pub fn mark_observation_reverified(&self, generation_id: i64, path: &Path, identity: &ReviewedIdentity) -> Result<()>;
pub fn last_forced_scan_at(&self) -> Result<Option<SystemTime>>;
pub fn record_forced_scan_at(&self, when: SystemTime) -> Result<()>;
```

Cleanup queries must never use `projects` as authority.

- [ ] **Step 6: Run migration and history regressions**

Run:

```bash
cargo test --locked --test store
```

Expected: all existing and new store tests pass, including schema-v8 recovery repair.

- [ ] **Step 7: Commit Task 3**

```bash
git add src/store.rs tests/store.rs
git commit -m "feat: persist discovery authority generations"
```

### Task 4: Origin-aware Scanning and Transactional Authority

**Files:**
- Modify: `src/scanner.rs`
- Modify: `src/daemon.rs`
- Modify: `src/cache.rs`
- Modify: `src/cli.rs`
- Test: `tests/scanner.rs`
- Test: `tests/cache_cleaner_daemon.rs`
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: `ScopePolicy`, `IdentityProvider`, and `Store::reconcile_generation`.
- Produces: `ScanReport::origins`, a matching current generation, and incomplete coverage for failed origins.

- [ ] **Step 1: Write failing origin tests**

Cover:

- two scan roots where one completes and one fails;
- an explicit project origin;
- a project absent from a successfully completed origin;
- a failed origin retaining history but no authority;
- a broken/retargeted root alias;
- worktree discovery attached to the origin that authorized its primary.

- [ ] **Step 2: Run focused scanner tests**

Run:

```bash
cargo test --locked --test scanner origin
cargo test --locked --test cache_cleaner_daemon generation
```

Expected: failure because `ScanReport` lacks origin results.

- [ ] **Step 3: Replace path-only scan results with origin results**

Add:

```rust
pub enum DiscoveryOriginKind { ScanRoot, ExplicitProject }

pub struct DiscoveryOriginResult {
    pub kind: DiscoveryOriginKind,
    pub configured_path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub completed: bool,
    pub error: Option<String>,
    pub projects: Vec<ObservedProject>,
}

pub struct ObservedProject {
    pub path: PathBuf,
    pub project_identity: FilesystemIdentity,
    pub target_identity: Option<FilesystemIdentity>,
}
```

An origin error must not abort unrelated origins. An invalid configured root is a policy error before scanning; traversal errors make that origin incomplete.

- [ ] **Step 4: Reconcile one generation after every scan**

Change daemon and one-shot scan paths to call `Store::reconcile_generation` once after all origins finish. Remove cleanup authorization from `Cache::reconcile_for_review`; keep only historical alias normalization that does not grant authority.

- [ ] **Step 5: Make scan output name generation and incomplete origins**

Text output prints generation ID and each incomplete origin. JSON includes:

```json
{
  "generation": 42,
  "policy_hash": "7d8db91f70fd49f91f5812f213e2b132a80976115a4d4d106ebe45d2405df79a",
  "origins": [{"path": "/Users/me", "completed": false, "error": "permission denied"}],
  "projects": []
}
```

Return `CommandOutcome::Incomplete` when any origin is incomplete.

- [ ] **Step 6: Run scanner, daemon, and CLI tests**

Run:

```bash
cargo test --locked --test scanner
cargo test --locked --test cache_cleaner_daemon
cargo test --locked --test cli
```

Expected: all pass.

- [ ] **Step 7: Commit Task 4**

```bash
git add src/scanner.rs src/daemon.rs src/cache.rs src/cli.rs tests/scanner.rs tests/cache_cleaner_daemon.rs tests/cli.rs
git commit -m "feat: bind scans to authority generations"
```

### Task 5: Review-time and Pre-Cargo Identity Enforcement

**Files:**
- Modify: `src/safety.rs`
- Modify: `src/cleaner.rs`
- Modify: `src/daemon.rs`
- Modify: `src/cli.rs`
- Test: `tests/safety.rs`
- Test: `tests/cache_cleaner_daemon.rs`
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: authorized `ProjectObservation`, current `ScopePolicy`, current exclusion snapshot, and process-local reviewed identity.
- Produces: `ProjectReview.reviewed_identity` and `revalidate_before_clean`.

- [ ] **Step 1: Add failing stale-authority and replacement tests**

Add behavioral tests for:

- removing/narrowing a scan root after discovery;
- removing an explicit project;
- `--no-scan` with no matching generation;
- project replacement between review and clean;
- target symlink/replacement between review and clean;
- cross-device target;
- different-boot reauthorization while still in scope;
- same-generation inode change rejection.

Use a hookable fake cleaner to mutate the filesystem between review and execution.

- [ ] **Step 2: Run focused tests and confirm unsafe behavior**

Run:

```bash
cargo test --locked --test cache_cleaner_daemon identity
cargo test --locked --test cli no_scan
```

Expected: at least one test fails because path-only cached rows still reach review.

- [ ] **Step 3: Make reviews consume authorized observations only**

Change `ProjectReview` to include:

```rust
pub struct ReviewedIdentity {
    pub project: FilesystemIdentity,
    pub target: FilesystemIdentity,
    pub boot_session: Option<BootSessionId>,
}
```

Every review requires an observation from the current generation. A missing or mismatched generation produces incomplete coverage and no cleanable decision.

- [ ] **Step 4: Add immediate pre-Cargo revalidation**

Before `Cleaner::clean`, call:

```rust
pub fn revalidate_before_clean(
    review: &ProjectReview,
    policy: &ScopePolicy,
    identity: &dyn IdentityProvider,
    activity: &mut ActivitySampler,
    now: SystemTime,
) -> Result<ExecutionDecision>;
```

Re-check direct directory/file types, exact process-local identity, same-device relation, scope, exclusions, protected storage, activity, scan diagnostics, and quiet period. A failure becomes a recorded skip/error and never calls Cargo. Do not reauthorize a new identity inside the same run.

- [ ] **Step 5: Run safety and integration tests**

Run:

```bash
cargo test --locked --test safety
cargo test --locked --test cache_cleaner_daemon
cargo test --locked --test cli
```

Expected: all pass and fake Cargo records zero calls for every blocked identity case.

- [ ] **Step 6: Commit Task 5**

```bash
git add src/safety.rs src/cleaner.rs src/daemon.rs src/cli.rs tests/safety.rs tests/cache_cleaner_daemon.rs tests/cli.rs
git commit -m "feat: enforce execution-time cleanup identity"
```

### Task 6: Bounded Activity Refresh and Forced Policy Scan

**Files:**
- Modify: `src/activity.rs`
- Modify: `src/daemon.rs`
- Modify: `src/store.rs`
- Test: `tests/safety.rs`
- Test: `tests/cache_cleaner_daemon.rs`
- Test: `tests/store.rs`

**Interfaces:**
- Consumes: `ProcessInspector`, current policy hash, scheduler state.
- Produces: `ActivitySampler::active_projects_at` and rate-limited forced scanning.

- [ ] **Step 1: Add failing time-controlled tests**

Add a fake clock and counting inspector to assert:

```rust
assert_eq!(inspector.enumerations(), 1); // multiple targets inside 30 seconds
clock.advance(Duration::from_secs(31));
assert_eq!(inspector.enumerations(), 2); // next target refreshes
```

Add daemon tests where `next_scan_at` is hours away but no matching generation exists; the next cycle scans immediately, while restarts inside five minutes do not rescan.

- [ ] **Step 2: Run focused tests and confirm failure**

Run:

```bash
cargo test --locked --test cache_cleaner_daemon forced_scan
cargo test --locked --test safety activity_refresh
```

Expected: failure because the snapshot is cycle-wide and scheduler lacks policy awareness.

- [ ] **Step 3: Implement `ActivitySampler`**

Use:

```rust
pub const ACTIVITY_MAX_AGE: Duration = Duration::from_secs(30);

pub struct ActivitySampler<'a, I: ProcessInspector> {
    inspector: &'a I,
    sampled_at: Option<SystemTime>,
    active: BTreeSet<PathBuf>,
}
```

Refresh only when no sample exists or its age exceeds `ACTIVITY_MAX_AGE`. Consult it immediately before each cleanable target.

- [ ] **Step 4: Force a missing-policy scan safely**

Before waiting for the normal deadline:

```rust
let needs_generation = !store.has_matching_generation(policy.hash())?;
let rate_limit_elapsed = store.last_forced_scan_at()?
    .is_none_or(|last| now.duration_since(last).unwrap_or_default() >= Duration::from_secs(300));
if needs_generation && rate_limit_elapsed {
    schedule.next_scan_at = now;
    store.record_forced_scan_at(now)?;
}
```

Retain existing scan failure backoff after the forced attempt.

- [ ] **Step 5: Run daemon and store suites**

Run:

```bash
cargo test --locked --test cache_cleaner_daemon
cargo test --locked --test store
cargo test --locked --test safety
```

Expected: all pass.

- [ ] **Step 6: Commit Task 6**

```bash
git add src/activity.rs src/daemon.rs src/store.rs tests/safety.rs tests/cache_cleaner_daemon.rs tests/store.rs
git commit -m "feat: refresh authority on bounded schedules"
```

### Task 7: Policy Diagnostics and Slice-A Documentation

**Files:**
- Modify: `src/cli.rs`
- Modify: `README.md`
- Modify: `docs/configuration.md`
- Modify: `docs/releases/v0.4.0.md`
- Test: `tests/cli.rs`
- Test: `tests/packaging.rs`

**Interfaces:**
- Consumes: `ScopePolicy::diagnostics`, current generation, origin completion.
- Produces: stable text and JSON diagnostics without changing `config` TOML output.

- [ ] **Step 1: Write failing diagnostic-contract tests**

Assert `health` and `status` print/configure JSON fields for config source, canonical scope roots, policy hash, current generation, protected roots with provenance, and incomplete origins. Assert:

```rust
let emitted = command("config").stdout;
write_config(&emitted);
command_with_config("config", emitted).assert().success();
```

- [ ] **Step 2: Add text and JSON diagnostics**

Keep `config` configuration-only. Add `--json` to `health` and `status` if not already present, with one serializable diagnostic DTO shared by both commands.

- [ ] **Step 3: Document the authority model**

Document:

- cached projects are history, not authority;
- a matching policy hash and generation are required;
- `--no-scan` does not bypass checks;
- missing speculative exclusions are normal;
- protected-root provenance and service-definition divergence;
- exit `2` for migrated state without a generation;
- residual TOCTOU honestly.

- [ ] **Step 4: Run documentation and CLI tests**

Run:

```bash
cargo test --locked --test cli
cargo test --locked --test packaging
make test-release-notes
```

Expected: all pass.

- [ ] **Step 5: Commit Task 7**

```bash
git add src/cli.rs README.md docs/configuration.md docs/releases/v0.4.0.md tests/cli.rs tests/packaging.rs
git commit -m "docs: explain cleanup authority model"
```

### Task 8: Slice-A Full Verification

**Files:**
- Modify only files required by test failures attributable to Slice A.

**Interfaces:**
- Consumes: Tasks 1–7.
- Produces: a clean, independently reviewable Slice-A commit series.

- [ ] **Step 1: Run formatting**

```bash
cargo fmt --all -- --check
```

- [ ] **Step 2: Run Clippy**

```bash
cargo clippy --all-targets --locked -- -D warnings
```

- [ ] **Step 3: Run the complete suite**

```bash
make test
```

- [ ] **Step 4: Verify database migration from released fixtures**

Run the schema-v1/v4/v7/v8 fixture tests and confirm historical projects, events, errors, and successful recovery totals remain readable while current authority is empty.

- [ ] **Step 5: Inspect the exact diff**

```bash
git diff --check
git status --short
git log --oneline --decorate -8
```

Expected: no whitespace errors, no uncommitted changes, and one commit per task.
