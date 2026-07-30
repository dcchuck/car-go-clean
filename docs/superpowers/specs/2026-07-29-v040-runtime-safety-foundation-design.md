# v0.4 Runtime Safety Foundation Design

## Context

The first v0.4 hardening pass made exclusions a cleanup-boundary check, but
independent release reviews found that cached project rows still carry cleanup
authority after discovery scope changes. They also found unsafe configuration
fallbacks, stale exclusion identities in a long-lived daemon, incomplete
managed-storage classification, and incorrect treatment of failed Cargo
commands.

This design supersedes the cached-state, storage-profile, configuration, and
clean-result portions of the earlier v0.4 hardening design. It intentionally
adds a database migration because path-only cache rows cannot represent
current cleanup authority safely.

## Goals

1. The current `scan_dirs` and `project_dirs` are a hard cleanup boundary for
   every manual and scheduled cleanup, including `--no-scan`.
2. A cached row is never sufficient authorization by itself.
3. Filesystem identity changes, ambiguous paths, scan failures, and exclusion
   failures block cleanup.
4. Default and environment-relocated manager/container roots share one
   discovery and cleanup policy.
5. Configuration mistakes fail with actionable errors instead of silently
   broadening, emptying, or relocating scope.
6. A failed `cargo clean` is recorded as a failure, is excluded from every
   recovery total, and makes a one-shot command exit `1`.
7. Existing run history, clean events, diagnostics, and recovery totals remain
   available across migration.
8. An upgrade from v0.2/v0.3 never leaves a working installation unable to
   load its own configuration.
9. The installed daemon and an interactive command derive the same effective
   policy, or the difference is visible.

## Non-goals

- Do not preserve cleanup authority for projects outside current effective
  scope.
- Do not make `--no-scan` bypass scope, exclusion, identity, activity,
  quiet-period, or managed-storage checks.
- Do not make a failed root scan authorize cleanup from that root.
- Do not clean direct `target/` directories that are symlinks, mount points,
  or on a different filesystem from their project.
- Do not change the release tag or publish v0.4.0.

## Effective Scope Policy

Each config load constructs one immutable `ScopePolicy` for the command or
daemon cycle. It contains:

- canonical scan roots;
- canonical explicit project paths;
- lexical and currently canonical exclusion identities;
- default and environment-derived protected-storage roots, each with its
  provenance;
- the effective config source path;
- a deterministic policy hash.

Every project considered for cleanup must be inside at least one current scan
root or exactly match a current explicit project. The comparison is performed
against canonical paths. An absent or non-canonicalizable configured root
produces a scope error; it never expands authority.

`--no-scan` skips discovery only. It may reuse the latest successful
observation for a project, but only when that project is still inside the
current `ScopePolicy` and every execution-time safety check passes.

### Policy Hash Inputs

The policy hash is not "cleanup-relevant values" by judgement. It is a hash
over an explicitly enumerated, ordered tuple, so that two builds and two
processes agree byte for byte:

1. `policy_hash_format_version` (an integer constant bumped whenever this list
   changes, so hashes from different binaries can never collide);
2. canonical scan roots, sorted;
3. canonical explicit project paths, sorted;
4. lexical exclusion patterns, sorted;
5. canonical identities for absolute exclusions that resolved, sorted;
6. resolved protected roots with their kinds, sorted;
7. `target_quiet_period`;
8. `scan_interval` (it defines the scan-error window that gates cleanup);
9. the effective config source path.

Deliberately excluded: `clean_interval` and `log_level`. They change schedule
and verbosity, never cleanup authority, and including them would invalidate
review plans for no safety benefit.

A changed policy hash invalidates review plans and the current discovery
generation. `health` and `status` print the hash and the inputs that produced
it.

## Discovery Generations and Origins

Add persisted discovery generations and project observations. A generation
records:

- generation ID and timestamp;
- policy hash;
- boot session ID (see below);
- each configured origin and whether its enumeration completed successfully;
- each observed canonical project;
- the origin that authorized the observation;
- filesystem identity for the project and direct target;
- last successful observation time.

On Unix, filesystem identity uses device and inode from `symlink_metadata`.
Platform-specific identity is isolated behind a small internal interface so a
future non-Unix implementation can supply an equivalent.

### Identity Is a Change Detector, Not a Capability Token

`st_dev` is not stable across reboots or remounts on either macOS or Linux. A
persisted device number therefore cannot be treated as durable proof of
identity: after an ordinary reboot every stored device would mismatch, and a
naive "mismatch blocks cleanup" rule would make `--no-scan` and every cached
observation useless until the next scheduled scan.

Each generation records a boot session ID — `kern.boottime` on macOS,
`/proc/sys/kernel/random/boot_id` on Linux — resolved behind the same platform
interface as identity.

- Same boot session: persisted device/inode are authoritative. A mismatch means
  the path was replaced and blocks cleanup for that project.
- Known different boot session: device numbers are not comparable. The
  observation is stale rather than hostile. The project is re-stat'ed, and it
  may be re-authorized only if it still resolves inside the current
  `ScopePolicy`, is not excluded, and passes every execution-time check. The
  refreshed identity replaces the stored one.
- Unavailable boot ID: exact device/inode equality may continue, but a mismatch
  is a replacement and fails closed. Missing boot identity cannot authorize a
  freshly observed replacement inside the same execution.
- Within a single process, identity captured at review time is always
  authoritative for the pre-Cargo recheck, regardless of boot session. This is
  the check that actually defends against replacement during a run.

An inode mismatch always blocks for same-boot or unavailable-boot comparisons,
and for the process-local pre-Cargo recheck. A known different boot may
re-authorize the current identity only after the full restat and policy checks
above.

### Scan Scheduling for Migrated or Repolicied State

A generation authorizes cleanup only when its policy hash equals the current
one. Migration produces no generation at all, and a config edit invalidates the
existing one. Without an explicit rule, a daemon holding `next_scan_at` twenty
hours out would sit inert through every clean deadline in between.

When no generation matches the current policy hash, the daemon schedules a scan
at its next cycle rather than waiting for `next_scan_at`. The forced scan is
rate-limited through `scheduler_state` to at most one per five minutes so a
restart loop or a config that fails to produce observations cannot become a
scan loop. Existing scan-failure backoff still applies on top of that limit.

A scan is reconciled transactionally:

1. Insert the generation and origin results.
2. Mark observations from successfully completed origins current.
3. Revoke current authority for projects absent from a successfully completed
   origin.
4. Revoke authority for projects outside the current policy or matching an
   exclusion.
5. Preserve prior rows and diagnostic provenance for origins that failed, but
   mark their observations blocked rather than authorized.
6. Upsert newly observed projects and worktree relationships.

Historical project rows may remain for stats and diagnostics. Cleanup selects
only currently authorized observations.

The migration from v0.2/v0.3/v0.4 path-only state grants no implicit current
authority. The first successful scan creates observations. If cleanup becomes
due first, it reports cached rows as blocked and performs no Cargo operation.
A one-shot command in that state exits with the incomplete-coverage code
(`2`), not `0`, because "cleaned nothing" and "was not authorized to look" are
different outcomes for an operator or an agent.

## Execution-time Identity Boundary

Immediately before invoking Cargo, revalidate:

- the project path is a direct directory, not a symlink;
- its canonical path and device/inode match the identity captured during this
  process's review pass;
- `Cargo.toml` remains a direct regular file;
- `target/` is a direct directory, not a symlink;
- the target device/inode matches the reviewed observation;
- project and target are on the same device;
- current scope, exclusions, protected-storage classification, process
  activity, scan diagnostics, and quiet period still permit cleanup.

There is no separate mount-table lookup. A mount point differs in `st_dev`
from its parent by definition, so the project/target same-device comparison
already detects one, and it does so without parsing a platform-specific table
or opening a second race window. The stated non-goal of never cleaning a
target that is a mount point is preserved by the device comparison.

Any mismatch converts the target to a skipped/blocked result. It is not
automatically re-authorized under its new identity within this run.

### Residual TOCTOU

These checks narrow the race window; they do not close it. `cargo clean`
resolves `--target-dir` itself, after the last check this process performs, so
a sufficiently determined local attacker who can write to a parent directory
can still swap a component between the final `symlink_metadata` and Cargo's
own resolution. The design states this rather than implying the identity
boundary is airtight. What it does guarantee is that ordinary
symlink/mount/replacement mistakes — the failure modes that actually produce
data loss reports — are caught, and that a stale cached row can never reach
Cargo on its own.

### Activity Sampling Cost

"Refresh activity before every target" is correct for safety and unaffordable
as written: `SysinfoProcessInspector` enumerates the whole process table, so a
few hundred projects would mean a few hundred full enumerations per run.

The snapshot is instead refreshed on demand with a floor: a target consults
the cached snapshot, and a fresh sample is taken only when the snapshot is
older than an internal `ACTIVITY_MAX_AGE` (30s). A long cycle therefore costs
at most one enumeration per 30 seconds of wall time while still guaranteeing
that no target is cleaned on evidence older than that. Activity is never
sampled once for an entire cycle.

## Exclusion and Protected-storage Snapshots

Build the exclusion matcher from current lexical patterns and the canonical
identities of absolute exclusions for each scan and each review/cleanup cycle.
A long-lived daemon never reuses a canonical target captured only at startup.

Relative exclusions such as `.git` and `node_modules` are component/path
patterns, not filesystem identities. They remain lexical-only: policy
construction never passes them to the canonicalizer, anchors them to the
process working directory, or records a canonical identity for them. This
keeps policy hashes deterministic between an interactive command and a
service with a different working directory.

### Absent Absolute Exclusions Are Normal, Unreadable Absolute Exclusions Are Not

"Canonicalization uncertainty blocks cleanup" must not be implemented as
"any canonicalization failure blocks cleanup". The default exclusion set is
home-anchored and deliberately speculative: `~/.bun/install/cache`,
`~/go/pkg/mod`, `~/.colima`, `~/.lima`, `~/.local/share/containers`, and
`~/OrbStack` are absent on most machines. Treating their `NotFound` as
uncertainty would block every cycle on a stock install, and the tool would
never clean anything again.

The following error rule applies to absolute exclusion entries only:

- `NotFound` — the absolute exclusion cannot alias anything that exists. Keep
  the lexical pattern active, record nothing, do not block. This is the common
  case for defaults.
- Permission denied, symlink loop, I/O error, or any other failure — the
  absolute exclusion may be aliasing a real path that this cycle cannot see.
  Block cleanup for the cycle and report which entry failed and why.

This restores the distinction the superseded design made for cached paths
("missing paths are marked for eviction; any other canonicalization failure
aborts") and which was lost in the rewrite.

The shared protected-storage profile includes home defaults plus canonical
roots derived from:

- `CARGO_HOME`;
- `RUSTUP_HOME`;
- `XDG_CACHE_HOME`;
- `XDG_DATA_HOME` for supported rootless container layouts;
- `GOMODCACHE`;
- supported Bun cache/install root overrides;
- supported container data-root configuration that is directly discoverable
  from the process environment.

Structural recognition for Cargo registry/git and container storage remains a
fallback when a relocated path is encountered without a corresponding
environment variable.

Protected storage requires both deliberate discovery configuration and
`--include-managed-cache`. `--force` never supplies that authorization.

### The Daemon Does Not Share the Shell's Environment

Deriving protected roots "from the process environment" quietly means two
different answers. `packaging/launchd/com.dcchuck.car-go-clean.plist` declares
no `EnvironmentVariables`, and a systemd user unit is equally isolated, so a
`CARGO_HOME` or `GOMODCACHE` exported from a shell profile is visible to
`car-go-clean run` in a terminal and invisible to the installed daemon. The
daemon would compute a *weaker* protected set than the command the operator
used to verify it, and a different policy hash, silently.

Three parts fix this:

1. `service install` resolves the supported overrides and writes them into the
   rendered service definition — launchd `EnvironmentVariables`, systemd
   `Environment=`. The daemon therefore enforces the environment the operator
   installed it with, not an empty one.
2. `health` and `status` print every protected root with its provenance:
   `default`, `env:CARGO_HOME`, `service-definition`, or `structural`. A root
   that exists in one context and not the other is visible instead of
   inferred.
3. `health` warns when the current environment resolves protected roots that
   differ from those captured in the installed service definition, and names
   `service install` as the way to re-capture. Captured values are a snapshot
   by design; a later `export` does not reach the running daemon.

Because resolved protected roots are a policy-hash input, a divergence between
the two contexts surfaces as a policy mismatch — a refused review plan and a
visible error — rather than as weaker protection nobody notices.

## Strict Configuration

Deserialize into a raw overlay type whose fields are optional, then apply it
to `Config::default`.

Rules:

- unknown keys are rejected;
- missing environment variables are errors, in every expanded field —
  `scan_dirs`, `project_dirs`, and exclusion lists alike;
- both `${NAME}` and bare `$NAME` remain supported, matching v0.2/v0.3
  behavior; unterminated `${NAME` is an error;
- expanded `scan_dirs` and `project_dirs` must be absolute;
- relative exclusion entries remain lexical-only component/path patterns and
  are never canonicalized or working-directory anchored, while absolute
  exclusions must remain absolute after expansion and receive fresh canonical
  identity snapshots;
- empty effective scope (`scan_dirs` and `project_dirs` both empty) is invalid;
- `extra_excludes` appends to protected defaults;
- `override_excludes` deliberately replaces editable discovery exclusions;
- protected-storage cleanup classification remains independent of any
  override.

### Legacy `excludes` Is Deprecated, Not Rejected

Rejecting `excludes` outright breaks every existing installation. It is a
documented user-facing key (`README.md`, `Config::excludes`), so a v0.2/v0.3
user who upgrades would find every command failing on config load — including
each daemon cycle, which is exactly the state the upgrade flow is trying to
avoid leaving them in.

For the v0.4 line, `excludes` is accepted as a deprecated alias for
`override_excludes`:

- it loads, with a deprecation warning on stderr and a standing notice in
  `health` and `config`;
- specifying both `excludes` and `override_excludes` is a hard error;
- `car-go-clean config migrate` rewrites the file in place, preserving
  comments where the format permits and printing a diff first;
- removal is deferred to v0.5 and announced in the v0.4.0 notes.

This is safe under the new model precisely because it was not safe under the
old one: a legacy `excludes` list that omits `~/Library` no longer authorizes
cleaning `~/Library`, because protected-storage classification is now
independent of discovery exclusions. The alias inherits `override_excludes`
semantics, and its advanced/broad labeling.

### `config` Stays Machine-readable

`config` currently prints the effective configuration as valid TOML that can
be redirected into a config file. Printing the policy hash and resolved
protected roots into that stream would break the round trip outright, because
strict mode rejects unknown keys — the command's own output would no longer be
a legal input.

So: `config` prints configuration only, and remains round-trippable.
Diagnostics — config source path, effective canonical roots, policy hash,
protected roots with provenance — go to `health` and `status`, which are
already presentation surfaces, plus their JSON forms. A daemon validates the
policy before install/start guidance declares it usable.

## Cargo Failure Semantics

Every Cargo invocation produces an audit event containing exit code, bytes
before/after, duration, and a character-boundary-safe stderr excerpt.

If Cargo exits nonzero:

- record the clean event with its real nonzero `exit_code`;
- record a `clean` error with the stderr excerpt;
- do not increment `projects_cleaned`;
- do not update `last_cleaned_at`;
- increment the run error count;
- continue to other independently authorized targets;
- make a one-shot CLI command exit `1` after printing the full summary.

Partial byte recovery may be reported as observed bytes for that event, but it
is never described as a successful clean.

### Failed Events Must Not Enter Recovery Totals

Recording a failed event with its partial bytes is only safe if every
aggregate excludes it. `total_bytes_recovered` and the per-project top list
both sum `bytes_before - bytes_after` across `clean_events`, so a failed
`cargo clean` that deleted half a target would otherwise inflate lifetime
"bytes recovered" — the one number a user is most likely to quote.

Every recovery aggregate filters `exit_code = 0`: the all-time total, the
since-window total, the per-project ranking, and the per-run
`runs.bytes_recovered`. Failed events remain queryable for diagnostics and are
surfaced in `logs` and `stats` as failures, never as recovery. Existing
history tests are updated to assert this boundary rather than the current
unconditional sum.

### Exit Codes

One taxonomy, shared by every one-shot command and documented for agents:

- `0` — completed, coverage was complete. Safety skips alone still exit `0`.
- `2` — completed, but coverage was incomplete: scan errors, an origin that
  failed to enumerate, blocked cached rows, or a policy/generation mismatch
  under `--no-scan`. Results printed are partial but valid.
- `1` — failure: a nonzero Cargo exit, a config or policy error, a database
  error, or a lock conflict.

`1` outranks `2` when both occur. The operator-control design binds the
CLI-facing half of this contract, including how the upgrade helper reads it.

## Error Handling

- A root scan error is visible, blocks authority from that origin, and yields
  exit `2`.
- Database reconciliation is atomic.
- A configuration or policy error aborts before review with exit `1`.
- Identity or device uncertainty skips the affected project before Cargo.
- Missing projects and targets are normal stale-state skips.
- An absent exclusion path is not an error; an unreadable one blocks the cycle.
- A stale observation from an earlier boot session is re-verified, not treated
  as an attack.
- UTF-8 truncation always selects a valid character boundary. The current
  `stderr_excerpt` slices at a raw byte offset from the end and panics on
  multibyte input; that is a live defect, not a hypothetical.

## Testing

Behavioral tests cover:

- narrowing/removing scan roots after a cached discovery;
- explicit project removal;
- empty scope and partial config overlay;
- unknown keys and missing variables;
- migration from realistic v0.2/v0.3 path-only databases;
- broken and retargeted scan-root aliases;
- per-cycle exclusion retargeting;
- reused project path with changed device/inode;
- target symlink, mount/different-device, and identity replacement;
- relocated manager/container roots and required opt-in;
- activity beginning between two projects in one run;
- nonzero Cargo exits with no successful-clean accounting;
- partial deletion plus nonzero exit;
- long multibyte stderr;
- `--no-scan` under a changed policy.

Cases added by review, each pinned to a failure this design would otherwise
ship:

- a stock config whose speculative protected roots (`~/.colima`, `~/OrbStack`,
  `~/go/pkg/mod`, …) do not exist still cleans normally, while an exclusion
  that exists but cannot be read blocks the cycle;
- a daemon holding a distant `next_scan_at` with no matching generation scans
  at its next cycle, and a restart loop cannot exceed the forced-scan rate
  limit;
- a persisted observation from a different boot session is re-verified and
  re-authorized when still in scope, and blocked when it is not, while an
  inode change inside one generation always blocks;
- a config using legacy `excludes` loads with a deprecation warning, and
  `excludes` plus `override_excludes` together is a hard error;
- `config` output is fed back in as a config file and loads unchanged under
  strict validation;
- an unset variable in an exclusion entry is an error, and bare `$NAME`
  expands as it did in v0.3;
- a failed clean with partial byte deletion does not appear in the all-time
  total, the since-window total, the per-project ranking, or
  `runs.bytes_recovered`;
- exit codes: complete run `0`, scan-error run `2`, Cargo-failure run `1`,
  both `1`, migrated-state `--no-scan` `2`;
- protected roots resolved under a service-like empty environment match those
  captured in the service definition, and `health` reports the provenance of
  each;
- an activity sample is reused within `ACTIVITY_MAX_AGE` and refreshed beyond
  it, with a bounded enumeration count over a long cycle.

The implementation must keep existing history and statistics tests green,
except where the recovery-total boundary deliberately changes them.

## Implementation Slices

This document is a schema migration, a scope policy, an identity boundary,
strict configuration, and Cargo semantics. That is too much to review as one
change, and the last two are independent of the first three.

1. **Slice A — authority.** Scope policy and policy hash, discovery
   generations and observations, the identity boundary, exclusion and
   protected-storage snapshots, the migration and its scan scheduling.
2. **Slice B — configuration and results.** Strict configuration with the
   `excludes` deprecation, `config migrate`, Cargo failure semantics, recovery
   aggregates, exit codes, the stderr boundary fix.

Slice B is small, high-value, and unblocked; it can land first. Operator
control depends on Slice A for the policy hash and on Slice B for exit codes.

## Release Boundary

This foundation may be committed and pushed to `main` after its independent
review and full verification. It does not authorize a tag, release, local
Homebrew upgrade, or interaction with the real installed daemon.
