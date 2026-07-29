# v0.4.0 Release Hardening Design

## Context

The v0.4.0 release candidate adds platform-aware scan exclusions, automatic
pre-run scanning, one-shot onboarding, and safer scan-persistence behavior.
The release review found that the individual features work, but exclusions
are not yet a cleanup-boundary invariant for upgraded state:

- a symlink-spelled cached project can survive exclusion reconciliation and
  later canonicalize into an excluded root;
- a daemon with a nonempty v0.2/v0.3 cache can reach a persisted clean
  deadline before its first v0.4 scan;
- the scanner touches some excluded paths before checking exclusions;
- legacy explicit configuration can omit newly protected manager roots;
- the documented validation and release procedures have gaps.

Current Homebrew users may skip directly from v0.2.0 to v0.4.0. The design
therefore treats the full v0.2.0 state and behavior as the upgrade boundary,
even if the v0.3.0 formula is merged before v0.4.0 is released.

## Goals

1. No project excluded under its persisted or physical path can reach Cargo.
2. Managed cache and container storage remain protected independently of
   discovery configuration.
3. Excluded scan paths are rejected before filesystem access.
4. Existing v0.2/v0.3 state becomes safe before any review or cleanup without
   requiring a full scan.
5. Active-service users have a supported stop-preview-start workflow.
6. Fresh-install validation and release instructions execute as written.
7. Published v0.4.0 notes explain the operational upgrade delta.

## Non-goals

- Do not change the database schema or discard recovery history.
- Do not automatically merge Homebrew pull requests.
- Do not automatically install, upgrade, enable, or restart a daemon.
- Do not make managed storage cleanable merely because a custom `excludes`
  list omits it.
- Do not tag or release v0.4.0 as part of implementation.

## Chosen Approach

Use a cleanup-boundary safety invariant backed by one shared platform storage
profile.

Alternatives were rejected:

- Always scanning before cleanup is expensive, remains timing-dependent, and
  does not protect cached-only review.
- A one-time version migration only handles known state once and does not
  protect future aliases, configuration changes, or newly supported managers.

Discovery and cleanup authorization remain separate:

- `excludes` controls discovery and cache membership.
- The cleanup classifier independently protects managed and container storage.
- `--include-managed-cache` is required to authorize cleanup of a discovered
  managed location.

## Shared Platform Storage Profile

Introduce one internal profile that derives home-anchored protected roots for
the current platform. Both default exclusions and cleanup classification use
this profile, preventing the two policies from drifting.

Portable protected roots:

- `~/.cargo`
- `~/.rustup`
- `~/.cache`
- `~/.bun/install/cache`
- `~/go/pkg/mod`
- `~/.colima`
- `~/.lima`
- `~/.local/share/containers`

macOS protected roots:

- `~/Library`
- `~/.Trash`
- `~/OrbStack`

Linux protected roots:

- `~/.local/share/docker`
- `~/.docker/desktop`
- `~/.local/share/rancher-desktop`
- `~/.local/share/Trash`

Container and VM roots classify as `ContainerStorage`. Language/package
caches, `Library`, and Trash roots classify as `ManagedCache`. Existing exact
Cargo, Bun, Go, `Library/Caches`, and `OrbStack/docker` recognition remains
compatible, while the home-anchored profile covers the broader protected
roots.

The profile is created only from an absolute home directory. Matching checks
both the supplied path and its physical canonical path when available.
Failure to canonicalize never turns a protected or unreadable path into a
cleanable project.

## Cached-State Reconciliation

Before every project review or cleanup:

1. Load cached project paths.
2. Check the persisted spelling against the active exclusion matcher.
3. Canonicalize every existing cached path. Missing paths are marked for
   eviction; any other canonicalization failure aborts the review.
4. Check the canonical path against the matcher.
5. Transactionally remove excluded project rows and their excluded worktree
   provenance/failure state.
6. Perform the existing on-disk cache synchronization.
7. Classify and review the surviving canonical projects.

Filesystem canonicalization occurs before opening the deletion transaction;
the resulting deletion set is then applied atomically. Missing paths continue
through normal cache eviction. Other inspection failures abort review/cleanup
before Cargo, preserving fail-closed behavior.

This reconciliation runs for:

- scheduled daemon cleanup;
- automatic-scan manual cleanup;
- `run --no-scan`;
- dry-run review;
- project review/refresh paths.

It is not a full filesystem scan and does not discover new projects. Thus a
restarted daemon with a persisted clean deadline cannot clean stale newly
excluded state before its next scan.

Historical run totals, clean events, scheduler state, resolved diagnostic
history, and unrelated errors remain intact.

## Scanner Filesystem Boundary

Scanner ordering becomes:

1. Check the lexical path against exclusions.
2. If excluded, return without `metadata`, `canonicalize`, manifest probing,
   directory reads, or Git worktree resolution.
3. Canonicalize included paths when required.
4. Check the canonical path against exclusions.
5. Only then inspect metadata, `Cargo.toml`, directory contents, or Git state.

This ordering applies to scan roots, explicit project directories, recursive
walk entries, and discovered worktree candidates.

An excluded missing, unreadable, or protected path produces no finding,
resolver call, or scan error. An included path with a genuine access failure
continues to produce the existing scan diagnostic.

## Daemon Upgrade Behavior

The daemon does not need to force a full scan at every startup. Cleanup-boundary
reconciliation makes persisted schedules safe:

- if cleanup is due before scan, stale excluded rows are pruned before review;
- if scan is due first, normal scan reconciliation also prunes them;
- scan-persistence failure continues to defer cleanup and schedule a retry;
- successful and failed scan scheduling semantics otherwise remain unchanged.

This avoids startup scan storms while closing the v0.2/v0.3 upgrade window.

## Service Lifecycle UX

Add explicit cross-platform actions:

```text
car-go-clean service stop
car-go-clean service start
```

`stop` stops the user service but preserves its installed definition. It is
idempotent when the definition exists but is already inactive. `start` starts
an installed inactive service and is idempotent when already active. Starting
without an installed definition returns a clear error.

macOS uses `launchctl bootout` without removing the plist, followed by
`bootstrap` and `kickstart` for start. Linux uses
`systemctl --user stop/start`. Existing install, status, restart, and uninstall
behavior remains unchanged.

The documented active-service preview becomes:

```sh
car-go-clean service stop
car-go-clean run --dry-run --all
car-go-clean service start
```

Installation and Homebrew upgrade still modify only the binary. They never
enable or restart the daemon implicitly.

## Fresh-install Validation

The isolated validation configuration uses
`target_quiet_period = "1s"` and waits exactly two seconds after building the
fixture. It then performs dry-run and real one-shot checks without a preceding
explicit scan.

The recipe continues to use an isolated scan root and state directory and
must not affect the user's real service or state.

## Release Notes and Homebrew Completion

Add `docs/releases/v0.4.0.md` as the reviewed source of version-specific
release notes. It covers:

- automatic scan-before-run behavior;
- `--no-scan`;
- new platform storage exclusions and cleanup protection;
- the legacy explicit-configuration caveat;
- cached-state reconciliation;
- service stop/start/restart behavior;
- manual upgrade verification.

The release workflow derives the versioned notes path from the validated tag,
requires that file to exist, and prepends it to cargo-dist's generated
install/download/attestation body before creating the draft.

The runbook adds a mandatory post-publication phase:

1. Inspect the verified `formula/car-go-clean-v0.4.0` pull request.
2. Merge that formula pull request.
3. Close or supersede any older formula pull request deliberately.
4. Run `brew update`.
5. Install or upgrade `dcchuck/tap/car-go-clean`.
6. Verify `car-go-clean version` prints `0.4.0`.
7. For an active service, stop it, run the preview, and start it again.
8. Verify service status.

The workflow continues to avoid direct pushes to the tap's default branch and
does not auto-merge formula pull requests.

## Error Handling

- Excluded paths do not generate errors merely because they are inaccessible.
- Included inaccessible paths retain scan diagnostics.
- Cached reconciliation or classification uncertainty aborts cleanup before
  Cargo.
- Database deletion remains transactional.
- Service start/stop report platform command failures without deleting service
  definitions.
- Missing release notes fail the tag workflow before publication.
- Formula verification failure continues to leave the GitHub Release in draft
  state.

## Testing

### Storage and safety

- Profile-wide macOS and Linux default-exclusion assertions.
- Profile-wide cleanup classification assertions.
- Legacy explicit-config coverage proving protected roots remain skipped.
- Symlink-spelled cached project whose canonical path is excluded.
- End-to-end regression proving the excluded alias never invokes fake Cargo.

### Upgrade scheduling

- A populated v0.2-style cache with a clean deadline before its scan deadline.
- Library and OrbStack targets remain untouched.
- Excluded rows are pruned before the clean review.
- Recovery totals and unrelated state remain.

### Scanner

- Excluded inaccessible and missing roots produce no errors.
- Excluded explicit projects do not probe manifests.
- Excluded aliases are rejected after canonicalization.
- Worktree resolution remains uncalled for excluded candidates.

### Service lifecycle

- macOS and Linux start/stop command sequences.
- Idempotent active/inactive behavior.
- Missing-definition start failure.
- CLI help and visible status output.

### Documentation and release

- Fresh-install validation executes with a one-second quiet period and a
  two-second wait.
- v0.4.0 release notes contain required operational guidance.
- The release workflow composes reviewed notes with cargo-dist output.
- The runbook includes formula merge, Homebrew upgrade, version, preview, and
  service verification.
- Existing four-target, checksum, installer, and custom formula-publisher
  assertions remain.
- New tests assert executable behavior or parsed structure; they do not use
  brittle whole-paragraph prose matching.

### Final gates

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --locked -- -D warnings`
- `cargo test --locked`
- `make test-installer`
- `dist plan --tag v0.4.0 --output-format=json`
- built CLI help/version checks
- clean/synchronized `main`
- successful remote CI
- independent v0.4.0 release-gate review

## Release Boundary

Implementation may commit and push only after explicit user authorization.
It must not create `v0.4.0`, publish a GitHub Release, merge a Homebrew pull
request, upgrade the local Homebrew installation, or restart the running
daemon. Those operations occur only after the final release review is clean
and the user explicitly authorizes the release.
