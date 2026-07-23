# Nested Git Worktree Discovery Design

## Goal

Make `car-go-clean` discover Rust projects that are linked Git worktrees, even
when those worktrees live beneath another Cargo project or in an ignored
worktree directory. New worktrees should be discovered by the daemon within
one day by default.

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
does today. If the project is a primary Git checkout, it additionally asks Git
for linked worktree paths. A linked path is added only when all of the
following are true:

- it is within one of the configured scan roots;
- it contains a direct `Cargo.toml`; and
- it is not excluded by the configured exclusion policy.

The scanner does not recursively walk arbitrary Cargo project subdirectories.
This preserves the existing one-project-per-workspace behavior and avoids
discovering every member crate as its own cleanup unit. A Git command failure
is reported as a scan error while preserving discovery of the primary project.

`review_project` and `cleaner` behavior remain unchanged. Every newly
discovered worktree still requires a direct, readable `target/`, must be
outside managed caches and container storage, must have no related scan error,
must have no active process, and must be quiet for at least
`target_quiet_period` before `cargo clean` may run.

The default `scan_interval` changes from seven days to one day. Config files
that explicitly set `scan_interval` retain their current value.

## Verification

Tests will create a primary Git repository with a linked Rust worktree in an
ignored nested worktree location. They will verify discovery of both project
roots, exclusion of linked worktrees outside configured scan roots, resilience
to Git discovery failures, and the new daily default. Existing safety tests
continue to establish that active, recently written, managed-cache, and
container targets are not cleaned by default.

## Non-Goals

- Discovering arbitrary nested Cargo workspace members.
- Following worktree paths outside configured scan roots.
- Relaxing activity, quiet-period, cache, container, or direct-target safety
  protections.
- Changing explicitly configured scan intervals.
