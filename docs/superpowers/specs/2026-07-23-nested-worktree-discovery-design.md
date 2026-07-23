# Nested Git Worktree Discovery Design

## Goal

Make `car-go-clean` discover Rust projects that are linked Git worktrees, even
when those worktrees live beneath another Cargo project or in an ignored
worktree directory. Discovery must not weaken scan-root, cache, container, or
activity protections. New worktrees should be discovered by the daemon within
one day by default, including after an upgrade from the previous seven-day
default.

## Current Behavior

The scanner records a directory containing `Cargo.toml` as a project and then
stops walking below it. This prevents it from finding linked worktrees nested
under that project, such as `contextone-session-router/.worktrees/AI-5405`.
The default scan interval is seven days, so separately located worktrees can
also remain absent from the cached inventory for nearly a week.

## Approaches Considered

1. Recursively scan every directory below every Cargo project. This would find
   nested worktrees, but would also treat Cargo workspace members as separate
   cleanable projects and duplicate safety reviews.
2. Ask Git for linked worktrees from a discovered primary checkout. This is
   the selected approach: Git supplies authoritative worktree paths without
   scanning arbitrary nested Cargo manifests.
3. Only scan directories named `.worktrees` or `worktrees`. This is simpler,
   but misses valid layouts such as `ai-5433-worktrees`.

## Design

When the scanner finds a Cargo project, it continues to add that project as it
does today. Explicit `project_dirs` that contain `Cargo.toml` follow this same
path. A primary checkout is a project whose direct `.git` entry is a directory;
a linked worktree has a `.git` file and does not enumerate sibling worktrees.

For a primary checkout, a small injectable Git-worktree resolver invokes Git
without a shell using `git -C <primary> worktree list --porcelain -z`. It parses
only NUL-delimited `worktree <path>` records, ignores the primary checkout's
own record, and returns either a deterministic list of paths or a discovery
error. This boundary makes command failure and malformed-output behavior
testable without depending on ambient `PATH` or a real Git repository.

The scanner canonicalizes every existing configured scan root and every Git
candidate before evaluating it. A linked path is added only when all of the
following are true:

- its canonical physical path is within a canonical configured scan root;
- its canonical path contains a direct `Cargo.toml`; and
- its canonical path is not excluded by the configured exclusion policy.

The canonical path is the path saved for later classification, activity checks,
and cleaning. This prevents a Git-reported symlink path from bypassing cache,
container, or activity safeguards. Git-reported worktrees intentionally bypass
`.gitignore` traversal rules, because Git is authoritative for those paths,
but they still honor configured excludes and the scanner's built-in skip rules.
Duplicate canonical paths are deduplicated.

The scanner does not recursively walk arbitrary Cargo project subdirectories.
This preserves the existing one-project-per-workspace behavior and avoids
discovering every member crate as its own cleanup unit. A non-Git Cargo project
does not produce a Git-discovery error. A Git discovery failure is reported as
a scan error while preserving discovery of the primary project.

The cache persists the association between a primary checkout and every linked
worktree it discovered. A successful enumeration replaces that association. If
enumeration fails, the primary checkout and every linked worktree previously
associated with it remain ineligible for cleaning until a successful
enumeration replaces the failed result. Discovery errors are retained for that
association until that successful replacement scan; they do not expire merely
because the new scan interval has elapsed.

The existing `review_project` and `cleaner` policy gates remain unchanged;
only scan-error provenance is extended to cover associated worktrees. Every
newly discovered worktree still requires a direct, readable `target/`, must be
outside managed caches and container storage, must have no related scan error,
must have no active process, and must be quiet for at least
`target_quiet_period` before `cargo clean` may run.

The default `scan_interval` changes from seven days to one day in both the
configuration and daemon option defaults. Config files that explicitly set
`scan_interval` retain their current value. On daemon startup, a persisted
`next_scan_at` that is later than `now + effective_scan_interval` is clamped to
that bound, so a daemon upgraded from the old schedule cannot wait almost a
week before its next scan.

## Verification

Tests will use an injectable Git-worktree resolver and cover:

- discovery of an ignored nested linked Rust worktree and deduplication of its
  canonical path;
- rejection of out-of-scope, symlinked, excluded, stale, and manifest-less
  Git-reported paths;
- non-Git Cargo projects, primary-checkout detection, malformed resolver
  output, and resolver failures;
- blocking previously discovered linked worktrees after a resolver failure,
  until a successful replacement enumeration;
- canonical-path classification and activity behavior for a linked worktree;
- one-day configuration and daemon defaults, explicit configuration retention,
  and clamping of a persisted seven-day scheduler deadline; and
- README examples documenting the daily default and Git-worktree discovery.

Existing safety tests continue to establish that active, recently written,
managed-cache, and container targets are not cleaned by default.

## Non-Goals

- Discovering arbitrary nested Cargo workspace members.
- Following worktree paths outside configured scan roots.
- Following Git-reported paths whose canonical location is outside physical
  scan scope.
- Relaxing activity, quiet-period, cache, container, or direct-target safety
  protections.
- Changing explicitly configured scan intervals.
