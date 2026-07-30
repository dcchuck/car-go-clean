# Release Packaging

Released binaries are published only from annotated `vX.Y.Z` Git tags to the
GitHub Release for `dcchuck/car-go-clean`. The release is distributed through
the public Homebrew tap, `dcchuck/homebrew-tap`, and the HTTPS shell installer
at `car-go-clean-installer.sh`.

Each release includes these archives and a matching per-archive SHA-256
checksum asset:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `aarch64-unknown-linux-musl`
- `x86_64-unknown-linux-musl`

For example, the Apple Silicon archive is
`car-go-clean-aarch64-apple-darwin.tar.xz` and its checksum file is
`car-go-clean-aarch64-apple-darwin.tar.xz.sha256`. The shell installer chooses
the appropriate target, verifies that checksum, and atomically replaces only
the binary. Homebrew uses the same published release artifacts.

Neither binary installation path enables or starts the daemon. Users opt in
with `car-go-clean service install`, and restart an existing service after an
upgrade with `car-go-clean service restart`.

The separate v0.4 state-preserving helper upgrades existing v0.2/v0.3
installations only through their current owner. Its `--method homebrew|shell`
argument must match the visible command's verified origin; it is not a
cross-method migration option. The helper persists the exact replacement
binary path in its private two-phase session and never substitutes a later
`PATH` resolution for config, preview, or reviewed execution.

Cargo remains the source/developer install path for a checked-out repository:

```bash
cargo install --path .
```
