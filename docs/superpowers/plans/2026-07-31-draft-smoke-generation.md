# Draft Smoke Generation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the authenticated v0.4.0 draft smoke establish a discovery generation before asserting health, then recover the unpublished release and upgrade the current Mac through the verified v0.4 flow.

**Architecture:** Keep runtime semantics unchanged. Strengthen the release workflow by making its isolated install-path smoke follow the same `scan`-then-`health` sequence as the release rehearsal, guard that sequence structurally in the Rust packaging tests, and only replace the private draft/tag after the corrected commit passes CI and exact rehearsal.

**Tech Stack:** GitHub Actions YAML, POSIX shell, Rust integration tests with `yaml-rust2`, cargo-dist 0.32.0, GitHub CLI, Homebrew, launchd.

## Global Constraints

- Do not change car-go-clean runtime exit codes; missing discovery generations must remain incomplete with exit code 2.
- The draft smoke must use the same isolated `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, config path, and state directory for `scan` and `health`.
- The private draft and tag may be replaced only after the corrected commit passes local verification, main CI, and the exact release rehearsal.
- Do not approve either protected publication environment until every preceding job and exact release-state check passes.
- Do not execute any cleanup review on the live Mac without inspecting its generated candidates first.

---

### Task 1: Establish a generation in authenticated draft smoke

**Files:**
- Modify: `tests/packaging.rs`
- Modify: `.github/workflows/release-verify.yml`

**Interfaces:**
- Consumes: the `smoke` job's `Verify authenticated draft install paths` shell step and its isolated environment variables.
- Produces: a structural regression test named `authenticated_draft_smoke_establishes_generation_before_health` and a workflow sequence in which `"$binary" scan` precedes `"$binary" health --skip-cargo`.

- [ ] **Step 1: Write the failing packaging regression test**

Add this test beside `authenticated_draft_verification_requires_all_fifteen_assets` in `tests/packaging.rs`:

```rust
#[test]
fn authenticated_draft_smoke_establishes_generation_before_health() {
    let verify = workflow(".github/workflows/release-verify.yml");
    let smoke_steps = workflow_steps(&verify, "smoke");
    let verify_paths = named_step(smoke_steps, "Verify authenticated draft install paths");
    let run = run_command(verify_paths).unwrap();

    let scan = run
        .find("\"$binary\" scan")
        .expect("authenticated draft smoke must create a discovery generation");
    let health = run
        .find("\"$binary\" health --skip-cargo")
        .expect("authenticated draft smoke must verify health");
    assert!(
        scan < health,
        "authenticated draft smoke must scan before checking health"
    );
}
```

- [ ] **Step 2: Run the focused test and verify the intended failure**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --locked --test packaging authenticated_draft_smoke_establishes_generation_before_health -- --exact
```

Expected: FAIL with `authenticated draft smoke must create a discovery generation`.

- [ ] **Step 3: Add the minimal isolated scan command**

In `.github/workflows/release-verify.yml`, immediately after writing `config.toml` and before the existing health command, add:

```yaml
          HOME="$ISOLATED_ROOT/home" \
            XDG_CONFIG_HOME="$ISOLATED_ROOT/home/.config" \
            XDG_STATE_HOME="$ISOLATED_ROOT/home/.local/state" \
            "$binary" scan \
              --config "$ISOLATED_ROOT/config.toml" \
              --state-dir "$ISOLATED_ROOT/state"
```

Keep the following `health --skip-cargo` command unchanged.

- [ ] **Step 4: Run the focused test and workflow-sensitive packaging suite**

Run:

```bash
mise exec rust@1.95.0 -- cargo test --locked --test packaging authenticated_draft_smoke_establishes_generation_before_health -- --exact
mise exec rust@1.95.0 -- cargo test --locked --test packaging
```

Expected: both commands PASS.

- [ ] **Step 5: Run repository verification**

Run:

```bash
git diff --check
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
mise exec rust@1.95.0 -- cargo test --locked
make test
dist plan --tag v0.4.0 --output-format=json
```

Expected: every command exits 0; the dist plan contains the reviewed 12 cargo-dist artifacts.

- [ ] **Step 6: Commit the implementation**

```bash
git add tests/packaging.rs .github/workflows/release-verify.yml
git commit -m "fix: establish generation in draft smoke"
```

Expected: one focused implementation commit following the committed design.

---

### Task 2: Reauthorize and publish the corrected v0.4.0 release

**Files:**
- Read: `docs/releasing.md`
- Read: `.github/workflows/rehearse-release.yml`
- Read: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: the exact corrected `main` commit, the `v0.4.0` release tag, and the protected environments `v040-prerelease` and `v040-stable`.
- Produces: a stable/latest public GitHub release containing exactly 15 verified assets and a formula-only Homebrew tap pull request.

- [ ] **Step 1: Push the corrected commit and require green main CI**

```bash
git push origin main
release_sha=$(git rev-parse 'HEAD^{commit}')
ci_run=$(gh run list --repo dcchuck/car-go-clean --branch main --workflow CI --commit "$release_sha" --limit 1 --json databaseId --jq '.[0].databaseId')
test -n "$ci_run"
gh run watch "$ci_run" --repo dcchuck/car-go-clean --exit-status
```

Expected: the CI run for the exact local `HEAD` completes successfully.

- [ ] **Step 2: Run and verify the exact release rehearsal**

Run:

```bash
git fetch origin main
release_sha=$(git rev-parse 'HEAD^{commit}')
test "$(git rev-parse 'origin/main^{commit}')" = "$release_sha"
gh workflow run rehearse-release.yml --ref main -f commit_sha="$release_sha" -f version=0.4.0
run_id=$(gh run list --workflow rehearse-release.yml --branch main --event workflow_dispatch --commit "$release_sha" --limit 1 --json databaseId --jq '.[0].databaseId')
test -n "$run_id"
gh run watch "$run_id" --exit-status
record_dir=$(mktemp -d)
gh run download "$run_id" --name "release-authorization-$release_sha-v0.4.0" --dir "$record_dir"
jq -e --arg exact_sha "$release_sha" '.exact_sha == $exact_sha and .version == "0.4.0" and .status == "success"' "$record_dir/rehearsal-authorization.json"
gh attestation verify "$record_dir/rehearsal-authorization.json" --repo dcchuck/car-go-clean --signer-workflow dcchuck/car-go-clean/.github/workflows/rehearse-release.yml --source-digest "$release_sha" --signer-digest "$release_sha" --source-ref refs/heads/main
```

Expected: all four native build/install jobs, tap-capability rehearsal, evidence aggregation, and the signed authorization record succeed for the same `release_sha`.

- [ ] **Step 3: Validate the failed release is still unpublished**

```bash
gh release view v0.4.0 --repo dcchuck/car-go-clean --json id,isDraft,isPrerelease,isLatest,targetCommitish
gh api repos/dcchuck/car-go-clean/actions/runs/30632058689/pending_deployments
```

Expected: the release is draft-only, is not prerelease/latest, targets the previous exact commit, and has no approved protected deployment.

- [ ] **Step 4: Delete only the unpublished draft and old tag**

Run:

```bash
failed_release=$(mktemp)
gh api repos/dcchuck/car-go-clean/releases/tags/v0.4.0 > "$failed_release"
jq -e '.tag_name == "v0.4.0" and .draft == true and .prerelease == false and .target_commitish == "f6dfec483ab35a8672342ffd247f9879e50a5bc6"' "$failed_release"
release_id=$(jq -er '.id | select(type == "number")' "$failed_release")
printf 'Deleting unpublished release ID %s\n' "$release_id"
gh api --method DELETE "repos/dcchuck/car-go-clean/releases/$release_id"
git push origin :refs/tags/v0.4.0
git tag -d v0.4.0
git rev-parse -q --verify refs/tags/v0.4.0
git ls-remote --exit-code --tags origin refs/tags/v0.4.0
gh release view v0.4.0 --repo dcchuck/car-go-clean
```

Expected: each of the final three lookup commands reports the tag/release absent. Do not continue if any lookup still succeeds.

- [ ] **Step 5: Create and push the corrected annotated tag**

```bash
git tag -a v0.4.0 -m "car-go-clean 0.4.0"
git push origin v0.4.0
```

Expected: the annotated tag peels to the exact corrected `main` SHA and starts a new release workflow run.

- [ ] **Step 6: Inspect and approve the prerelease gate**

Resolve the new run and wait until its only pending deployment is the prerelease gate:

```bash
release_sha=$(git rev-parse 'HEAD^{commit}')
release_run=$(gh run list --repo dcchuck/car-go-clean --workflow release.yml --event push --commit "$release_sha" --limit 1 --json databaseId --jq '.[0].databaseId')
test -n "$release_run"
gh run view "$release_run" --repo dcchuck/car-go-clean --json jobs
pending=$(mktemp)
gh api "repos/dcchuck/car-go-clean/actions/runs/$release_run/pending_deployments" > "$pending"
prerelease_environment_id=$(jq -er 'select(length == 1) | .[0] | select(.environment.name == "v040-prerelease") | .environment.id' "$pending")
gh api --method POST "repos/dcchuck/car-go-clean/actions/runs/$release_run/pending_deployments" -F "environment_ids[]=$prerelease_environment_id" -f state=approved -f comment='Authenticated draft verification passed for the exact rehearsed v0.4.0 commit.'
```

Before the POST, require every completed build, global artifact, draft hosting, shell asset, inventory, attestation, and authenticated draft smoke job to have conclusion `success`.

- [ ] **Step 7: Require public hosted smoke before stable promotion**

After prerelease publication, run:

```bash
candidate=$(mktemp)
gh api repos/dcchuck/car-go-clean/releases/tags/v0.4.0 > "$candidate"
jq -e --arg release_sha "$release_sha" '.draft == false and .prerelease == true and .target_commitish == $release_sha and (.assets | length) == 15' "$candidate"
gh run view "$release_run" --repo dcchuck/car-go-clean --json jobs
pending=$(mktemp)
gh api "repos/dcchuck/car-go-clean/actions/runs/$release_run/pending_deployments" > "$pending"
stable_environment_id=$(jq -er 'select(length == 1) | .[0] | select(.environment.name == "v040-stable") | .environment.id' "$pending")
gh api --method POST "repos/dcchuck/car-go-clean/actions/runs/$release_run/pending_deployments" -F "environment_ids[]=$stable_environment_id" -f state=approved -f comment='All four public hosted smokes passed for the exact v0.4.0 candidate.'
```

Before the POST, require all four public hosted smoke jobs and the tap-CI capability check to have conclusion `success`.

- [ ] **Step 8: Verify stable release and tap pull request**

Run:

```bash
gh run watch "$release_run" --repo dcchuck/car-go-clean --exit-status
stable=$(mktemp)
gh api repos/dcchuck/car-go-clean/releases/tags/v0.4.0 > "$stable"
jq -e --arg release_sha "$release_sha" '.draft == false and .prerelease == false and .target_commitish == $release_sha and (.assets | length) == 15' "$stable"
asset_dir=$(mktemp -d)
gh release download v0.4.0 --repo dcchuck/car-go-clean --dir "$asset_dir"
scripts/verify-release-assets.sh v0.4.0 "$asset_dir"
scripts/verify-shell-release-assets.sh "$asset_dir"
for asset in "$asset_dir"/*; do gh attestation verify "$asset" --repo dcchuck/car-go-clean; done
formula_pr=$(gh pr list --repo dcchuck/homebrew-tap --state open --search 'car-go-clean v0.4.0 in:title' --json number --jq '.[0].number')
test -n "$formula_pr"
gh pr diff "$formula_pr" --repo dcchuck/homebrew-tap --name-only
gh pr checks "$formula_pr" --repo dcchuck/homebrew-tap --watch
gh pr merge "$formula_pr" --repo dcchuck/homebrew-tap --merge --delete-branch
```

Expected: `v0.4.0` is public, stable, latest, commit-bound to `release_sha`, all 15 assets pass checksum/attestation verification, the tap diff contains only `Formula/car-go-clean.rb`, and the green formula PR is merged.

---

### Task 3: Upgrade and validate this Mac through the v0.4 helper

**Files:**
- Read: `docs/releasing.md`
- Read: `docs/v0.4-owner-tour.md`
- Runtime state: Homebrew installation and user launchd service on the current Mac.

**Interfaces:**
- Consumes: the public attested `car-go-clean-upgrade.sh`, `car-go-clean-shell-assets.sha256`, and v0.4.0 Homebrew formula.
- Produces: a Homebrew-installed car-go-clean 0.4.0 binary with the previously active launchd service restored and verified.

- [ ] **Step 1: Capture the pre-upgrade host state**

Run without changing any files:

```bash
command -v car-go-clean
car-go-clean version
brew list --versions car-go-clean
car-go-clean service status
pgrep -fl car-go-clean
test -f "$HOME/.config/car-go-clean/config.toml"
```

Expected pre-state: Homebrew-owned v0.2.0, installed/enabled/running launchd service, and no config file. Record the exact PID.

- [ ] **Step 2: Download and verify the public upgrade helper**

Run:

```bash
upgrade_dir=$(mktemp -d)
gh release download v0.4.0 --repo dcchuck/car-go-clean --pattern 'car-go-clean-installer.sh' --pattern 'car-go-clean-upgrade.sh' --pattern 'car-go-clean-shell-assets.sha256' --dir "$upgrade_dir"
scripts/verify-shell-release-assets.sh "$upgrade_dir"
gh attestation verify "$upgrade_dir/car-go-clean-upgrade.sh" --repo dcchuck/car-go-clean
gh attestation verify "$upgrade_dir/car-go-clean-shell-assets.sha256" --repo dcchuck/car-go-clean
```

Expected: checksum inventory and both attestations pass before execution.

- [ ] **Step 3: Run upgrade phase one**

```bash
set -o pipefail
"$upgrade_dir/car-go-clean-upgrade.sh" --version 0.4.0 --method homebrew 2>&1 | tee "$upgrade_dir/phase-one.log"
```

Expected: the helper records original service state, safely stops/disables the service, upgrades Homebrew to 0.4.0, validates configuration, leaves the service stopped while review is pending, and prints a review ID.

- [ ] **Step 4: Inspect the generated cleanup review**

Run:

```bash
session_file=$HOME/.local/state/car-go-clean/upgrade-session
test "$(stat -f '%Lp' "$session_file")" = 600
review_id=$(sed -n 's/^review_id=//p' "$session_file")
printed_review_id=$(sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p' "$upgrade_dir/phase-one.log")
printf '%s\n' "$review_id" | LC_ALL=C grep -Eq '^[1-9][0-9]*$'
test "$review_id" = "$printed_review_id"
sed -n '1,320p' "$upgrade_dir/phase-one.log"
```

Inspect candidate projects, failure/incomplete reasons, estimated reclaim, policy hash, and discovery generation in the complete phase-one output. Do not execute the review if any candidate or authority boundary is unexpected; report the review to the user instead.

- [ ] **Step 5: Complete the approved review and restore service state**

If the review is safe and separately approved for execution, run:

```bash
session_file=$HOME/.local/state/car-go-clean/upgrade-session
test "$(stat -f '%Lp' "$session_file")" = 600
review_id=$(sed -n 's/^review_id=//p' "$session_file")
printf '%s\n' "$review_id" | LC_ALL=C grep -Eq '^[1-9][0-9]*$'
"$upgrade_dir/car-go-clean-upgrade.sh" --version 0.4.0 --method homebrew --execute-review "$review_id"
```

Expected: the bound review executes and the helper restores the service because it was active before upgrade.

- [ ] **Step 6: Verify the live v0.4 installation**

Run:

```bash
test "$(command -v car-go-clean)" = /opt/homebrew/bin/car-go-clean
test "$(car-go-clean version)" = 0.4.0
brew list --versions car-go-clean
car-go-clean service status
car-go-clean health --skip-cargo --json || test $? -eq 2
car-go-clean status --json || test $? -eq 2
car-go-clean config
pgrep -fl car-go-clean
```

Confirm the Homebrew formula reports 0.4.0; launchd is installed, enabled, and running under a new PID; diagnostic JSON reports the v0.4 safety state; macOS `~/Library` and OrbStack defaults are protected; and the service environment shows no manager-root divergence.
