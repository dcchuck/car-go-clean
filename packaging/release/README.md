# Release Packaging

Primary distribution channel: `cargo install`.

Build and install from a checked-out release tag:

```bash
cargo install --path .
```

Published crate installs should use the same binary name:

```bash
cargo install car-go-clean
```

Homebrew is the secondary channel once published artifacts have stable checksums.
Keep the service files in `packaging/systemd/` and `packaging/launchd/` in sync
with the installed binary path used by the chosen package manager.
