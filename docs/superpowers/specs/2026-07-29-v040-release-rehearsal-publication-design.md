# v0.4 Release Rehearsal and Publication Design

## Context

The existing tag workflow verifies raw archives and formula syntax but does
not install through the uploaded shell installer or Homebrew before
automatically publishing `latest`. Draft creation is not retry-safe, token
preflight checks only presence, and no fresh VM transcript exists.

This design depends on the runtime safety and operator-control work. It
supersedes the publication and fresh-install sections of the earlier v0.4
hardening design.

## Goals

1. Rehearse all release paths from an exact commit before creating a tag.
2. Verify real installer and formula behavior on macOS and Linux.
3. Make draft hosting and reruns idempotent and commit-bound.
4. Require human approval before public prerelease and stable/latest
   promotion.
5. Test public versioned URLs before a release becomes latest.
6. Prove tap-token capability before tagging.
7. Preserve sanitized evidence for the release decision.
8. Delete every Tart VM and report reclaimed disk space after acceptance.

## Non-goals

- Do not auto-merge the Homebrew tap pull request.
- Do not move a published tag after a failed rehearsal or release.
- Do not treat source-built unit tests as install validation.
- Do not alter the real installed binary or daemon on the development Mac.
- Do not publish v0.4.0 during implementation.

## Pre-tag Rehearsal Workflow

A manually dispatched workflow accepts an immutable commit SHA and proposed
version. It rejects a dirty/unreachable or version-mismatched commit.

It builds all four cargo-dist archives:

- `aarch64-apple-darwin`;
- `x86_64-apple-darwin`;
- `aarch64-unknown-linux-musl`;
- `x86_64-unknown-linux-musl`.

For each applicable runner it verifies:

- archive and checksum inventory;
- exact `car-go-clean version` output;
- artifact attestations;
- `health` with isolated config/state;
- the actual shell installer using the rehearsal artifacts through its
  supported download-base override;
- a locally rendered formula using rehearsal checksums;
- `brew install` and `brew test` where Homebrew is supported;
- fresh install does not create or start a service.

Actions are pinned to immutable commit SHAs. The cargo-dist installer is
version- and digest-pinned rather than executed from an unverified movable
script.

## Tap Capability Rehearsal

Before a release tag, a guarded job uses `HOMEBREW_TAP_TOKEN` to:

1. Read the tap default branch.
2. Push a uniquely named rehearsal branch containing an inert evidence file
   or formula-only no-op change.
3. Open a draft pull request.
4. Verify branch, PR, contents, and pull-request permissions worked.
5. Close the draft and delete the branch.

Cleanup runs even when an intermediate step fails. The workflow never prints
the token. Existing user branches and PRs are untouched.

This rehearsal writes to a public repository, so its side effects are public:

- the branch name is explicitly marked as a rehearsal artifact and carries the
  rehearsal run ID, so anyone reading the tap can tell what it was;
- a closed pull request remains permanently visible in the tap's history. That
  is accepted as the cost of proving token capability before tagging;
- before enabling this, confirm which of the tap's own workflows fire on
  `pull_request` from a branch in the same repository. A formula-only no-op
  can trigger a `brew test-bot` run that consumes minutes and reports a
  confusing failure on an inert change. Prefer the inert evidence file over a
  formula edit for exactly this reason, and if tap CI still fires, scope it to
  ignore the rehearsal branch prefix.

## Retry-safe Draft Workflow

An annotated stable tag starts hosting but does not imply immediate latest
publication.

Draft behavior:

- create the draft only when absent;
- when present, require draft state, matching tag, and exact target commit;
- update reviewed notes and upload artifacts with guarded replacement;
- reject an existing published release or commit mismatch;
- make formula branches formula-only and verify their base/head relationship;
- never push the tap default branch.

The workflow verifies exact versions, checksums, attestations, installer
behavior against authenticated draft assets, formula rendering, and release
notes before any public state.

## Publication Gates

Publication has two protected GitHub environment approvals:

1. Publish the verified draft as a prerelease, explicitly not latest.
2. After hosted smoke succeeds, promote it to stable/latest and open the tap
   PR.

Hosted smoke uses unauthenticated versioned URLs and verifies:

- shell installer on fresh macOS and Linux runners;
- exact binary version;
- checksums and attestations;
- Homebrew formula install/test against the public archives. The tap PR is
  still unmerged at this point, so the smoke renders the formula locally from
  the release artifacts and installs from that file. It does not install from
  the tap, which would test the previous version;
- no implicit service installation;
- formula URL/version/hash equality with release artifacts.

Failure leaves the release as a prerelease. The tag is never moved. A defect
in final-tag artifacts requires a corrected patch release.

The tap PR remains manual. Its CI requires a formula-only diff and repeats
install/test before merge.

## Hands-on VM Acceptance

Before tagging, use fresh, digest-pinned Tart images for Apple Silicon macOS
and Linux. Do not treat the existing aged VM as fresh.

The exact rehearsal artifacts are copied into each VM. The acceptance script
tests:

- fresh shell-installer and local formula paths;
- exact version and health output;
- disposable Rust project build;
- dry-run byte preservation;
- review-ID execution and recovered-byte accounting;
- `--no-scan`;
- narrowed scope retaining an out-of-scope sentinel;
- Cargo failure reporting and exit `1`;
- an incomplete scan reporting exit `2` while still printing usable results,
  and a fully clean run reporting `0`;
- strict config typo/undefined-variable handling;
- a legacy `excludes` config loading with a deprecation warning, and
  `config migrate` rewriting it to `override_excludes`;
- `config` output redirected to a config file and reloaded unchanged;
- service absent/install/running/stop/reboot/start/uninstall behavior;
- config/state retention;
- v0.2 and v0.3 upgrades in active, stopped, and absent states;
- macOS default `~/Library` protection and visible privacy errors.

Intel macOS and x86_64 Linux remain covered by GitHub-hosted architecture
runners because Apple Silicon Tart cannot execute those native targets.

This is a dependency on a runner label GitHub is retiring. Before relying on
it, the rehearsal workflow resolves the Intel macOS label it will use and
fails loudly if that label no longer exists, rather than silently falling back
to an Apple Silicon runner and reporting `x86_64-apple-darwin` as validated.
If no Intel macOS runner is available, the honest position is that
`x86_64-apple-darwin` has archive- and checksum-level verification but no
install-path validation, and the release notes say so. Do not treat an
arm64 runner's success as coverage for that target.

Sanitized transcripts, commit SHA, image digests, artifact hashes, workflow
run URLs, and pass/fail results are stored as release evidence without
credentials or machine-specific secrets.

## Tart Cleanup

The user authorized deletion of every Tart VM on the development Mac after
acceptance.

Cleanup procedure:

1. Capture `tart list`, Tart storage usage, and host free space. Record each
   VM's **source image reference and digest**, not only its local name — a
   bare name is not enough to pull anything back. Any VM whose source cannot
   be determined is reported by name before deletion, because that is the one
   case where deletion is genuinely unrecoverable.
2. Verify no acceptance process still depends on a VM.
3. Stop every running Tart VM.
4. Delete every listed VM, including pre-existing VMs.
5. Run Tart garbage collection.
6. Require `tart list` to be empty.
7. Report host free space before/after and estimated bytes reclaimed.

Deletion happens only after evidence is copied out and validation is complete.
VM deletion is not recoverable unless the source image is pulled again, and
local changes inside a VM are never recoverable.

Step 4 deletes pre-existing VMs that have nothing to do with this release. The
user authorized that explicitly. The inventory from step 1 is printed and
confirmed immediately before deletion rather than only archived, so the
authorization is exercised against a concrete list instead of a category.

## Error Handling and Rollback

- Every workflow job binds evidence to an exact commit.
- Partial draft uploads are safely rerunnable.
- Tap rehearsal state is cleaned on failure.
- A failed prerelease smoke is never promoted.
- A failed stable smoke after promotion is documented as a patch-release
  incident; tags are immutable.
- VM failure preserves the VM until logs are extracted, then cleanup proceeds.
- No command modifies the development Mac's installed service.

## Final Release-readiness Gate

Release readiness requires:

- all runtime and operator plans complete and independently reviewed;
- full local Rust, installer, workflow, and release-note gates;
- pre-tag rehearsal green for all four targets;
- tap capability rehearsal green;
- macOS and Linux VM acceptance green;
- combined independent release review with no Critical or Important findings;
- clean synchronized `main`;
- successful exact-SHA CI;
- all Tart VMs deleted and cleanup verified;
- no v0.4.0 tag or release until the user separately authorizes it.
