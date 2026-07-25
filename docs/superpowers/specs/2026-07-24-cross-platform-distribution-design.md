# Cross-Platform Distribution Design

## Status

Approved for planning on 2026-07-24.

## Goal

Make `car-go-clean` easy to install and upgrade on public macOS and Linux
systems without requiring Rust. Releases must provide the same verified binary
to Homebrew and direct shell installation. Daemon installation remains an
explicit user action.

## Scope

- Public GitHub Release artifacts for version tags.
- macOS Apple Silicon and Intel support.
- Linux x86_64 and ARM64 support.
- A public `dcchuck/homebrew-tap` formula.
- A checksum-verifying shell installer.
- Cross-platform, explicit user-service management in the CLI.

## Non-goals

- Publishing to crates.io, Homebrew core, apt, yum, deb, rpm, Nix, or Snap.
- Requiring `sudo`.
- Auto-enabling, auto-starting, or auto-updating the daemon when a binary is
  installed or upgraded.
- Windows support in this release.

## Release Contract

`Cargo.toml` remains the version source of truth. A release begins only when a
maintainer pushes an annotated SemVer tag named `vX.Y.Z` whose version exactly
matches `Cargo.toml`. Ordinary `main` pushes do not publish releases.

Cargo-dist provides the GitHub Actions release-artifact pipeline. The release
matrix produces one archive per target:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `aarch64-unknown-linux-musl`
- `x86_64-unknown-linux-musl`

The bundled SQLite dependency permits self-contained Linux artifacts. Each
release contains the target archives, their cargo-dist-generated `.sha256`
checksum assets, and GitHub artifact provenance attestations. The release job
verifies locked tests, Clippy with warnings denied, and an extracted-binary
smoke check before publication.

## User Installation

### Homebrew

The public tap repository is `dcchuck/homebrew-tap`. It contains
`Formula/car-go-clean.rb`, which downloads the matching public GitHub Release
asset and pins its SHA-256.

```sh
brew install dcchuck/tap/car-go-clean
brew upgrade car-go-clean
```

The release workflow opens or updates a formula-bump pull request in the tap
using a fine-grained `HOMEBREW_TAP_TOKEN` secret that can write only that
repository. The formula installs the binary only; it does not define or start
a Homebrew service.

### Shell Installer

The public installer is served from the release and is invoked with a pinned
transport policy:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh \
  | sh
```

It detects the supported OS and CPU, downloads the corresponding archive and
its cargo-dist-generated `.sha256` asset, verifies the checksum, and atomically
installs the binary to `~/.local/bin` by default. It supports `--version X.Y.Z` and
`--install-dir PATH`. It never uses `sudo`, touches config/state, or starts a
daemon. Unsupported platforms and checksum failures terminate before changing
the installed binary.

Running the installer again upgrades only the binary. Users restart an
explicitly installed daemon with `car-go-clean service restart` when they want
the running process to use the new executable.

## Explicit Service Management

The CLI gains a `service` command group:

- `car-go-clean service install`
- `car-go-clean service status`
- `car-go-clean service restart`
- `car-go-clean service uninstall`

`service install` is the explicit opt-in point: it renders a user-service file
with the absolute path to the installed binary, enables it, and starts it.

On macOS it renders the existing launchd template into
`~/Library/LaunchAgents/com.dcchuck.car-go-clean.plist`, then bootstraps and
kickstarts the user agent. On Linux it renders a systemd user unit under
`~/.config/systemd/user/`, runs `systemctl --user daemon-reload`, and enables
and starts the unit. The Linux command fails clearly when `systemd --user` is
unavailable rather than silently installing an alternative scheduler.

`service uninstall` stops and removes only car-go-clean's user service file.
`status` reports the platform service state and the resolved binary path.
Existing launchd and systemd templates become implementation inputs for one
shared rendering path rather than separate installer scripts with divergent
behavior.

## Safety and Upgrade Behavior

Config and state remain in their current XDG locations. Neither the installer
nor the service commands rewrite scan roots, exclusions, or retained database
state. The service always invokes the installed binary by absolute path,
avoiding shell `PATH` changes after an upgrade.

The installer replaces the binary only after archive and checksum verification
succeed. A failed download, unsupported target, or failed verification leaves
the prior binary untouched. Service management is separate, so installing or
upgrading the CLI cannot start background cleanup without the user's explicit
`service install` action.

## Validation

Pull-request CI continues to run locked tests, formatting, and strict Clippy.
Tag-release CI additionally validates:

- Version/tag consistency.
- All four archives and their matching SHA-256 assets.
- Extracted-binary `version` and `health --skip-cargo` smoke checks.
- Shell-installer target selection, checksum rejection, and atomic replacement.
- launchd and systemd rendering using an absolute binary path.
- `service install`, `status`, `restart`, and `uninstall` unit/integration
  behavior behind platform adapters.
- Homebrew formula syntax and checksum alignment with the published archive.

## Documentation

The README leads with Homebrew and shell-install commands, keeps Cargo install
as a developer option, and documents explicit daemon activation separately.
Release documentation explains tag publication, the public tap, supported
targets, checksum verification, and the restart requirement after a binary
upgrade.
