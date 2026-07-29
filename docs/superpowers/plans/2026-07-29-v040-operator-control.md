# v0.4 Operator Control Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let operators execute exactly a persisted review, make output machine-actionable, and make service and upgrade behavior persistently correct on macOS and Linux.

**Architecture:** Persist immutable review plans bound to the Runtime Safety Slice-A policy hash and discovery generation. Route dynamic runs and reviewed runs through one execution engine, with reviewed execution allowed only to remove targets after safety revalidation. Extend the service manager to model definition, enablement, and process state separately, capture protected-root environment into service definitions, and ship a tested upgrade helper that understands exit `0`/`2`/`1`.

**Tech Stack:** Rust 1.88+, clap, serde/serde_json, SQLite/rusqlite, launchctl, systemd user services, POSIX shell integration tests.

## Global Constraints

- Runtime Safety Slice A and Slice B must be complete first.
- Do not alter the real installed binary, launchd service, config, or state on the development Mac.
- Every dry run persists a plan; `--all` remains display-only.
- Plans expire after 30 minutes and at most 20 current plans remain.
- Plan validity requires both current policy hash and current discovery generation.
- Reviewed execution may remove targets after safety checks and may never add targets.
- Exit `1` outranks `2`; upgrade preview accepts `0` and `2` and rejects `1`.
- `service stop` must remain stopped across login/reboot.
- `service install` must clear macOS’s persistent disabled state before bootstrap.
- Linux lingering is documented but never enabled automatically.
- Binary installation alone never creates or starts a service.

---

### Task 1: Persisted Review-plan Schema and Pruning

**Files:**
- Modify: `src/store.rs`
- Test: `tests/store.rs`

**Interfaces:**
- Consumes: current `ScopePolicy::hash`, `DiscoveryGeneration::id`, and complete `ProjectReview` records.
- Produces: schema version 10, `Store::create_review_plan`, `Store::load_review_plan`, and `Store::prune_review_plans`.

- [ ] **Step 1: Add failing schema and plan tests**

Cover plan creation, exact ordered targets, policy mismatch, generation mismatch, 30-minute expiry, pruning beyond 20, pruning on store open, and preservation of unrelated run/clean history.

- [ ] **Step 2: Run the focused tests**

```bash
cargo test --locked --test store review_plan
```

Expected: compilation fails because review-plan APIs do not exist.

- [ ] **Step 3: Add schema version 10**

Use:

```sql
CREATE TABLE review_plans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    policy_hash TEXT NOT NULL,
    generation_id INTEGER NOT NULL REFERENCES discovery_generations(id),
    coverage_incomplete INTEGER NOT NULL CHECK(coverage_incomplete IN (0, 1)),
    candidate_bytes INTEGER NOT NULL
);

CREATE TABLE review_plan_targets (
    plan_id INTEGER NOT NULL REFERENCES review_plans(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    project_path TEXT NOT NULL,
    target_path TEXT NOT NULL,
    project_device INTEGER NOT NULL,
    project_inode INTEGER NOT NULL,
    target_device INTEGER NOT NULL,
    target_inode INTEGER NOT NULL,
    reviewed_bytes INTEGER NOT NULL,
    decision TEXT NOT NULL,
    skip_reason TEXT,
    PRIMARY KEY(plan_id, ordinal)
);

CREATE INDEX idx_review_plans_expires ON review_plans(expires_at);
INSERT INTO schema_version(version) VALUES (10);
```

- [ ] **Step 4: Implement typed plan APIs**

Add:

```rust
pub const REVIEW_PLAN_TTL: Duration = Duration::from_secs(30 * 60);
pub const REVIEW_PLAN_RETENTION: usize = 20;

pub struct ReviewPlan {
    pub id: i64,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
    pub policy_hash: String,
    pub generation_id: i64,
    pub coverage_incomplete: bool,
    pub candidate_bytes: i64,
    pub targets: Vec<ReviewPlanTarget>,
}
```

Creation and target inserts are one transaction. `load_review_plan` returns a typed `PlanLoadError::{Missing, Expired, PolicyMismatch, GenerationMismatch}`. Prune expired/mismatched plans on store open and every creation, then retain the newest 20.

- [ ] **Step 5: Run store tests**

```bash
cargo test --locked --test store
```

Expected: all pass.

- [ ] **Step 6: Commit Task 1**

```bash
git add src/store.rs tests/store.rs
git commit -m "feat: persist bounded cleanup review plans"
```

### Task 2: Dry-run Plan Creation and Exact Reviewed Execution

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/daemon.rs`
- Modify: `src/safety.rs`
- Test: `tests/cli.rs`
- Test: `tests/cache_cleaner_daemon.rs`

**Interfaces:**
- Consumes: `Store::create_review_plan`, `Store::load_review_plan`, `revalidate_before_clean`, and `CommandOutcome`.
- Produces: `car-go-clean run --review <ID>`, plan IDs from every dry run, and one shared execution engine.

- [ ] **Step 1: Add failing CLI behavior tests**

Cover:

```rust
#[test]
fn dry_run_without_all_persists_and_prints_review_id() {}
#[test]
fn reviewed_run_executes_only_the_persisted_targets() {}
#[test]
fn newly_eligible_target_is_not_added_to_reviewed_run() {}
#[test]
fn newly_unsafe_target_is_removed_from_reviewed_run() {}
#[test]
fn superseded_generation_rejects_the_entire_plan() {}
#[test]
fn expired_or_policy_mismatched_plan_exits_one_without_cargo() {}
#[test]
fn reviewed_run_fails_cleanly_while_daemon_holds_lock() {}
#[test]
fn run_all_without_dry_run_is_a_cli_error() {}
```

- [ ] **Step 2: Run focused CLI tests**

```bash
cargo test --locked --test cli review
```

Expected: failure because `--review` does not exist and dry runs do not persist plans.

- [ ] **Step 3: Extend the parser with unambiguous options**

Change `Commands::Run`/`RunOptions`:

```rust
#[arg(long, value_name = "ID", conflicts_with_all = [
    "dry_run", "no_scan", "include_managed_cache", "include_active", "force", "all"
])]
review: Option<i64>,

#[arg(long)]
json: bool,
```

Keep `--all` allowed only with `--dry-run`; Clap parse errors remain exit `1`.

- [ ] **Step 4: Persist every dry-run**

After fresh discovery and review, persist all decisions and identities, then print:

```text
Review ID: 42
Policy hash: 7d8db91f70fd49f91f5812f213e2b132a80976115a4d4d106ebe45d2405df79a
Discovery generation: 17
Created: 2026-07-29T18:00:00Z
Expires: 2026-07-29T18:30:00Z
Candidate bytes: 1048576
```

`--all` controls only target-list truncation.

- [ ] **Step 5: Execute exact plans through one engine**

Extract:

```rust
fn execute_reviews<R: CommandRunner>(
    store: &Store,
    policy: &ScopePolicy,
    reviews: Vec<ProjectReview>,
    cleaner: &Cleaner<R>,
    source: RunSource,
) -> Result<RunCycleResult>;
```

Dynamic `run` supplies current authorized reviews. `run --review` loads the plan, requires matching policy/generation, converts only persisted cleanable targets to reviews, and calls the same engine without discovery. Revalidation may skip but never append.

- [ ] **Step 6: Print each actual Cargo target before invocation**

Text:

```text
Cleaning /canonical/project/target (project /canonical/project)
```

JSON emits a target event before the summary. Complete independent safe targets even after one Cargo failure; merge outcome severity at the end.

- [ ] **Step 7: Run CLI and daemon integration tests**

```bash
cargo test --locked --test cli
cargo test --locked --test cache_cleaner_daemon
```

Expected: all pass.

- [ ] **Step 8: Commit Task 2**

```bash
git add src/cli.rs src/daemon.rs src/safety.rs tests/cli.rs tests/cache_cleaner_daemon.rs
git commit -m "feat: execute persisted cleanup reviews"
```

### Task 3: Stable JSON and Exit-reason Contract

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/outcome.rs`
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: scan origins, policy diagnostics, review plans, project decisions, and `CommandOutcome`.
- Produces: a versioned JSON envelope and explicit outcome reasons.

- [ ] **Step 1: Add failing JSON snapshot tests**

Test complete (`0`), incomplete (`2`), failed (`1`), and combined scan/Cargo failure (`1`) cases. Parse JSON structurally; do not compare presentation whitespace.

- [ ] **Step 2: Add a versioned response envelope**

Use:

```rust
#[derive(Serialize)]
struct CommandReport<T> {
    format_version: u32,
    command: &'static str,
    outcome: OutcomeReport,
    policy_hash: Option<String>,
    generation: Option<i64>,
    review_id: Option<i64>,
    scan_errors: Vec<ScanErrorReport>,
    data: T,
}

#[derive(Serialize)]
struct OutcomeReport {
    code: u8,
    kind: &'static str,
    reasons: Vec<String>,
}
```

`format_version` begins at `1`. Text and JSON derive from the same result object; do not rebuild outcome logic in the presenter.

- [ ] **Step 3: Preserve severity ordering**

Keep:

```rust
Failed > Incomplete > Complete
```

Safety skips do not create incomplete coverage. Scan/origin/generation incompleteness does.

- [ ] **Step 4: Run all CLI tests**

```bash
cargo test --locked --test cli
```

Expected: all pass.

- [ ] **Step 5: Commit Task 3**

```bash
git add src/cli.rs src/outcome.rs tests/cli.rs
git commit -m "feat: expose stable command outcome reports"
```

### Task 4: Persistent Service State and Captured Environment

**Files:**
- Modify: `src/service.rs`
- Modify: `src/cli.rs`
- Modify: `packaging/launchd/com.dcchuck.car-go-clean.plist`
- Modify: `packaging/systemd/car-go-clean.service`
- Test: `tests/service.rs`
- Test: `tests/packaging.rs`

**Interfaces:**
- Consumes: `protected_roots_for`, supported manager/container environment variables, and platform command runners.
- Produces: `ServiceStatus { installed, enabled, active }`, persistently correct lifecycle verbs, and rendered environment snapshots.

- [ ] **Step 1: Add failing command-sequence tests**

Assert exact order:

- macOS install after disabled uninstall: write definition, `launchctl enable gui/<uid>/<label>`, `bootstrap`, then `kickstart -k`;
- macOS stop: `launchctl disable gui/<uid>/com.dcchuck.car-go-clean` before tolerant `bootout`;
- macOS start: `enable` before `bootstrap`/`kickstart`;
- Linux install/start: `systemctl --user daemon-reload`, then `enable --now`;
- Linux stop: `disable --now`;
- restart requires installed and enabled;
- uninstall stops/disables, removes only the definition, reloads the manager, and retains config/state.

- [ ] **Step 2: Run focused service tests**

```bash
cargo test --locked --test service
```

Expected: failures because status has no persistent enablement and stop is transient.

- [ ] **Step 3: Model three independent states**

Change:

```rust
pub struct ServiceStatus {
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
}
```

On macOS, query both `launchctl print-disabled gui/<uid>` and `launchctl print gui/<uid>/<label>`. On Linux, query `is-enabled` and `is-active`. Missing definitions are not errors; malformed manager output is.

- [ ] **Step 4: Implement persistent lifecycle semantics**

Make install/start/stop/restart/uninstall exactly match the approved contract. Treat missing bootout/unload as idempotent only after status establishes the intended state.

- [ ] **Step 5: Capture supported environment overrides**

Add:

```rust
pub struct ServiceEnvironment {
    pub values: BTreeMap<String, OsString>,
}

impl ServiceEnvironment {
    pub fn capture(environment: &dyn Environment) -> Self;
}
```

Capture only the supported root variables used by policy construction. Render them into a launchd `<key>EnvironmentVariables</key><dict>` containing one key/string pair per captured variable, with XML escaping, and systemd `Environment=` lines with systemd quoting. Never capture arbitrary environment or secrets.

- [ ] **Step 6: Expose provenance and divergence**

`service status`, `status`, and `health` print installed/enabled/running separately. Parse the installed service definition’s captured roots and warn when the current shell’s resolved roots differ, naming `car-go-clean service install` as the recapture command.

- [ ] **Step 7: Run service and packaging suites**

```bash
cargo test --locked --test service
cargo test --locked --test packaging
cargo test --locked --test cli service
```

Expected: all pass.

- [ ] **Step 8: Commit Task 4**

```bash
git add src/service.rs src/cli.rs packaging/launchd/com.dcchuck.car-go-clean.plist packaging/systemd/car-go-clean.service tests/service.rs tests/packaging.rs
git commit -m "feat: persist service enablement safely"
```

### Task 5: v0.2/v0.3 Upgrade Helper

**Files:**
- Create: `packaging/release/car-go-clean-upgrade.sh`
- Modify: `.github/workflows/publish-shell-installer.yml`
- Modify: `tests/installer.sh`
- Create: `tests/upgrade.sh`
- Modify: `Makefile`
- Test: `tests/packaging.rs`

**Interfaces:**
- Consumes: old `service status`, platform-native service commands, Homebrew, new `car-go-clean config`, dry-run review, and exit taxonomy.
- Produces: a released upgrade helper that preserves active/stopped/absent service state.

- [ ] **Step 1: Add failing shell fixtures**

Create `tests/upgrade.sh` with command shims that emulate real v0.2/v0.3 output and assert:

- active old service is stopped natively and restored if Homebrew fails before replacement;
- active old service upgrades, accepts preview `0` or `2`, then starts only after explicit execution approval;
- preview `1` leaves the new service stopped and prints rollback/start guidance;
- stopped stays stopped;
- absent stays absent;
- legacy `excludes` warns and prints `config migrate`;
- the helper requires exact `0.4.0`.

- [ ] **Step 2: Add the failing test entry point**

Update:

```make
.PHONY: test-upgrade
test: test-installer test-upgrade test-release-notes
test-upgrade:
	sh tests/upgrade.sh
```

Run `make test-upgrade`; expected failure because the helper is missing.

- [ ] **Step 3: Implement a fail-closed POSIX helper**

The helper accepts:

```text
--version 0.4.0
--method homebrew|shell
--execute-review ID
```

It records old state before replacement, uses native launchctl/systemctl commands compatible with old binaries, registers a trap only until replacement succeeds, validates exact version, runs `config`, generates a dry-run review, accepts status `0` or `2`, and requires a supplied review ID before cleanup. It never enables a previously stopped/absent service.

- [ ] **Step 4: Publish the helper with release assets**

Add it to the shell-installer publisher and release manifest. Verify checksum/attestation inventory includes the helper.

- [ ] **Step 5: Run shell and packaging tests**

```bash
make test-installer
make test-upgrade
cargo test --locked --test packaging
```

Expected: all pass.

- [ ] **Step 6: Commit Task 5**

```bash
git add packaging/release/car-go-clean-upgrade.sh .github/workflows/publish-shell-installer.yml tests/installer.sh tests/upgrade.sh tests/packaging.rs Makefile
git commit -m "feat: add state-preserving v0.4 upgrades"
```

### Task 6: Operator Documentation and Executable Examples

**Files:**
- Modify: `README.md`
- Modify: `docs/configuration.md`
- Modify: `docs/fresh-install-validation.md`
- Modify: `docs/releases/v0.4.0.md`
- Modify: `docs/releasing.md`
- Modify: `tests/packaging.rs`
- Modify: `tests/release-notes.sh`

**Interfaces:**
- Consumes: exact CLI/service/helper behavior from Tasks 1–5.
- Produces: human and Agent Quick Start guidance that runs against the real CLI.

- [ ] **Step 1: Add documentation contract tests**

Require docs to include:

- `run --dry-run` producing a review ID;
- `run --review <ID>`;
- dynamic bare `run`;
- exit `0`/`2`/`1` and ordinary macOS TCC incompleteness;
- managed-storage’s two gates;
- persistent install/start/stop semantics;
- uninstall retaining config/state;
- `loginctl enable-linger $USER`;
- v0.2/v0.3 active/stopped/absent upgrades and rollback;
- legacy `excludes` migration/removal timeline.

- [ ] **Step 2: Update all command examples**

Make the recommended destructive path:

```bash
car-go-clean run --dry-run --all
car-go-clean run --review REVIEW_ID
```

Explain that bare `run` remains dynamic and is appropriate only when the operator intentionally accepts a fresh target set.

- [ ] **Step 3: Add executable Agent Quick Start checks**

Extract shell blocks or maintain explicit command fixtures so `tests/packaging.rs` runs `--help` and validates every documented flag/subcommand.

- [ ] **Step 4: Run docs and packaging checks**

```bash
cargo test --locked --test packaging
make test-release-notes
```

Expected: all pass.

- [ ] **Step 5: Commit Task 6**

```bash
git add README.md docs/configuration.md docs/fresh-install-validation.md docs/releases/v0.4.0.md docs/releasing.md tests/packaging.rs tests/release-notes.sh
git commit -m "docs: add reviewed cleanup and upgrade guidance"
```

### Task 7: Operator-control Full Verification

**Files:**
- Modify only files required by failures attributable to Tasks 1–6.

**Interfaces:**
- Consumes: all operator-control tasks.
- Produces: a clean, independently reviewable operator-control commit series.

- [ ] **Step 1: Run formatting and Clippy**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
```

- [ ] **Step 2: Run every local gate**

```bash
make test
```

- [ ] **Step 3: Run command-state matrices**

Run the service fake-runner matrix and upgrade helper matrix for macOS/Linux × installed/enabled/active combinations and v0.2/v0.3 × active/stopped/absent.

- [ ] **Step 4: Inspect the exact diff**

```bash
git diff --check
git status --short
git log --oneline --decorate -7
```

Expected: no whitespace errors, no uncommitted changes, and one commit per task.
