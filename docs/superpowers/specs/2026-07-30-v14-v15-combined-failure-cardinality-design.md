# v14-to-v15 Combined-Failure Cardinality Design

## Scope

Schema v14 represented a cleanup whose Cargo process exited nonzero and whose
post-Cargo measurement also failed as one `measurement_failure` event. The
retained nonzero exit code lets schema v15 recover the independent execution
outcome, but an af419-era v14 writer emitted only the measurement audit and
counted only that reason. The v15 migration must recover the missing Cargo
audit and run count without duplicating complete histories from older writers.

This design changes only the version-gated v14-to-v15 transaction. It does not
change current runtime accounting or any earlier schema migration.

## Historical invariant

An exact Cargo audit signature is:

1. the clean-event timestamp;
2. category `clean`;
3. the clean-event path; and
4. `cargo clean exited <code>` plus `: <stderr>` when stderr is nonempty.

Supported historical writers and the v7-to-v8 migration emitted an exact Cargo
audit together with the owning run's Cargo-error count. The `errors` table has
no `run_id`, so individual audit rows cannot be assigned after the fact.
Migration decisions therefore use exact-signature cardinality:

- zero matching audits means every event in the group is missing its Cargo
  side effects;
- exactly as many audits as events means the supported historical writer
  invariant makes the group complete in aggregate;
- a nonzero partial count or an excess count is unassignable and must fail.

Run counts can include legitimate older command or runtime failures that have
no `clean_events` row. Those counts are preserved as slack and are never
recomputed.

## Reconciliation

The migration collects all v14 `measurement_failure` events with a nonzero
exit code, reconstructs their exact Cargo audit messages, and groups them by
signature.

For a group with `N` events and `M` exact audits:

- `M == 0`: plan one exact audit insertion and one `errors_count` increment
  against the event's owning run for each of the `N` events.
- `M == N`: preserve the audits and counters.
- `0 < M < N` or `M > N`: return an actionable cardinality error naming the
  timestamp, path, event count, and audit count.

For defense in depth, every run participating in an `M == N` group must meet a
projected independent-error lower bound after all planned `M == 0` repairs.
The lower bound sums the minimum reasons represented by every v14 clean event
in that run:

- `success`: zero;
- `cargo_nonzero`: one;
- `runner_failure`: one;
- `measurement_failure` with exit code zero: one;
- `measurement_failure` with a nonzero exit code: two.

The check is `projected errors_count >= lower bound`, not equality. Counts
above the lower bound preserve legitimate legacy failures without events. The
lower bound detects visibly incomplete run accounting but is not treated as
proof of individual audit-row ownership; aggregate ownership comes from the
supported historical writer invariant and exact signature cardinality.

The transaction validates cardinality and projected lower bounds before
mutation. It then inserts the planned audits, increments their owning runs,
rebuilds `clean_events` as v15, and records schema version 15. Any later error
still rolls back all of those effects.

## Failure and retry behavior

Partial, excess, missing-run, and lower-bound failures are deterministic and
actionable. They leave schema version 14, the v14 event schema and rows, all
audits, and all run counts unchanged. Retrying the same migration returns the
same error.

After a successful migration, schema version 15 prevents the repair from
running again. Repeated `migrate` calls therefore do not duplicate audits or
run counts.

## Test design

Authentic v14 fixtures exercise real SQLite state and the public migration:

- two same-signature events in one run with no Cargo audits repair to two
  audits and add two run errors;
- two same-signature events in different runs with no Cargo audits repair one
  audit and one run error per event and owning run;
- complete same-run and different-run collisions with `M == N` and sufficient
  run counts remain unchanged;
- partial (`0 < M < N`) and excess (`M > N`) groups fail twice with identical
  messages and no database mutation;
- a complete historical group whose run also contains a legitimate non-event
  failure remains openable and preserves that count as slack;
- the existing single-event repair, preservation, rollback, and idempotency
  cases remain green.
