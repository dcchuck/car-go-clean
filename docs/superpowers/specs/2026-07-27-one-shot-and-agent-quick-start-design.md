# One-Shot Cleanup and Agent Quick Start

**Status:** Behavior approved; written specification awaiting user review

## Problem

The released `car-go-clean` CLI exposes discovery and cleanup as separate
operations. A fresh installation must run `car-go-clean scan` before
`car-go-clean run --dry-run` or `car-go-clean run`; otherwise the run reviews
only the existing cache and may find nothing. That separation is useful
inside the daemon scheduler but is surprising for a person who wants to
install the tool and clean once.

The README currently reflects the implementation rather than the user's
first-run goal. It explains installation, service activation, development,
configuration, safety, commands, and maintainer validation before providing a
short guided path for ordinary use. It also lacks a self-contained prompt a
person can paste into a coding agent to receive a safe, machine-aware
installation and dry-run walkthrough.

Version `v0.3.0` is already published and immutable. This behavior and
onboarding change will therefore target `v0.4.0`.

## Goals

- Make `car-go-clean run` useful immediately after a fresh installation.
- Make the normal one-shot path discover current projects before reviewing
  or cleaning them.
- Preserve an explicit cached-only mode for advanced and diagnostic use.
- Keep the daemon's independent scan and clean schedules unchanged.
- Put a concise human Quick Start immediately after installation.
- Provide one copyable Agent Quick Start prompt that installs or upgrades
  from canonical sources, inspects the machine, performs a health check and
  dry run, and asks before destructive or persistent changes.
- Retain every existing cleanup safety gate and process-lock guarantee.

## Non-Goals

- Starting, installing, or restarting the background service during binary
  installation.
- Combining the daemon's `scan_interval` and `clean_interval`.
- Adding an interactive confirmation prompt to `car-go-clean run`.
- Changing the default quiet period, exclusions, activity detection,
  managed-cache policy, scan-error policy, or worktree safety policy.
- Replacing `scan` as an explicit command.
- Cloning or building the repository during a normal binary installation.
- Rewriting or deleting the published `v0.3.0` release.

## Chosen Interface

`car-go-clean run` will scan before it reviews cleanup candidates:

```sh
car-go-clean run --dry-run --all
car-go-clean run
```

Both dry and real runs use the refreshed cache. An advanced escape hatch
retains the current cached-only behavior:

```sh
car-go-clean run --no-scan
```

`--no-scan` composes with the existing run options, including the
`--dry-run --all` preview. It means only "do not refresh discovery before
this run"; it does not relax any safety gate.

The explicit `car-go-clean scan` command remains available for users,
automation, and diagnostics that want discovery without a cleanup review.

## CLI Control Flow

A one-shot run follows this order:

1. Acquire the existing process lock.
2. Resolve paths and load the effective configuration.
3. Open the existing state database.
4. Unless `--no-scan` was supplied, execute the same scan cycle used by
   `car-go-clean scan`.
5. Persist successful discovery, worktree provenance, scan errors, and
   exclusion reconciliation through the existing scan path.
6. Print `Scan complete` after the scan cycle succeeds.
7. Perform the existing cleanup safety review against the refreshed cache.
8. In dry-run mode, display the existing preview and persist the review
   without invoking Cargo.
9. In real-run mode, clean only projects that pass the existing review and
   persist the existing run results and statistics.

The scan should be invoked through shared application logic rather than by
recursively parsing or launching another CLI command. Lock acquisition,
configuration loading, and database opening remain single operations for the
overall run.

When `--no-scan` is supplied, the flow skips steps 4 through 6 and otherwise
matches the current `run` behavior.

## Error and Safety Semantics

Ordinary traversal problems keep the existing fail-closed behavior:

- The scan records unreadable paths and other non-fatal discovery errors.
- Only projects physically related to an active scan error are blocked by the
  existing review policy.
- Unrelated projects may still be previewed or cleaned.
- Worktree-discovery failures continue to block their affected
  primary/worktree set.

The run must abort before cleanup if the scan cycle itself cannot complete or
persist its required state. Examples include failure to open or update the
state database or a fatal scan-cycle error. A run must never fall back to
cleaning stale cached entries after its requested pre-run scan fails.

All existing risk flags retain their current meanings. Auto-scan does not
imply `--force`, `--include-active`, or `--include-managed-cache`, and it does
not bypass target validation, quiet-period checks, activity detection,
managed-storage classification, scan-error checks, or process locking.

There is no new interactive prompt. `car-go-clean run --dry-run --all` is the
explicit review step, and a real `car-go-clean run` remains an intentional
cleanup command.

## Daemon Isolation and Upgrade Behavior

The daemon remains a scheduler with independent scan and clean cycles:

- Scheduled scans continue to use `scan_interval`.
- Scheduled clean cycles continue to use `clean_interval`.
- A scheduled clean does not gain an implicit scan.
- Installation or upgrade does not install, start, or restart the service.
- The existing process lock prevents a manual run from colliding with the
  daemon.

Installing the `v0.4.0` binary does not mutate configuration, cached projects,
statistics, or service state. A daemon already in memory continues running
its loaded binary until the user explicitly runs:

```sh
car-go-clean service restart
```

After restart, it uses the upgraded binary with the same scheduling behavior.
Only manual `car-go-clean run` invocations receive the new automatic scan.

## Command Help

The CLI help should make the important distinctions visible without requiring
the configuration reference:

- `run`: scan for projects, then run one cleanup review/cycle now.
- `--dry-run`: show what the refreshed review would clean without invoking
  Cargo.
- `--no-scan`: use cached discovery state instead of scanning first.
- `--all`: show all applicable dry-run detail according to its existing
  behavior.
- Existing risk flags: describe the specific safety gate they override and
  retain their current warning language.

The `daemon` and `scan` help text should continue to describe their independent
purposes so that auto-scan is not mistaken for a scheduler change.

## README Structure

The top-level README will prioritize the ordinary user journey:

1. Install
2. Quick Start
3. Agent Quick Start
4. Background Service (Optional)
5. Configuration
6. Safe Cleaning Model
7. Commands
8. Services and Packaging
9. Development

The existing long `Fresh Install Validation` section is maintainer-facing. It
will move to a focused document under `docs/`, linked from Development, rather
than remaining in the user quick-start path.

### Human Quick Start

The Quick Start will use:

```sh
car-go-clean health
car-go-clean run --dry-run --all

# After reviewing the preview:
car-go-clean run
car-go-clean stats
```

The surrounding text will state:

- `run` scans automatically before reviewing or cleaning.
- Installation does not start a daemon.
- `--dry-run --all` is the recommended preview.
- A real run has no interactive confirmation.
- The quiet period and all other default safety gates still apply.
- `--no-scan` exists for advanced cached-only use, not as the normal path.

### Agent Quick Start

The README will contain this self-contained copyable prompt:

> Install and configure the latest stable release of `car-go-clean` from its
> canonical repository:
>
> https://github.com/dcchuck/car-go-clean
>
> Before acting, read the current README and latest release. Use only these
> official installation sources:
>
> - Homebrew formula: `dcchuck/tap/car-go-clean`
> - Checksum-verifying installer:
>   `https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh`
>
> Do not use a similarly named package from another repository or registry.
>
> First inspect this machine's operating system, architecture, available
> package manager, Cargo availability, existing `car-go-clean` installation,
> configuration, and service status. Recommend Homebrew or the verified shell
> installer and briefly explain why.
>
> Install or upgrade the binary, verify the installed version, and run:
>
> ```sh
> car-go-clean health
> car-go-clean run --dry-run --all
> ```
>
> Explain what would be cleaned, what would be skipped, and why. Then
> recommend either one-shot usage or the background service based on how this
> machine is used.
>
> Inspection, installation or upgrade, health checks, and the dry run are
> authorized by this prompt. Ask before:
>
> - Performing actual cleanup.
> - Installing or enabling the background service.
> - Changing configuration or exclusions.
> - Using `--force`, `--include-active`, or `--include-managed-cache`.
> - Cloning and building from source.
>
> Do not weaken safety checks, manually delete `target/` directories, or work
> around scan errors or process locks. Report blockers and final results
> clearly.

The prompt intentionally authorizes reversible inspection, installation or
upgrade, health checks, and a dry run. It withholds authority for actual
cleanup, persistent service changes, configuration changes, risky overrides,
and source builds.

## Maintainer Validation Document

The current README validation commands will move to
`docs/fresh-install-validation.md`. They will be updated for auto-scan so that
the normal fresh-install path does not require an explicit `scan`. The
document may retain an explicit `scan` check when validating that command
itself, but it must separately prove that a fresh `run --dry-run` discovers
projects without it.

The document will distinguish source-checkout validation from released-binary
validation and avoid hard-coding an obsolete release version in user-facing
installer examples.

## Testing

Automated tests will cover:

1. A fresh state database plus `run --dry-run` discovers a fixture project
   without a prior explicit scan.
2. A fresh state database plus a real `run` discovers and cleans an eligible
   fixture project.
3. `run --no-scan` does not discover a project absent from cached state.
4. `run --dry-run --no-scan` previews only cached state and does not invoke
   discovery.
5. A fatal scan or scan-persistence failure aborts before any Cargo clean
   invocation.
6. Non-fatal unreadable-path errors block only related projects while
   unrelated eligible projects remain reviewable.
7. Auto-scan emits `Scan complete` before the run summary; `--no-scan` does
   not.
8. CLI help documents the default scan and cached-only escape hatch.
9. Daemon scheduler tests continue to prove independent scan and clean
   intervals, including that a scheduled clean does not trigger a scan.
10. README/documentation checks verify the canonical repository, official
    Homebrew formula, verified installer URL, human Quick Start, agent consent
    boundaries, and the absence of the maintainer validation block from the
    top-level README.

Existing CLI, scanner, state, safety, worktree, service, and packaging tests
must continue to pass.

## Release

This is a minor release because it deliberately changes the default behavior
of an existing command. The implementation will bump the package to `v0.4.0`
only after the code and documentation are complete and verified.

The published `v0.3.0` tag and release remain untouched. The `v0.4.0` release
will use the existing release workflow and produce the existing macOS/Linux
archives, checksums, installer, formula, manifest, and attestations. Its
Homebrew formula update will go through the public
`dcchuck/homebrew-tap` repository. If an older formula pull request is still
open at release time, it will be evaluated explicitly rather than silently
overwritten.
