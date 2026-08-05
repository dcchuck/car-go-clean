# Stale Worktree Discovery Failure Recovery Implementation Plan

**Goal:** Prevent missing, previously trusted worktree-failure paths from
globally blocking all cached Cargo projects; release the repair as `0.4.1` and
recover disk space through the normal safe review-and-clean flow.

**Architecture:** Reconcile only failure records whose canonical primary path
is absent before calculating block paths. Delete their dependent linked
worktree rows atomically. Preserve global fail-closed behavior for unresolved
identities and current failed worktrees.

## Task 1: Capture the failure mode with a regression test

**Files:** `tests/store.rs`

1. Add a test with a canonical primary and linked path, record a discovery
   failure, then remove the primary tree.
2. Add an unrelated cached project to make any global fallback observable.
3. Assert the block calculation removes the stale failure/provenance and does
   not return the unrelated project.
4. Run the focused test and confirm it fails against the current code because
   the missing path triggers the global fail-closed fallback.

## Task 2: Reconcile stale trusted failure records

**Files:** `src/store.rs`, `tests/store.rs`

1. Add a transactional store helper that identifies canonical failure paths
   missing from disk.
2. Delete only each stale failure and its linked-worktree provenance.
3. Call it before calculating the trusted and untrusted discovery block state.
4. Keep the present behavior for live paths and `NULL` canonical identities.
5. Run the focused regression and all store tests.

## Task 3: Publish and validate the patch

**Files:** `Cargo.toml`, `docs/releases/v0.4.1.md` (if used by current release
   conventions)

1. Bump the package version to `0.4.1` and add concise release notes.
2. Run `git diff --check`, formatting, Clippy, locked tests, and the release
   plan check.
3. Commit the implementation, push `main`, wait for exact-SHA CI and rehearsal,
   then create and publish the `v0.4.1` tag under the repository release gates.

## Task 4: Recover this Mac safely

1. Upgrade the installed formula to the verified `0.4.1` build.
2. Run a fresh review, confirm stale state self-healed, and inspect candidate
   paths/bytes.
3. Run the approved normal cleanup, then report recovered bytes and current
   free space.
