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
6. A failed `cargo clean` is recorded as a failure and makes a one-shot command
   exit nonzero.
7. Existing run history, clean events, diagnostics, and recovery totals remain
   available across migration.

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
- default and environment-derived protected-storage roots;
- the effective config source path;
- a deterministic policy hash over cleanup-relevant values.

Every project considered for cleanup must be inside at least one current scan
root or exactly match a current explicit project. The comparison is performed
against canonical paths. An absent or non-canonicalizable configured root
produces a scope error; it never expands authority.

`--no-scan` skips discovery only. It may reuse the latest successful
observation for a project, but only when that project is still inside the
current `ScopePolicy` and every execution-time safety check passes.

## Discovery Generations and Origins

Add persisted discovery generations and project observations. A generation
records:

- generation ID and timestamp;
- policy hash;
- each configured origin and whether its enumeration completed successfully;
- each observed canonical project;
- the origin that authorized the observation;
- stable filesystem identity for the project and direct target;
- last successful observation time.

On Unix, stable identity uses device and inode from `symlink_metadata`.
Platform-specific identity is isolated behind a small internal interface so a
future non-Unix implementation can supply an equivalent.

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

## Execution-time Identity Boundary

Immediately before invoking Cargo, revalidate:

- the project path is a direct directory, not a symlink;
- its canonical path and device/inode match the authorized observation;
- `Cargo.toml` remains a direct regular file;
- `target/` is a direct directory, not a symlink;
- the target device/inode matches the reviewed observation;
- project and target are on the same filesystem;
- the target is not a mount point according to the platform mount table;
- current scope, exclusions, protected-storage classification, process
  activity, scan diagnostics, and quiet period still permit cleanup.

Any mismatch converts the target to a skipped/blocked result. It is not
automatically re-authorized under its new identity. Activity is refreshed
before each target rather than sampled once for an entire long cycle.

## Exclusion and Protected-storage Snapshots

Build the exclusion matcher from current lexical and canonical identities for
each scan and each review/cleanup cycle. A long-lived daemon never reuses a
canonical target captured only at startup. Canonicalization uncertainty for a
configured exclusion blocks cleanup for that cycle.

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

## Strict Configuration

Deserialize into a raw overlay type whose fields are optional, then apply it
to `Config::default`.

Rules:

- unknown keys are rejected;
- missing environment variables are errors;
- malformed `${NAME}` syntax is an error;
- expanded `scan_dirs` and `project_dirs` must be absolute;
- relative exclusion entries remain component/path patterns, while absolute
  exclusions must remain absolute after expansion;
- empty effective scope (`scan_dirs` and `project_dirs` both empty) is invalid;
- `extra_excludes` appends to protected defaults;
- legacy `excludes` is rejected with a migration message;
- `override_excludes` deliberately replaces editable discovery exclusions;
- protected-storage cleanup classification remains independent of any
  override.

`health`, `config`, and `status` print the config source, effective canonical
roots, policy hash, and protected roots. A daemon validates the policy before
install/start guidance declares it usable.

## Cargo Failure Semantics

Every Cargo invocation produces an audit event containing exit code, bytes
before/after, duration, and a character-boundary-safe stderr excerpt.

If Cargo exits nonzero:

- record the clean event as failed;
- record a `clean` error with the stderr excerpt;
- do not increment `projects_cleaned`;
- do not update `last_cleaned_at`;
- increment the run error count;
- continue to other independently authorized targets;
- make a one-shot CLI command exit nonzero after printing the full summary.

Partial byte recovery may be reported as observed bytes, but it is never
described as a successful clean.

## Error Handling

- A root scan error is visible and blocks authority from that origin.
- Database reconciliation is atomic.
- A configuration or policy error aborts before review.
- Identity or mount uncertainty skips the affected project before Cargo.
- Missing projects and targets are normal stale-state skips.
- UTF-8 truncation always selects a valid character boundary.

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

The implementation must keep existing history and statistics tests green.

## Release Boundary

This foundation may be committed and pushed to `main` after its independent
review and full verification. It does not authorize a tag, release, local
Homebrew upgrade, or interaction with the real installed daemon.
