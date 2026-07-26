# Releasing car-go-clean

Releases are tag-driven GitHub Releases. Begin from a clean checkout of the
verified commit that will become `v0.2.0`; do not create a release from local
uncommitted work. Before tagging, ensure the public
[`dcchuck/homebrew-tap`](https://github.com/dcchuck/homebrew-tap) repository
exists and that this repository has the `HOMEBREW_TAP_TOKEN` Actions secret
from a fine-grained token with repository contents and pull-request write
permission limited to that tap. Store it without printing it:

```bash
printf %s "$HOMEBREW_TAP_TOKEN" | gh secret set HOMEBREW_TAP_TOKEN --repo dcchuck/car-go-clean
```

The release preflight fails before hosting if this secret is empty or absent.

Run the complete local verification suite:

```bash
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
mise exec rust@1.95.0 -- cargo test --locked
make test-installer
dist plan --tag v0.2.0 --output-format=json
git tag -a v0.2.0 -m "car-go-clean v0.2.0"
git push origin main v0.2.0
```

The release workflow accepts only an annotated `vX.Y.Z` tag whose version
matches `Cargo.toml`. The shell installer's `--version` option likewise accepts
only exactly three decimal components such as `0.2.0`, with no prefix, suffix,
fourth component, whitespace, or path characters.

The workflow first creates a GitHub draft containing four target archives
(`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-musl`, and `x86_64-unknown-linux-musl`), a matching
`.sha256` file for each archive, and provenance attestations. After that draft
exists, one publisher uploads `car-go-clean-installer.sh` to it while the tap
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
