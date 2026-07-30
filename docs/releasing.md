# Releasing car-go-clean

Releases are tag-driven GitHub Releases. Begin from a clean checkout of the
verified commit that will become `v0.4.0`; do not create a release from local
uncommitted work. Before tagging, ensure the public
[`dcchuck/homebrew-tap`](https://github.com/dcchuck/homebrew-tap) repository
exists and that this repository has the `HOMEBREW_TAP_TOKEN` Actions secret
from a fine-grained token with repository contents and pull-request write
permission limited to that tap. Store it without printing it:

```bash
printf %s "$HOMEBREW_TAP_TOKEN" | gh secret set HOMEBREW_TAP_TOKEN --repo dcchuck/car-go-clean
```

The release preflight fails before hosting if this secret is empty or absent.

Configure both publication environments before tagging. The authenticated
GitHub user must be the intended release approver and must have repository
Administration write permission:

```bash
gh auth status
scripts/configure-release-environments.sh dcchuck/car-go-clean
```

The configurator is restricted to this repository. It replaces
`v040-prerelease` and `v040-stable` with one required user reviewer,
`prevent_self_review: false`, a zero wait timer, and no deployment-branch
policy, then reads both environments back and fails unless the exact reviewer
configuration is present.

Inspect any older open formula pull request in `dcchuck/homebrew-tap` before
tagging. Explicitly merge, close, or supersede it according to the version
that should remain installable; do not silently overwrite its branch or the
tap's default branch.

Run the complete local verification suite:

```bash
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
mise exec rust@1.95.0 -- cargo test --locked
make test
dist plan --tag v0.4.0 --output-format=json
```

Rust 1.95 is both the repository toolchain and the declared minimum. The
`make test` gate verifies those declarations remain aligned.

## Bind the tag to a successful rehearsal

Push the intended clean commit to `main`, then record its full SHA and version.
The rehearsal must be dispatched from `main` at that same SHA; an ancestor SHA
is not accepted even when it is contained by `main`.

```sh
git push origin main
git fetch origin main
release_sha=$(git rev-parse 'HEAD^{commit}')
version=$(cargo metadata --no-deps --format-version 1 |
  jq -er '.packages[] | select(.name == "car-go-clean").version')
test "$(git rev-parse 'origin/main^{commit}')" = "$release_sha"
printf '%s\n' "$release_sha" | LC_ALL=C grep -Eq '^[0-9a-f]{40}$'
printf '%s\n' "$version" |
  LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'

gh workflow run rehearse-release.yml \
  --ref main \
  -f commit_sha="$release_sha" \
  -f version="$version"
run_id=$(
  gh run list \
    --workflow rehearse-release.yml \
    --branch main \
    --event workflow_dispatch \
    --commit "$release_sha" \
    --limit 1 \
    --json databaseId \
    --jq '.[0].databaseId'
)
test -n "$run_id"
gh run watch "$run_id" --exit-status
```

The successful aggregate creates
`release-authorization-$release_sha-v$version`. Download and independently
verify the record before tagging:

```sh
record_dir=$(mktemp -d)
gh run download "$run_id" \
  --name "release-authorization-$release_sha-v$version" \
  --dir "$record_dir"
jq -e \
  --arg exact_sha "$release_sha" \
  --arg version "$version" \
  '.exact_sha == $exact_sha and .version == $version and .status == "success"' \
  "$record_dir/rehearsal-authorization.json"
gh attestation verify "$record_dir/rehearsal-authorization.json" \
  --repo dcchuck/car-go-clean \
  --signer-workflow dcchuck/car-go-clean/.github/workflows/rehearse-release.yml \
  --source-digest "$release_sha" \
  --signer-digest "$release_sha" \
  --source-ref refs/heads/main
```

Only the aggregate job can create this record. Its record, attestation, and
90-day artifact upload run after validation, all four builds, all four
install-path smokes, hosted-runner resolution, tap capability, complete
evidence collection, and sanitization have all succeeded. Earlier jobs upload
diagnostic evidence on failure, but cannot reach the authorization steps.
`actions/attest` signs the record digest using GitHub OIDC; the release
workflow verifies the signer workflow, `refs/heads/main`, and both the source
and signer digest against the tag's full `github.sha`. It also selects only a
successful rehearsal run whose head SHA equals that full SHA, requires the
SHA-and-version artifact key, and compares both fields in the record. Thus an
ancestor rehearsal, a suffix-bearing tag, a renamed partial artifact, or a
record from another workflow fails closed before cargo-dist planning or draft
hosting. A repository administrator can delete the retained artifact and
thereby block a release, but cannot substitute different bytes without
invalidating the attestation.

After the rehearsal is green, require that local and remote `main` still point
to the rehearsed commit, create the annotated tag on that exact commit, and
push only the tag:

```bash
test "$(git rev-parse 'HEAD^{commit}')" = "$release_sha"
test "$(git rev-parse 'origin/main^{commit}')" = "$release_sha"
git tag -a "v$version" "$release_sha" -m "car-go-clean v$version"
git push origin "v$version"
```

The release workflow accepts only an annotated stable tag matching
`^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$`, whose exact commit
and version match the attested rehearsal and `Cargo.toml`. The shell
installer's `--version` option likewise accepts only exactly three decimal
components such as `0.4.0`, with no prefix, suffix, fourth component,
whitespace, or path characters.

The workflow first creates a GitHub draft containing four target archives
(`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-musl`, and `x86_64-unknown-linux-musl`), a matching
`.sha256` file for each archive, and the four cargo-dist global assets: 12
files in total. The shell publisher then adds
`car-go-clean-installer.sh`, `car-go-clean-upgrade.sh`, and
`car-go-clean-shell-assets.sha256`. The authenticated draft verification gate
requires that exact 15-file inventory, the tag commit, checksums,
attestations, exact executable version and health, the actual installer, no
implicit service, and a locally rendered formula.

[release verification workflow](https://github.com/dcchuck/car-go-clean/blob/main/.github/workflows/release-verify.yml)
must succeed before the `v040-prerelease` environment asks for human approval.
That approved job publishes the draft as a prerelease with `latest=false`.
A rejected approval or draft-verification failure leaves the release in draft
state.

The public-asset smoke gate then starts fresh hosted macOS and Linux runners.
It downloads all four archives and checksums, all three shell assets, and the
four cargo-dist global assets (`car-go-clean.rb`, `sha256.sum`,
`source.tar.gz`, and `source.tar.gz.sha256`) through unauthenticated,
versioned public release URLs. The build workflow attests those four global
assets in a dedicated least-privilege job. Public smoke verifies those
attestations along with the native archive and shell-asset attestations. Its
token is read-only and covers source checkout plus public attestation API
verification. Each native target verifies the exact 15-file inventory,
checksums, version and health, runs the real public installer, proves no
service was installed, and renders the Homebrew formula locally from the
public assets. macOS installs and tests that local formula. The tap still
serves the previous stable version throughout this gate; no release formula
branch or pull request exists yet.

Only a successful public smoke reaches the second human approval in
`v040-stable`. Approval promotes the same prerelease to stable/latest. Only
after that promotion does the formula publisher create or update the
deterministic `formula/car-go-clean-vX.Y.Z` branch and its manual pull request;
it never pushes the tap's default branch. A public-smoke failure leaves the
release as a non-latest prerelease and does not invoke the tap publisher. The
workflow does not publish to crates.io or enable any daemon.

## Retry a publication gate

Use targeted retries so the workflow resumes from the release state that has
already been approved. If hosted public smoke fails while the release is a
non-latest prerelease, retry only the failed jobs:

```sh
gh run rerun RUN_ID --failed
```

If stable promotion succeeded but formula publication failed, find the
reusable formula job ID and retry that job:

```sh
gh run rerun RUN_ID --job FORMULA_JOB_ID
```

The transition guard re-reads the numeric release ID, tag commit, resolved
target, exact 15-asset inventory, and publication state immediately before
each mutation. A targeted prerelease retry that finds the release already
stable is a no-op, so a stable release is never demoted. A stable-transition
retry is also a no-op when that release is already stable but no longer
latest after a newer release; it does not make the older release latest again.

A full workflow rerun is not a publication retry: its authenticated verifier
requires the exact draft state and stops once the release has been published.

## Complete the Homebrew release

After stable/latest promotion creates or updates the manual tap pull request,
list all open formula pull requests:

```sh
gh pr list \
  --repo dcchuck/homebrew-tap \
  --state open \
  --search 'car-go-clean in:title' \
  --json number,title,url
```

Inspect each older formula pull request and either deliberately close it as
superseded or retain it with a written reason. Merge the v0.4.0 formula pull
request only after its checks pass. The `--web` step below is the required
human review of the formula diff.

```sh
formula_pr=$(
  gh pr list \
    --repo dcchuck/homebrew-tap \
    --state open \
    --search 'car-go-clean v0.4.0 in:title' \
    --json number \
    --jq '.[0].number'
)
test -n "$formula_pr"
gh pr checks "$formula_pr" --repo dcchuck/homebrew-tap
gh pr view "$formula_pr" --repo dcchuck/homebrew-tap --web
gh pr merge "$formula_pr" --repo dcchuck/homebrew-tap --merge --delete-branch
```

## Validate upgrade and fresh-install paths

Exercise the released `car-go-clean-upgrade.sh` against v0.2.0 and v0.3.0
fixtures for active, stopped, and absent service state. The helper is the
supported two-phase path. `--method` must name the owner of the existing
visible binary; it is not a requested migration destination:

```sh
./car-go-clean-upgrade.sh --version 0.4.0 --method homebrew
./car-go-clean-upgrade.sh \
  --version 0.4.0 \
  --method homebrew \
  --execute-review REVIEW_ID
```

Phase one records old absent/stopped/active state and persistently disables and
stops every installed definition with the native manager before replacement.
It upgrades through the verified owner, disarms automatic old restoration,
derives and validates the exact v0.4.0 binary, and uses that exact binary to
refresh the installed definition and physical manager-root environment without
enabling or starting it. Config validation and `run --dry-run --all` happen
while disabled. Preview exit `0` and `2` are accepted; exit `1` stops with
recovery guidance. Its mode-0600 session persists that absolute binary path
and makes the same phase-one command resumable.

Phase two invokes that exact path and executes only the supplied persisted ID
while still disabled. After explicit approval and success, it re-enables and
starts only a service that was originally active; stopped remains
installed/disabled/stopped and absent remains absent. A pre-replacement
failure may roll an active old service back. A wrong-version or later
pre-approval failure persists recovery state and stays disabled/stopped. An
ambiguous post-replacement execution fails closed and must not be repeated
blindly. Old-version rollback validates the restored binary, restores the
saved definition, reloads the manager where needed, and only then performs
native launchctl/systemd restoration. It never assumes v0.2/v0.3 has
`service start`. Service uninstall or rollback retains configuration, state,
logs, and history.

Use `--method shell` consistently in both phases only when the visible old
binary is shell-owned; use `homebrew` only when the visible old binary resolves
to the installed formula. Cross-method migration is deliberately outside this
helper and requires an explicit uninstall followed by a fresh install. Confirm
the helper warns on v0.4's still-supported legacy `excludes` key and points to
`car-go-clean config migrate`; v0.5 removes that
key.

Finally run the [fresh-install validation](fresh-install-validation.md) in a
disposable macOS or Linux environment. It must prove that binary installation
leaves the service absent, dry-run preserves a disposable Rust target, exact
review execution removes it, and format-v1 JSON/NDJSON exit codes match their
terminal envelopes. Do not use dynamic bare `run` outside that disposable
fixture.
