# v14-to-v15 Combined-Failure Cardinality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reconcile v14 combined Cargo/measurement failures by exact audit
cardinality without collapsing same-signature events or rejecting valid
legacy run-count slack.

**Architecture:** Build exact Cargo-audit signatures for every nonzero v14
`measurement_failure`, group events by signature, and classify each group as
fully missing, complete, or ambiguous. Validate projected per-run independent
error lower bounds before applying fully missing repairs inside the existing
v14-to-v15 transaction.

**Tech Stack:** Rust, rusqlite/SQLite transactions, Cargo integration tests.

## Global Constraints

- Change only the version-gated v14-to-v15 migration behavior.
- Never use boolean audit existence to reconcile multiple events.
- Preserve exact `M == N` histories under the supported historical-writer
  invariant; treat the run lower bound as defense in depth, not row ownership.
- Preserve legitimate run errors without clean-event rows as slack above the
  lower bound.
- Fail partial, excess, missing-run, and visibly incomplete histories
  atomically and deterministically.
- Prevalidate checked projected counts for every repair-owning run so SQLite
  cannot promote an overflowing integer count to `REAL`.
- Preserve the existing single-event repair, preservation, rollback, and
  idempotency behavior.
- Do not push, tag, publish, install, mutate services, or start VMs.

---

### Task 1: Authentic collision fixtures and repair RED

**Files:**
- Modify: `tests/store.rs:3528-3885`

**Interfaces:**
- Consumes: public `Store::migrate`, `Store::record_clean_event`,
  `Store::record_error`, and `Store::finish_run`.
- Produces:
  `create_authentic_v14_combined_failure_runs(database, events_per_run)` and
  collision regression tests over real SQLite v14 state.

- [ ] **Step 1: Generalize the authentic v14 fixture**

Create a test helper with this interface:

```rust
fn create_authentic_v14_combined_failure_runs(
    database: &Path,
    events_per_run: &[usize],
) -> (Vec<i64>, SystemTime)
```

For every requested run, write the requested number of identical
same-second/path/exit/stderr combined failures through the current store, write
one measurement audit per event, finish the run with one error per event, and
then rebuild `clean_events` in its exact v14 shape with
`attempt_outcome='measurement_failure'`. Keep the single-event helper as a
thin wrapper over `events_per_run=&[1]`.

- [ ] **Step 2: Add same-run and cross-run missing-audit tests**

Add:

```rust
#[test]
fn version_fourteen_same_run_collision_repairs_every_missing_cargo_result()

#[test]
fn version_fourteen_cross_run_collision_repairs_each_owning_run()
```

The first uses `events_per_run=&[2]` and asserts two exact Cargo audits and
`errors_count=4`. The second uses `events_per_run=&[1, 1]` and asserts two
exact Cargo audits and `errors_count=2` on each run. Both call `migrate` twice
and assert the counts remain unchanged.

- [ ] **Step 3: Run the repair regressions and verify RED**

Run:

```bash
cargo test --locked --test store version_fourteen_same_run_collision -- --nocapture
cargo test --locked --test store version_fourteen_cross_run_collision -- --nocapture
```

Expected: both fail against the boolean-`EXISTS` implementation because it
creates only one exact Cargo audit and increments only the first owning run.

### Task 2: Complete, ambiguous, lower-bound, and legacy-slack fixtures

**Files:**
- Modify: `tests/store.rs:3528-4000`

**Interfaces:**
- Consumes:
  `create_authentic_v14_combined_failure_runs(database, events_per_run)`.
- Produces: exact behavior contracts for `M == N`, partial/excess cardinality,
  projected lower bounds, atomic retry, and legacy non-event slack.

- [ ] **Step 1: Add complete-cardinality preservation tests**

Add one table-driven test covering `events_per_run=&[2]` and
`events_per_run=&[1, 1]`. Insert two exact Cargo audits. Set the same-run count
to 4 or each cross-run count to 2. Migrate twice and assert audits, run counts,
event count, `cargo_nonzero`, and `measurement_failed=1` remain exact.

- [ ] **Step 2: Add a legacy non-event-error preservation test**

Create one combined event, insert its exact Cargo audit and one unrelated
`clean` error representing a legacy command failure without a clean event,
and set `errors_count=3`. Assert migration succeeds twice and preserves all
three audits/counts. This catches an incorrect equality check against the
event-derived lower bound of 2.

- [ ] **Step 3: Add partial/excess atomic-failure tests**

For two identical events, exercise one exact Cargo audit (`M=1,N=2`) and three
exact audits (`M=3,N=2`). Snapshot the v14 schema text, event rows, audit rows,
and run counts. Assert:

```rust
let first = store.migrate().unwrap_err().to_string();
let second = store.migrate().unwrap_err().to_string();
assert_eq!(second, first);
```

The message must name the timestamp/path plus event and audit counts. After
both attempts, assert schema version 14, the original v14 schema and rows,
original audit cardinality, original run counts, and no `clean_events_v15`.

- [ ] **Step 4: Add a visibly incomplete complete-audit test**

Create two same-run events and two exact Cargo audits but leave
`errors_count=3`, below the independent lower bound of 4. Assert the migration
fails twice with the same actionable lower-bound error and leaves every
database surface unchanged.

- [ ] **Step 5: Add a repair-count overflow test**

Create one missing-audit event with `errors_count=i64::MAX`. Assert migration
fails twice with the same projected-count overflow error and leaves the v14
event, audit set, run count and SQLite integer type, schema, and temporary-table
state unchanged.

- [ ] **Step 6: Run the ambiguity tests and verify RED**

Run:

```bash
cargo test --locked --test store version_fourteen_combined_failure_cardinality -- --nocapture
cargo test --locked --test store version_fourteen_complete_combined_failure -- --nocapture
```

Expected: partial/excess and incomplete-count tests fail because current code
migrates them; complete-cardinality and legacy-slack cases characterize state
that must remain supported.

### Task 3: Exact-cardinality transactional reconciliation

**Files:**
- Modify: `src/store.rs:1-10`
- Modify: `src/store.rs:2497-2520`
- Modify: `src/store.rs:3100-3170`
- Test: `tests/store.rs:3528-4100`

**Interfaces:**
- Consumes: v14 `clean_events`, `errors`, and `runs` inside a rusqlite
  `Transaction`.
- Produces: deterministic exact-cardinality validation and a vector of
  per-event repairs applied before the existing v15 table rebuild.

- [ ] **Step 1: Represent signatures and grouped events**

Import `BTreeMap` alongside `BTreeSet`. Add an ordered signature:

```rust
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LegacyCargoAuditSignature {
    timestamp: i64,
    path: String,
    message: String,
}
```

Make `LegacyCombinedCleanFailure` clonable and store its `run_id` plus
signature. Reconstruct the message exactly as current runtime and v7-to-v8
history do.

- [ ] **Step 2: Replace per-event `EXISTS` with cardinality classification**

Group failures in a
`BTreeMap<LegacyCargoAuditSignature, Vec<LegacyCombinedCleanFailure>>`.
For each group, query `COUNT(*)` using exact timestamp/category/path/message.

```rust
match audit_count.cmp(&event_count) {
    Ordering::Equal => {
        runs_requiring_validation.extend(
            failures.iter().map(|failure| failure.run_id),
        );
    }
    Ordering::Less if audit_count == 0 => {
        for failure in failures {
            *planned_repairs_by_run.entry(failure.run_id).or_default() += 1;
            planned_repairs.push(failure.clone());
        }
    }
    _ => bail!(
        "ambiguous combined clean failure audit at timestamp {} for {:?}: \
         found {} event(s) and {} exact cargo audit(s)",
        signature.timestamp,
        signature.path,
        event_count,
        audit_count,
    ),
}
```

Use signed `i64` counts from SQLite and report both counts. Do not use
`EXISTS`.

- [ ] **Step 3: Validate projected run lower bounds**

Accumulate planned repair counts per run. Load every run that will be repaired,
require an integer `errors_count`, and use checked addition to validate its
projected total before mutation. For every run participating in a complete
`M == N` group, also calculate this lower bound over every v14 event in the
run:

```sql
SUM(
    CASE
        WHEN attempt_outcome = 'success' THEN 0
        WHEN attempt_outcome IN ('cargo_nonzero', 'runner_failure') THEN 1
        WHEN attempt_outcome = 'measurement_failure' AND exit_code = 0 THEN 1
        WHEN attempt_outcome = 'measurement_failure' THEN 2
    END
)
```

Use checked addition for `projected = stored + planned`. Fail when the run is
missing or projected is below the lower bound. Permit any value above the
bound so legacy non-event failures remain intact.

- [ ] **Step 4: Apply all planned missing repairs**

After validation, insert one exact Cargo audit per planned event and increment
only that event's `run_id`. Require exactly one updated run. Leave the existing
v15 event-table rebuild and schema-version insert in the same transaction.

- [ ] **Step 5: Run the focused v14 migration tests and verify GREEN**

Run:

```bash
cargo test --locked --test store version_fourteen -- --nocapture
```

Expected: all v14 migration tests pass, including original single-event,
same-run, cross-run, complete-cardinality, partial/excess, lower-bound,
rollback, legacy-slack, and idempotency coverage.

### Task 4: Repository verification, audit report, and implementation commit

**Files:**
- Modify: `/private/tmp/car-go-clean-v040-state-authority-b-report.md`
- Verify: `src/store.rs`
- Verify: `tests/store.rs`
- Verify:
  `docs/superpowers/specs/2026-07-30-v14-v15-combined-failure-cardinality-design.md`
- Verify:
  `docs/superpowers/plans/2026-07-30-v14-v15-combined-failure-cardinality.md`

**Interfaces:**
- Consumes: complete round-three patch and test evidence.
- Produces: clean local implementation commit plus a round-three report
  section with exact SHA and gate results.

- [ ] **Step 1: Run focused and full Rust gates**

Run:

```bash
cargo fmt
cargo test --locked --test store
cargo test --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt -- --check
git diff --check
```

Expected: every command exits zero with no test failures or warnings.

- [ ] **Step 2: Run compatibility gates**

Run:

```bash
make test-upgrade test-release-scripts
```

Expected: exit zero. Rejection messages printed by negative fixtures are
expected.

- [ ] **Step 3: Review and commit the implementation**

Inspect `git diff`, stage only the plan, production, and test files, and
create:

```bash
git commit -m "fix: reconcile combined failure audit cardinality"
```

Confirm `git diff HEAD^ --check`, the exact SHA/parent, a clean worktree, and
the branch-ahead count. Do not push.

- [ ] **Step 4: Append round-three evidence**

Append the root cause, RED/GREEN evidence, historical-writer invariant,
cardinality cases, lower-bound/slack behavior, rollback/idempotency results,
all gate counts, exact commit SHA/subject/parent, and no-publication statement
to `/private/tmp/car-go-clean-v040-state-authority-b-report.md`.
