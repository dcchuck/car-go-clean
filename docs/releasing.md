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

Rust 1.95 is the repository toolchain, but the manifest declares Rust 1.88 as
the minimum. This compatibility gate is separate and unresolved until the
minimum-version lane passes (or the declared minimum is deliberately revised):

```bash
mise exec rust@1.88.0 -- cargo test --locked
```

Do not tag on the strength of Rust 1.95 alone. Only after both compatibility
decisions and all remaining release gates are green:

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
`.sha256` file for each archive, and provenance attestations. After that draft
exists, one publisher uploads `car-go-clean-installer.sh`,
`car-go-clean-upgrade.sh`, and `car-go-clean-shell-assets.sha256` while the tap
publisher pushes the generated formula only to the deterministic
`formula/car-go-clean-vX.Y.Z` branch. It opens or updates a formula-bump pull request
and never pushes the tap's default branch.

[release verification workflow](https://github.com/dcchuck/car-go-clean/blob/main/.github/workflows/release-verify.yml)
downloads each archive from the authenticated draft, verifies its checksum,
smoke-tests the binary, and audits the formula from that deterministic pull
request branch. The announce job publishes the draft only after every archive
and formula check succeeds. A failed check leaves the GitHub Release in
draft state for investigation. The workflow does not publish to crates.io or
enable any daemon.

## Complete the Homebrew release

After GitHub publishes the verified release, list all open formula pull
requests:

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
supported two-phase path:

```sh
./car-go-clean-upgrade.sh --version 0.4.0 --method homebrew
./car-go-clean-upgrade.sh \
  --version 0.4.0 \
  --method homebrew \
  --execute-review REVIEW_ID
```

Phase one records old state, stops an active old service with the native
manager, installs exact v0.4.0, validates config, and creates
`run --dry-run --all` review state. Preview exit `0` and `2` are accepted;
exit `1` stops with recovery guidance. Its session makes the same phase-one
command resumable. Phase two executes only the supplied persisted ID, accepts
reviewed exit `0` or `2`, and restores only a service that was originally
active. A pre-replacement failure rolls an active old service back; an
ambiguous post-replacement execution fails closed and must not be repeated
blindly.

Use `--method shell` consistently in both phases when validating the shell
installer route. Confirm the helper warns on v0.4's still-supported legacy
`excludes` key and points to `car-go-clean config migrate`; v0.5 removes that
key.

Finally run the [fresh-install validation](fresh-install-validation.md) in a
disposable macOS or Linux environment. It must prove that binary installation
leaves the service absent, dry-run preserves a disposable Rust target, exact
review execution removes it, and format-v1 JSON/NDJSON exit codes match their
terminal envelopes. Do not use dynamic bare `run` outside that disposable
fixture.
