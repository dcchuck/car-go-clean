# Releasing car-go-clean

Releases are tag-driven GitHub Releases. Begin from a clean checkout of the
verified commit that will become `v0.2.0`; do not create a release from local
uncommitted work. Before tagging, ensure the public
[`dcchuck/homebrew-tap`](https://github.com/dcchuck/homebrew-tap) repository
exists and that this repository has the `HOMEBREW_TAP_TOKEN` Actions secret
with permission to update its formula.

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
matches `Cargo.toml`. It publishes four target archives
(`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-musl`, and `x86_64-unknown-linux-musl`), a matching
`.sha256` file for each archive, provenance attestations, the
`car-go-clean-installer.sh` asset, and the Homebrew formula. It does not
publish to crates.io or enable any daemon.

After GitHub has published the release, the
[release verification workflow](https://github.com/dcchuck/car-go-clean/blob/main/.github/workflows/release-verify.yml)
downloads each archive, verifies its checksum, smoke-tests the binary, and
audits the public tap formula. Investigate any failed post-publication check
before announcing the release.
