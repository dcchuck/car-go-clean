# v0.4 Release Rehearsal and Publication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rehearse the exact v0.4.0 commit and every public installation path before tagging, then keep publication behind retry-safe, human-approved gates.

**Architecture:** Add a manual exact-SHA rehearsal workflow that builds the same four cargo-dist artifacts as release, runs installer/formula smoke tests, proves tap-token capability, and uploads sanitized evidence. Refactor tag publication so hosting is idempotent and commit-bound, publication first becomes a non-latest prerelease behind one protected environment, and stable/latest promotion plus tap-PR creation requires a second approval after public hosted smoke. Use repository scripts for validation/rendering so local, rehearsal, and tag workflows share behavior.

**Tech Stack:** GitHub Actions, cargo-dist 0.32.0, POSIX shell, `gh`, Homebrew, Tart, macOS/Linux hosted runners, GitHub artifact attestations.

## Global Constraints

- Runtime Safety Slice A, Slice B, and Operator Control must be complete and independently reviewed before this workflow is dispatched.
- Do not create or push `v0.4.0` while implementing or rehearsing.
- Rehearsal binds to immutable commit SHA `b900b802f620a548d50e958e4a79b5fdb44af43e` initially; after implementation merges, dispatch against that new exact `main` SHA.
- The proposed version is exactly `0.4.0`; workflow tags are exactly `vX.Y.Z`.
- All GitHub Actions use immutable commit SHAs.
- cargo-dist is exactly `0.32.0`; the installer script SHA-256 is `b657cf8c04a8b7bc28f39d220f7e6dd11bbd2bdb072c552262bd9ccf597261b5`.
- Rehearsal never changes the real installed binary, service, config, or state on the development Mac.
- Tap capability rehearsal creates a publicly visible draft PR, closes it, and deletes only its uniquely named branch.
- The tag workflow never pushes the tap default branch and never auto-merges its PR.
- A failed prerelease smoke never becomes stable/latest.
- Tart VMs are inventoried with source reference/digest and explicitly shown immediately before deletion; evidence is copied out first.
- The user separately authorizes the final annotated tag after the entire readiness gate passes.

## Immutable Action Pins

Use these exact pins in every changed workflow:

```yaml
actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803 # v6
actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7
actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8
actions/attest@36051bcae73b7c2a8a6945a48cbf80953c6baa35 # v4
actions/attest-build-provenance@96b4a1ef7235a096b17240c259729fdd70c83d45 # v2
dtolnay/rust-toolchain@2c7215f132e9ebf062739d9130488b56d53c060c
```

---

### Task 1: Shared Release-validation Scripts

**Files:**
- Create: `scripts/install-cargo-dist.sh`
- Create: `scripts/validate-release-inputs.sh`
- Create: `scripts/render-homebrew-formula.sh`
- Create: `scripts/verify-release-assets.sh`
- Modify: `packaging/release/car-go-clean-installer.sh`
- Modify: `tests/installer.sh`
- Create: `tests/release-scripts.sh`
- Modify: `Makefile`

**Interfaces:**
- Consumes: exact commit SHA, semantic version, cargo-dist installer bytes, artifact directory, and formula template.
- Produces: verified cargo-dist binary, normalized `RELEASE_SHA`/`VERSION`/`TAG`, rendered formula, and validated artifact inventory.

- [ ] **Step 1: Write failing shell tests**

`tests/release-scripts.sh` must cover:

- rejecting a non-40-hex commit, unreachable commit, commit not contained by `origin/main`, mismatched Cargo version, dirty checkout, and malformed version;
- accepting one exact reachable clean commit whose `Cargo.toml` version matches;
- refusing a cargo-dist installer whose digest differs by one byte;
- requiring exactly four archives and four matching checksum files;
- rejecting duplicate/malformed checksum lines;
- rendering every formula placeholder exactly once and leaving no `__[A-Z0-9_]+__`;
- formula URLs, versions, and hashes matching the artifact inventory.

- [ ] **Step 2: Add the failing Make target**

```make
.PHONY: test-release-scripts
test: test-release-scripts
test-release-scripts:
	sh tests/release-scripts.sh
```

Run `make test-release-scripts`; expected failure because the scripts do not exist.

- [ ] **Step 3: Add the verified cargo-dist installer**

`scripts/install-cargo-dist.sh` downloads:

```text
https://github.com/axodotdev/cargo-dist/releases/download/v0.32.0/cargo-dist-installer.sh
```

It computes SHA-256 with `shasum -a 256` or `sha256sum`, requires exact digest `b657cf8c04a8b7bc28f39d220f7e6dd11bbd2bdb072c552262bd9ccf597261b5`, then executes the verified local file. It prints the installed `dist --version`.

- [ ] **Step 4: Validate exact rehearsal inputs**

`scripts/validate-release-inputs.sh SHA VERSION` must:

```sh
case "$1" in
  [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*)
    test "${#1}" -eq 40 ;;
  *) exit 1 ;;
esac
git diff --quiet
git diff --cached --quiet
test "$(git rev-parse "$1^{commit}")" = "$1"
git merge-base --is-ancestor "$1" origin/main
test "$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "car-go-clean").version')" = "$2"
test "v$2" != "$(git tag --points-at "$1" --list 'v*' | head -n 1)"
```

Reject any existing `v$VERSION` tag locally or remotely.

- [ ] **Step 5: Add a supported installer download-base override**

Add `--download-base-url URL`, allowed only with explicit `--version`. Default remains GitHub releases. Require `https://` unless `CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1` is set; that environment variable is documented as test-only and accepted only for loopback/file-backed rehearsal servers. Preserve checksum verification and service non-installation.

- [ ] **Step 6: Extract formula rendering and asset validation**

`scripts/render-homebrew-formula.sh TAG ARTIFACT_DIR OUTPUT` validates checksums with one shared `checksum_for` function and renders `packaging/release/homebrew/car-go-clean.rb.in`.

`scripts/verify-release-assets.sh TAG ARTIFACT_DIR` requires:

```text
car-go-clean-aarch64-apple-darwin.tar.xz
car-go-clean-x86_64-apple-darwin.tar.xz
car-go-clean-aarch64-unknown-linux-musl.tar.xz
car-go-clean-x86_64-unknown-linux-musl.tar.xz
```

plus one `.sha256` for each and no duplicate archive basename.

- [ ] **Step 7: Run shell tests**

```bash
make test-installer
make test-release-scripts
```

Expected: all pass.

- [ ] **Step 8: Commit Task 1**

```bash
git add scripts/install-cargo-dist.sh scripts/validate-release-inputs.sh scripts/render-homebrew-formula.sh scripts/verify-release-assets.sh packaging/release/car-go-clean-installer.sh tests/installer.sh tests/release-scripts.sh Makefile
git commit -m "build: share verified release tooling"
```

### Task 2: Exact-SHA Pre-tag Rehearsal Workflow

**Files:**
- Create: `.github/workflows/rehearse-release.yml`
- Modify: `.github/workflows/ci.yml`
- Modify: `tests/packaging.rs`

**Interfaces:**
- Consumes: manual `commit_sha` and `version`, shared scripts from Task 1, `HOMEBREW_TAP_TOKEN`.
- Produces: all four rehearsal archives, checksums, attestations, installer/formula smoke results, tap-capability result, and sanitized evidence.

- [ ] **Step 1: Add failing workflow-contract tests**

In `tests/packaging.rs`, parse YAML and require:

- `workflow_dispatch` inputs `commit_sha` and `version`;
- checkout `ref: ${{ inputs.commit_sha }}` with `persist-credentials: false`;
- exact action SHAs from the global pin table;
- permissions are empty by default and elevated per job only;
- four exact targets and exact runner labels;
- `macos-15-intel` is paired only with `x86_64-apple-darwin`;
- a tap capability job with `if: always()` cleanup;
- evidence upload after every matrix job.

- [ ] **Step 2: Run packaging tests and confirm failure**

```bash
cargo test --locked --test packaging rehearse_release
```

Expected: failure because the workflow is absent.

- [ ] **Step 3: Add validate and plan jobs**

The workflow starts:

```yaml
name: Rehearse release
on:
  workflow_dispatch:
    inputs:
      commit_sha:
        description: Exact 40-character commit on main
        required: true
        type: string
      version:
        description: Exact X.Y.Z version
        required: true
        type: string
permissions: {}
```

Checkout the exact input SHA, fetch `origin/main` and tags, run `scripts/validate-release-inputs.sh`, install verified cargo-dist, and run:

```bash
dist plan --tag "v${{ inputs.version }}" --output-format=json
```

- [ ] **Step 4: Build all four target archives**

Use a matrix:

```yaml
include:
  - target: aarch64-apple-darwin
    runner: macos-14
  - target: x86_64-apple-darwin
    runner: macos-15-intel
  - target: aarch64-unknown-linux-musl
    runner: ubuntu-24.04-arm
  - target: x86_64-unknown-linux-musl
    runner: ubuntu-24.04
```

Before the Intel build, require `uname -m` to report `x86_64`; every other runner must match its target architecture. Build with cargo-dist and upload archive/checksum/manifests under names containing exact SHA and target. Attest each archive.

- [ ] **Step 5: Smoke the actual installer and formula**

For each target:

- verify archive/checksum inventory;
- extract and require `car-go-clean version` exactly `VERSION`;
- run `health --skip-cargo` with isolated HOME/config/state;
- serve the artifact directory from loopback and invoke the real installer with `--download-base-url`;
- require no launchd/systemd definition after install;
- on both macOS runners, render the formula, `brew install --formula ./car-go-clean.rb`, and `brew test`;
- on Linux, validate the rendered formula and shell installer path.

- [ ] **Step 6: Resolve Intel runner availability honestly**

Add a validation job that queries the workflow plan/job metadata and verifies the resolved runner labels. If `macos-15-intel` is unavailable, fail the install-path gate and write an evidence statement that x86_64 macOS has archive/checksum coverage only; never substitute arm64 success.

- [ ] **Step 7: Upload sanitized evidence**

Each job writes JSON containing exact SHA, version, target, runner OS/architecture, archive hash, action pins, cargo-dist version, command outcomes, and no home paths/tokens. Aggregate into `release-rehearsal-${SHA}`.

- [ ] **Step 8: Run workflow static tests**

```bash
cargo test --locked --test packaging
make test-release-scripts
```

Expected: all pass.

- [ ] **Step 9: Commit Task 2**

```bash
git add .github/workflows/rehearse-release.yml .github/workflows/ci.yml tests/packaging.rs
git commit -m "ci: add exact-sha release rehearsal"
```

### Task 3: Public Tap-capability Rehearsal

**Files:**
- Modify: `.github/workflows/rehearse-release.yml`
- Create: `scripts/rehearse-tap-capability.sh`
- Modify: `tests/release-scripts.sh`
- Modify: `tests/packaging.rs`

**Interfaces:**
- Consumes: `HOMEBREW_TAP_TOKEN`, repository `dcchuck/homebrew-tap`, and GitHub run ID.
- Produces: a closed public draft PR, deleted unique branch, permission evidence, and no formula/default-branch mutation.

- [ ] **Step 1: Add failing fake-gh tests**

Assert the script:

- reads the tap default branch;
- uses branch `rehearsal/car-go-clean-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}`;
- commits only `.release-rehearsal/<run-id>.txt`;
- opens a draft PR;
- verifies contents/read, branch/write, PR/write;
- closes only the PR it created;
- deletes only the exact rehearsal branch;
- performs cleanup after failures without printing the token.

- [ ] **Step 2: Implement the guarded script**

Use a temporary clone and trap. Refuse to continue if the computed branch equals the default branch or already exists. Record PR number immediately after creation. The cleanup trap closes that PR and deletes that branch; failure to clean is a failing result with explicit manual commands.

- [ ] **Step 3: Add the workflow job**

Give only this job access to `HOMEBREW_TAP_TOKEN`. Prefer the inert evidence path because the tap currently has no `.github/workflows`; re-check immediately before dispatch and fail if new tap workflows would execute on the rehearsal branch without an ignore rule.

- [ ] **Step 4: Run tests**

```bash
make test-release-scripts
cargo test --locked --test packaging
```

Expected: all pass.

- [ ] **Step 5: Commit Task 3**

```bash
git add .github/workflows/rehearse-release.yml scripts/rehearse-tap-capability.sh tests/release-scripts.sh tests/packaging.rs
git commit -m "ci: prove tap permissions before release"
```

### Task 4: Retry-safe Commit-bound Draft Hosting

**Files:**
- Modify: `.github/workflows/release.yml`
- Modify: `.github/workflows/publish-shell-installer.yml`
- Modify: `.github/workflows/publish-homebrew-formula.yml`
- Create: `scripts/upsert-draft-release.sh`
- Modify: `tests/release-scripts.sh`
- Modify: `tests/packaging.rs`

**Interfaces:**
- Consumes: annotated tag, exact workflow SHA, built artifacts, existing GitHub release/branch state.
- Produces: one rerunnable draft bound to the tag commit and one formula-only release branch.

- [ ] **Step 1: Add failing idempotency tests**

With fake `gh`, cover absent draft, matching draft rerun, existing published release, draft targeting another commit, partial assets, matching formula branch, formula branch with unrelated diff, and branch based on the wrong tap main.

- [ ] **Step 2: Pin every release action and installer**

Replace movable action tags in all release-related workflows with the immutable pin table. Replace each direct cargo-dist `curl | sh` with `scripts/install-cargo-dist.sh`.

- [ ] **Step 3: Implement draft upsert**

`scripts/upsert-draft-release.sh TAG SHA TITLE NOTES ARTIFACT_DIR`:

- creates a draft only when no release exists;
- requires any existing release to remain draft, have the same tag, and target exact SHA;
- updates title/notes;
- deletes/replaces only expected artifact names;
- rejects published or commit-mismatched releases;
- is safe to rerun after partial upload.

- [ ] **Step 4: Harden formula and installer publishers**

The formula publisher verifies its branch base is current tap main and that its diff contains only `Formula/car-go-clean.rb`. The installer publisher uploads the installer and upgrade helper with guarded replacement and attestations. Neither publisher changes a public release state.

- [ ] **Step 5: Run workflow and shell tests**

```bash
make test-release-scripts
cargo test --locked --test packaging
```

Expected: all pass.

- [ ] **Step 6: Commit Task 4**

```bash
git add .github/workflows/release.yml .github/workflows/publish-shell-installer.yml .github/workflows/publish-homebrew-formula.yml scripts/upsert-draft-release.sh tests/release-scripts.sh tests/packaging.rs
git commit -m "ci: make draft releases retry safe"
```

### Task 5: Two Human Publication Gates and Hosted Smoke

**Files:**
- Create: `.github/workflows/hosted-release-smoke.yml`
- Modify: `.github/workflows/release.yml`
- Modify: `.github/workflows/release-verify.yml`
- Create: `scripts/configure-release-environments.sh`
- Modify: `tests/release-scripts.sh`
- Modify: `tests/packaging.rs`
- Modify: `docs/releasing.md`

**Interfaces:**
- Consumes: verified commit-bound draft, protected environments `v040-prerelease` and `v040-stable`, public versioned URLs.
- Produces: approved prerelease (not latest), hosted smoke evidence, approved stable/latest release, and only then a tap PR.

- [ ] **Step 1: Add failing publication-order tests**

Parse workflow dependencies and assert:

```text
draft verification
  -> environment v040-prerelease
  -> publish prerelease with make_latest=false
  -> hosted unauthenticated smoke
  -> environment v040-stable
  -> stable/latest promotion
  -> tap PR creation
```

Assert no tag job can bypass the two environments.

- [ ] **Step 2: Configure protected environments reproducibly**

`scripts/configure-release-environments.sh` resolves the authenticated user ID and uses GitHub’s environments API to create `v040-prerelease` and `v040-stable`, each with that user as required reviewer and `prevent_self_review: false`. It reads configuration back and fails unless the expected reviewer exists.

- [ ] **Step 3: Publish a prerelease without latest**

After authenticated draft verification and `environment: v040-prerelease`, run:

```bash
gh release edit "$TAG" --draft=false --prerelease --latest=false
```

Do not create the tap PR yet.

- [ ] **Step 4: Add unauthenticated hosted smoke**

The reusable workflow uses fresh hosted macOS/Linux runners and no contents-write token. It downloads from:

```text
https://github.com/dcchuck/car-go-clean/releases/download/$TAG/$ARCHIVE
```

It verifies installer, exact version, checksums, attestations, no implicit service, and a locally rendered Homebrew formula against the public assets. The tap remains on the previous stable version during this check.

- [ ] **Step 5: Promote only after second approval**

After hosted smoke and `environment: v040-stable`, run:

```bash
gh release edit "$TAG" --prerelease=false --latest
```

Then invoke the Homebrew formula publisher to open/update its manual PR. A hosted-smoke failure leaves the release prerelease.

- [ ] **Step 6: Run tests**

```bash
make test-release-scripts
cargo test --locked --test packaging
make test-release-notes
```

Expected: all pass.

- [ ] **Step 7: Commit Task 5**

```bash
git add .github/workflows/hosted-release-smoke.yml .github/workflows/release.yml .github/workflows/release-verify.yml scripts/configure-release-environments.sh tests/release-scripts.sh tests/packaging.rs docs/releasing.md
git commit -m "ci: gate public release promotion"
```

### Task 6: Fresh Tart Acceptance Harness

**Files:**
- Create: `scripts/release/acceptance.sh`
- Create: `scripts/release/tart-rehearsal.sh`
- Create: `scripts/release/tart-inventory.sh`
- Create: `scripts/release/tart-cleanup.sh`
- Create: `tests/release-acceptance.sh`
- Modify: `Makefile`
- Modify: `docs/fresh-install-validation.md`

**Interfaces:**
- Consumes: exact rehearsal artifacts/evidence, digest-pinned Apple Silicon macOS and Linux Tart images.
- Produces: sanitized per-VM transcripts and a verified empty Tart inventory after cleanup.

- [ ] **Step 1: Add failing local harness tests**

Use fake `tart`, `ssh`, `scp`, `cargo`, and `car-go-clean` commands to verify:

- every acceptance assertion is executed;
- a failed assertion preserves logs before cleanup;
- inventory records name, source reference, and digest;
- unknown-source VMs are printed distinctly;
- cleanup targets only the exact concrete inventory;
- final `tart list` must be empty.

- [ ] **Step 2: Implement the guest acceptance script**

`acceptance.sh` tests:

- shell installer and local formula paths;
- exact version/health;
- disposable Rust build;
- dry-run byte preservation;
- review-ID execution/recovered bytes;
- `--no-scan`;
- narrowed-scope sentinel;
- Cargo failure exit `1`;
- incomplete scan exit `2` and complete exit `0`;
- strict config typo/undefined variable;
- legacy migration and config round trip;
- service absent/install/running/stop/reboot/start/uninstall;
- config/state retention;
- v0.2/v0.3 active/stopped/absent upgrades;
- macOS `~/Library` protection and visible privacy errors.

All HOME/config/state paths are guest-local.

- [ ] **Step 3: Implement digest-pinned VM orchestration**

`tart-rehearsal.sh` requires explicit image references including immutable digests, pulls fresh images, clones uniquely named VMs, copies exact artifacts, verifies their hashes inside guests, runs acceptance, and copies sanitized evidence out before returning.

- [ ] **Step 4: Implement safe complete cleanup**

`tart-inventory.sh` records:

```text
name<TAB>state<TAB>source_reference<TAB>source_digest
```

plus `du` for Tart storage and host `df` free space. `tart-cleanup.sh INVENTORY` prints the exact list, requires `CAR_GO_CLEAN_TART_DELETE_ALL=YES`, stops each listed VM, deletes each exact name, runs Tart garbage collection, and fails unless `tart list` is empty. It reports before/after bytes.

- [ ] **Step 5: Run fake harness tests**

```bash
make test-release-acceptance
```

Expected: all pass without touching real Tart.

- [ ] **Step 6: Commit Task 6**

```bash
git add scripts/release/acceptance.sh scripts/release/tart-rehearsal.sh scripts/release/tart-inventory.sh scripts/release/tart-cleanup.sh tests/release-acceptance.sh Makefile docs/fresh-install-validation.md
git commit -m "test: add fresh release acceptance harness"
```

### Task 7: Owner’s v0.4 Product Tour

**Files:**
- Create: `docs/v0.4-owner-tour.md`
- Modify: `README.md`
- Modify: `tests/packaging.rs`

**Interfaces:**
- Consumes: behavior proven by runtime, operator, release, and VM acceptance.
- Produces: a comprehensive owner-oriented walkthrough linked from the README.

- [ ] **Step 1: Add documentation contract tests**

Require the tour to include every section below and require every shown CLI flag to exist in `car-go-clean --help`.

- [ ] **Step 2: Write the tour around operator questions**

Use these sections:

1. “What car-go-clean is now” — one-page mental model.
2. “What it finds” — roots, explicit projects, worktrees, origins, exclusions, protected storage.
3. “What grants cleanup authority” — policy hash, generation, observation, identity, review.
4. “A target’s lifecycle” — discovery → review → revalidation → Cargo → audit/accounting.
5. “Using it once” — dynamic and review-ID paths.
6. “Running the daemon” — schedule, forced scan, persistent service states, environment capture.
7. “Understanding output” — text/JSON, `0`/`2`/`1`, skip reasons, macOS privacy.
8. “Configuration tour” — defaults, additions, advanced overrides, migration, round trip.
9. “Safety tour” — symlinks, devices, activity, quiet period, managed caches, residual TOCTOU.
10. “Data and accounting” — database history versus authority, clean events, errors, recovered bytes.
11. “Install, upgrade, rollback, uninstall” — Homebrew/shell and v0.2/v0.3 states.
12. “Release pipeline” — rehearsal, approvals, artifacts, hosted smoke, tap PR.
13. “Hands-on guided lab” — disposable Rust project with dry run, review execution, stats, and service lifecycle.
14. “Troubleshooting map” — privacy prompts, exit `2`, lock conflicts, service divergence, config migration.
15. “What remains deliberately manual” — reviewed cleanup, risky flags, tap merge, tag authorization.

Include a compact glossary and “If you remember only five things.”

- [ ] **Step 3: Link the tour from README**

Add a short “Owner’s tour” link near Quick Start; do not copy the full guide into README.

- [ ] **Step 4: Run documentation checks**

```bash
cargo test --locked --test packaging owner_tour
make test-release-notes
```

Expected: all pass.

- [ ] **Step 5: Commit Task 7**

```bash
git add docs/v0.4-owner-tour.md README.md tests/packaging.rs
git commit -m "docs: add comprehensive v0.4 owner tour"
```

### Task 8: Full Local and Hosted Pre-tag Rehearsal

**Files:**
- Create at runtime: `release-evidence/v0.4.0/<exact-sha>/` (gitignored evidence index only; raw logs remain workflow artifacts).
- Modify: `.gitignore` only if needed to exclude raw local evidence.

**Interfaces:**
- Consumes: completed Tasks 1–7, synchronized main, GitHub environments, Tart.
- Produces: exact-SHA release evidence with every readiness gate except the final tag.

- [ ] **Step 1: Run all local gates**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
make test
dist plan --tag v0.4.0 --output-format=json
git diff --check
git status --short
```

- [ ] **Step 2: Configure and verify publication environments**

Run:

```bash
scripts/configure-release-environments.sh dcchuck/car-go-clean
```

Record the API readback without tokens.

- [ ] **Step 3: Dispatch the exact-SHA rehearsal**

Resolve `RELEASE_SHA=$(git rev-parse origin/main)` after implementation is merged, then:

```bash
gh workflow run rehearse-release.yml \
  --repo dcchuck/car-go-clean \
  --ref main \
  -f commit_sha="$RELEASE_SHA" \
  -f version=0.4.0
```

Wait for completion and download the evidence artifact. Require all four target jobs and tap capability green.

- [ ] **Step 4: Run fresh Tart acceptance**

Record the exact macOS and Linux image references/digests, then run the harness against the same rehearsal artifact hashes. Preserve sanitized transcripts outside the VMs.

- [ ] **Step 5: Inventory and delete every Tart VM**

Run inventory, display the concrete list, verify evidence is copied out, then:

```bash
CAR_GO_CLEAN_TART_DELETE_ALL=YES scripts/release/tart-cleanup.sh /absolute/path/to/inventory.tsv
```

Require empty `tart list` and report reclaimed disk space. VMs and unexported local changes are unrecoverable after this step.

- [ ] **Step 6: Record the readiness index**

The committed or attached index contains exact SHA, workflow URLs, target results, hashes, image digests, sanitized transcript paths, CI result, review result, and an explicit statement that no `v0.4.0` tag/release exists.

### Task 9: Combined Independent Release Review

**Files:**
- Modify only files needed to address reviewer findings.

**Interfaces:**
- Consumes: exact merged implementation and complete rehearsal evidence.
- Produces: no unresolved Critical or Important findings.

- [ ] **Step 1: Dispatch independent reviewers**

Assign separate reviewers to:

- runtime authority/schema/identity;
- operator plans, service persistence, and upgrades;
- release workflow permissions/idempotency/publication ordering;
- installer/Homebrew and Tart evidence;
- documentation/product-tour accuracy.

- [ ] **Step 2: Require evidence-backed findings**

Every finding includes severity, file/line or evidence artifact, failure scenario, and required remediation. Reviewers must distinguish untested concerns from reproduced failures.

- [ ] **Step 3: Fix Critical and Important findings**

Use TDD for code fixes, rerun the narrow failing gate, then rerun the full affected subsystem. Commit each coherent fix.

- [ ] **Step 4: Re-review fixes independently**

No reviewer approves their own fix. Repeat until no Critical or Important findings remain.

- [ ] **Step 5: Run final exact-head verification**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
make test
dist plan --tag v0.4.0 --output-format=json
git diff --check
git status --short
```

- [ ] **Step 6: Present the release decision**

Report:

- exact candidate SHA;
- all local/hosted/Tart/reviewer evidence;
- the installed development-Mac version and daemon state, still unchanged;
- remaining manual steps: create annotated tag, approve prerelease, approve stable promotion, merge tap PR, optionally upgrade this Mac.

Do not create the tag until the user separately says to release that exact SHA.
