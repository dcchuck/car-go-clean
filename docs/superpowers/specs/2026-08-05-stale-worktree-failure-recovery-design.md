# Stale Worktree Discovery Failure Recovery Design

## Problem

`blocked_worktree_discovery_paths` treats a missing path stored in
`worktree_discovery_failures` as an identity-validation failure. Its fallback
adds every cached project to the blocked set. A transient Git worktree failure
under a deleted container filesystem can therefore turn unrelated, valid Cargo
targets into `scan_error` skips forever. The cleanup daemon then completes a
run successfully while reclaiming zero bytes.

## Design

Before calculating discovery blocks for a review, reconcile only cached
worktree-failure and linked-worktree provenance against the current exclusion
policy. Remove a failure record and its linked-worktree provenance only when
that policy excludes the affected durable identity. This includes deleted paths
beneath an active protected root such as OrbStack. Preserve ordinary cached
project history unchanged.

A current failure whose primary path still exists keeps the existing
fail-closed behavior. A missing primary with a live linked worktree also stays
blocked. Records without a canonical primary path keep the existing global
fail-closed behavior. Thus the repair only self-heals excluded stale state and
does not release a live linked worktree based on disappearance alone.

The repair runs from the normal block calculation path, so upgrading to the
patched binary automatically fixes existing affected state before a cleanup
review is produced; no operator database surgery is required.

## Regression Coverage

Add a store regression test that records a trusted failed primary and linked
worktree, deletes that primary, and verifies that:

- the review reconciliation removes excluded stale blocks;
- the stale failure does not trigger the global all-project fail-closed path;
- linked-worktree provenance for the missing primary is removed; and
- a missing primary with a live linked worktree remains blocked.

Run the focused store test first, then the complete locked Rust test suite and
format/Clippy checks before publishing `0.4.1`.

## Release and Recovery

Release the patch as `0.4.1` using the repository's verified release process.
After the installed tool is confirmed as `0.4.1`, generate and inspect a fresh
review. Execute cleanup only for its validated build-target candidates under
the existing safety policy, and report the measured disk recovery.
