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
`make test` gate verifies those declarations remain aligned. Only after that
and all remaining release gates are green:

```bash
git tag -a v0.4.0 -m "car-go-clean v0.4.0"
git push origin main v0.4.0
```

The release workflow accepts only an annotated `vX.Y.Z` tag whose version
matches `Cargo.toml`. The shell installer's `--version` option likewise accepts
only exactly three decimal components such as `0.4.0`, with no prefix, suffix,
fourth component, whitespace, or path characters.

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
It downloads all four archives and checksums plus the shell assets through
unauthenticated, versioned public release URLs. Its token is read-only and
covers source checkout plus public attestation API verification. Each native
target verifies exact checksums, version and health, runs the real public
installer, proves no service was installed, and renders the Homebrew formula
locally from the public assets. macOS installs and tests that local formula.
The tap still serves the previous stable version throughout this gate; no
release formula branch or pull request exists yet.

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

Phase one records old state, stops an active old service with the native
manager, upgrades through the verified owner, derives and validates the exact
v0.4.0 binary, validates config, and creates `run --dry-run --all` review
state. Preview exit `0` and `2` are accepted; exit `1` stops with recovery
guidance. Its mode-0600 session persists that absolute binary path and makes
the same phase-one command resumable. Phase two invokes that exact path,
executes only the supplied persisted ID, accepts reviewed exit `0` or `2`, and
restores only a service that was originally active. A pre-replacement failure
rolls an active old service back; an ambiguous post-replacement execution
fails closed and must not be repeated blindly.

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
