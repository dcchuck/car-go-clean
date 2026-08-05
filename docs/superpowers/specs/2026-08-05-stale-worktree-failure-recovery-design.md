# Stale Worktree Discovery Failure Recovery Design

## Problem

`blocked_worktree_discovery_paths` treats a missing path stored in
`worktree_discovery_failures` as an identity-validation failure. Its fallback
adds every cached project to the blocked set. A transient Git worktree failure
under a deleted container filesystem can therefore turn unrelated, valid Cargo
targets into `scan_error` skips forever. The cleanup daemon then completes a
run successfully while reclaiming zero bytes.

## Design

Before calculating discovery blocks, prune a failure record only when its
canonical primary path no longer exists on disk. Remove the corresponding
linked-worktree provenance in the same transaction.

A current failure whose primary path still exists keeps the existing
fail-closed behavior. Records without a canonical primary path also keep the
existing global fail-closed behavior. Thus the repair only self-heals stale,
previously trusted identities that no longer denote a filesystem object.

The repair runs from the normal block calculation path, so upgrading to the
patched binary automatically fixes existing affected state before a cleanup
review is produced; no operator database surgery is required.

## Regression Coverage

Add a store regression test that records a trusted failed primary and linked
worktree, deletes that primary, and verifies that:

- `blocked_worktree_discovery_paths` returns no stale blocks;
- the stale failure does not trigger the global all-project fail-closed path;
- linked-worktree provenance for the missing primary is removed; and
- a still-existing failure continues to block its primary and linked worktree.

Run the focused store test first, then the complete locked Rust test suite and
format/Clippy checks before publishing `0.4.1`.

## Release and Recovery

Release the patch as `0.4.1` using the repository's verified release process.
After the installed tool is confirmed as `0.4.1`, generate and inspect a fresh
review. Execute cleanup only for its validated build-target candidates under
the existing safety policy, and report the measured disk recovery.
