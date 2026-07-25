<p align="center">
  <img src="assets/car-go-clean-logo.png" alt="car-go-clean crab logo" width="640">
</p>

# car-go-clean

`car-go-clean` is a Rust CLI/daemon that finds Rust projects on disk, runs
`cargo clean`, and tracks how much space was reclaimed.

## Install

On macOS or Linux, install the released binary with Homebrew:

```sh
brew install dcchuck/tap/car-go-clean
brew upgrade car-go-clean
```

Or use the checksum-verifying shell installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh | sh
```

The installer supports macOS on Apple Silicon (`aarch64-apple-darwin`) and
Intel (`x86_64-apple-darwin`), plus Linux ARM64
(`aarch64-unknown-linux-musl`) and x86_64 (`x86_64-unknown-linux-musl`). It
downloads the matching release archive and its `.sha256` file, verifies the
SHA-256 checksum before replacing the binary, and does not require `sudo`.

By default it installs to `$HOME/.local/bin`. After a release such as `v0.2.0`
has been published, pin it or choose another location when needed:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh \
  | sh -s -- --version 0.2.0 --install-dir "$HOME/.local/bin"
```

Both installation paths install or upgrade only the binary; neither path starts
the daemon. The binary installer does not start the daemon. Activate daemon
management explicitly after installation.

## Explicit Service Activation

`car-go-clean` uses a per-user launchd service on macOS and a per-user systemd
service on Linux. Installation does not enable either service. Manage it only
when you choose to:

```sh
car-go-clean service install
car-go-clean service status
car-go-clean service restart
car-go-clean service uninstall
```

After upgrading a binary, run `car-go-clean service restart` if you have
already installed the service and want the daemon to use the new binary.

## Developer Installation

```bash
cargo install --path .
```

Or run from the repository:

```bash
cargo run -- scan
cargo run -- run
cargo run -- stats
```

This checkout also works with the local mise toolchain:

```bash
mise exec rust@1.95.0 -- cargo test
```

## Configuration

Config is optional. If no file exists, the tool scans `$HOME`.

Default config path:

```text
$XDG_CONFIG_HOME/car-go-clean/config.toml
# or
$HOME/.config/car-go-clean/config.toml
```

Example:

```toml
# ~/.config/car-go-clean/config.toml
scan_dirs = ["~"]
target_quiet_period = "2h"
clean_interval = "24h"
scan_interval = "1d"
```

The default scan interval is one day. When a scan finds a primary Git checkout,
it also discovers linked Rust worktrees that Git reports within the configured
scan roots. This includes Git-reported worktrees hidden by ignore rules;
configured exclusions and the normal cleaning safeguards still apply. A
successful enumeration also removes previously cached Git candidates that are
now excluded or outside the configured scan roots. Explicitly configured
`project_dirs` may authorize projects outside the scan roots, but never
override configured exclusions.

If Git worktree discovery fails for a primary checkout, the failure is recorded
as an ordinary scan error, which retains the usual recent, hierarchical
scan-error behavior. Independently, the canonical primary and exactly the
linked worktrees saved for it remain blocked until a later successful discovery
replaces the association. That persistent block normally does not extend to
ancestors, siblings, or other projects. New failures retain the primary's
canonical identity at failure time, so a primary alias changing targets cannot
transfer or clear the old failure. Successful linked-worktree associations also
retain the canonical primary identity, and their persisted primary identity is
not rewritten from a later filesystem target. Saved linked-path spellings are
likewise immutable outside a successful enumeration; ordinary cache
canonicalization only moves project review rows. Migrated primary aliases in
either failure or association state are trusted only when the persisted primary
was already canonical; unresolvable or noncanonical legacy associations are
never inferred from their current target or discarded merely because a new
checkout reuses the same path spelling. While any discovery failure is active,
such untrusted legacy association state blocks all cached projects. As another
conservative fallback, if a saved linked identity is unresolvable or no longer
canonical, all cached projects are temporarily blocked until successful
discovery safely replaces the association. Discovery-error diagnostics remain
in history, but a successful enumeration resolves their safety effect for that
primary immediately; unrelated ordinary scan errors remain effective. An
explicit forced run still bypasses either durable block.

## Safe Cleaning Model

By default, `car-go-clean` is safe against a broad `~` scan. It only runs
`cargo clean` for cached projects that pass all safety gates:

- `project/target` exists directly under the cached project path.
- The direct target directory can be read and measured.
- The newest non-symlink file under `target/` is at least
  `target_quiet_period` old.
- The project is not under a known managed cache or container storage path.
- No recent scan recorded a physically related unreadable ancestor or
  descendant path for the project.
- No running process has a cwd or command argument inside the project or
  `target/`.

Cached project rows are canonicalized to their current physical locations
before these gates run, even when immutable discovery provenance retains an
older primary spelling. This keeps cache/container classification physical
without transferring or clearing historical worktree associations.

On Unix, Rust compiler path options are parsed as native OS bytes, so non-UTF-8
path suffixes in `--manifest-path`, `--target-dir`, `--out-dir`, `--extern`,
`--emit`, `-L`, and `--library-path` still protect the matching canonical
project.

The default `target_quiet_period` is `2h`.

Use these commands to review or override the default policy:

- `car-go-clean run --dry-run` refreshes the safety review, prints a compact
  summary and target preview, and does not delete any `target/` directories.
- `car-go-clean run --dry-run --all` prints every cleanable target path.
- `car-go-clean run --include-managed-cache` includes known managed cache and
  container storage paths in the review policy.
- `car-go-clean run --include-active` includes projects with active process
  matches in the review policy.
- `car-go-clean run --force` bypasses policy gates except the direct,
  readable `project/target` requirement.
- `car-go-clean status` reports the last saved safety review without doing a
  live filesystem/process review.
- `car-go-clean status --refresh` recomputes and saves the safety review.
- `car-go-clean projects` refreshes the review and prints a compact summary.
- `car-go-clean projects --all` prints every cached project decision.
- `car-go-clean projects --risky` previews decisions with managed cache and
  container storage paths included.
- `car-go-clean projects --active` previews decisions with active process paths
  included.
- `car-go-clean projects --json` emits structured project review data.
- `car-go-clean logs --errors-only` shows scan, review, and clean diagnostics,
  including unreadable directories.

## Commands

| Command | Purpose |
| --- | --- |
| `car-go-clean daemon` | Long-running scheduler. |
| `car-go-clean scan` | Refresh the project cache. |
| `car-go-clean run` | Run one clean cycle now. |
| `car-go-clean health` | Validate config, Cargo availability, and state DB access. |
| `car-go-clean status` | Show cached project count, last saved safety review, scheduler timing, and last run summary. |
| `car-go-clean projects` | Refresh and summarize cached project cleanability decisions. |
| `car-go-clean stats` | Show recovered bytes and top projects. |
| `car-go-clean logs` | Tail logs or show recent stored errors. |
| `car-go-clean config` | Print effective config. |
| `car-go-clean version` | Print version. |

State lives under `$XDG_STATE_HOME/car-go-clean`, falling back to
`$HOME/.local/state/car-go-clean`.

Daemon logs are newline-delimited JSON written to the state directory at
`car-go-clean.log`. Logs rotate automatically as `car-go-clean.log.1`,
`car-go-clean.log.2`, and so on.
Unreadable directories are skipped during scans and recorded as scan errors;
view them with `car-go-clean logs --errors-only`.

## Fresh Install Validation

```bash
mise exec rust@1.95.0 -- cargo install --path . --force
car-go-clean health --skip-cargo
car-go-clean scan
car-go-clean status
car-go-clean run --dry-run
car-go-clean status
car-go-clean projects
car-go-clean projects --all
car-go-clean projects --json > /tmp/car-go-clean-projects.json
car-go-clean logs --errors-only
```

Validation points:

- `status` should be fast; before the first review it reports `Last review:
  <none>`, and after `run --dry-run` it reports the saved review summary.
- `status` should show `Clean interval`, `Scan interval`, and the next
  scheduled clean/scan time once the daemon has recorded scheduler state.
- `projects` should show a compact summary by default.
- `projects --all` should show why each cached project is cleanable or skipped.
- Unreadable directories such as protected macOS library folders should appear
  in `logs --errors-only`.
- `run --dry-run` should list a cleanable target preview and should not delete
  any `target/` directories.
- `run --dry-run --all` should list every cleanable target.
- A real `run` should clean only rows reported as `cleanable` by the same
  review policy.

## Services And Packaging

The release binary does not create or start a background daemon. Use the
explicit `car-go-clean service` commands above to manage the per-user launchd
or systemd service. Service templates live in `packaging/systemd/` and
`packaging/launchd/`; release packaging notes live in `packaging/release/`.

The daemon persists its next clean and scan times in the state database. On
restart, it resumes from that stored schedule instead of waiting a full interval
from process start. If no scheduler state exists yet, the daemon initializes the
next clean from the last completed run plus `clean_interval`, or from startup
plus `clean_interval` when there has never been a run.

## Development

```bash
mise exec rust@1.95.0 -- cargo fmt -- --check
mise exec rust@1.95.0 -- cargo test
mise exec rust@1.95.0 -- cargo clippy --all-targets -- -D warnings
mise exec rust@1.95.0 -- cargo build
```

See `docs/superpowers/specs/` for the design.
