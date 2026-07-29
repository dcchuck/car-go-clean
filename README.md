<p align="center">
  <img src="assets/car-go-clean-logo-readme.png" alt="car-go-clean crab logo" width="440">
</p>
<h1>car-go-clean</h1>

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

By default it installs to `$HOME/.local/bin`. After a release such as `v0.4.0`
has been published, pin it or choose another location when needed:

```sh
VERSION=0.4.0
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh \
  | sh -s -- --version "$VERSION" --install-dir "$HOME/.local/bin"
```

Both installation paths install or upgrade only the binary, and the installer does not start the daemon.
Activate daemon management explicitly after installation.

## Quick Start

Check the installation and preview every eligible cleanup target:

```sh
car-go-clean health
car-go-clean run --dry-run --all
```

`run` scans automatically before it reviews or cleans. The preview does not
invoke Cargo, and installation does not start the background service. The
default quiet period, active-process checks, scan-error checks, managed-storage
checks, and direct-target checks all remain in effect.

If the service is already active, stop it for the preview and resume it
afterward:

```sh
car-go-clean service stop
car-go-clean run --dry-run --all
car-go-clean service start
```

`service stop` preserves the installed service definition; `service start`
resumes it after you approve the preview.

After reviewing the preview:

```sh
car-go-clean run
car-go-clean stats
```

A real run has no interactive confirmation. For advanced cached-only use,
`car-go-clean run --no-scan` skips discovery but does not relax any safety
gate.

One-shot commands exit `0` for complete coverage, `2` for valid results with
incomplete discovery coverage, and `1` for failures. A macOS home scan can
legitimately return `2` when privacy-protected directories cannot be read.

## Agent Quick Start

Copy this prompt into your coding agent:

> Install and configure the latest stable release of `car-go-clean` from its
> canonical repository:
>
> https://github.com/dcchuck/car-go-clean
>
> Before acting, read the current README and latest release. Use only these
> official installation sources:
>
> - Homebrew formula: `dcchuck/tap/car-go-clean`
> - Checksum-verifying installer:
>   `https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh`
>
> Do not use a similarly named package from another repository or registry.
>
> First inspect this machine's operating system, architecture, available
> package manager, Cargo availability, existing `car-go-clean` installation,
> configuration, and service status. Recommend Homebrew or the verified shell
> installer and briefly explain why.
>
> Install or upgrade the binary, verify the installed version, and run:
>
> ```sh
> car-go-clean health
> car-go-clean run --dry-run --all
> ```
>
> Explain what would be cleaned, what would be skipped, and why. Then
> recommend either one-shot usage or the background service based on how this
> machine is used.
>
> Inspection, installation or upgrade, health checks, and the dry run are
> authorized by this prompt. Ask before:
>
> - Performing actual cleanup.
> - Installing or enabling the background service.
> - Changing configuration or exclusions.
> - Using `--force`, `--include-active`, or `--include-managed-cache`.
> - Cloning and building from source.
>
> Do not weaken safety checks, manually delete `target/` directories, or work
> around scan errors or process locks. Report blockers and final results
> clearly.

## Background Service (Optional)

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

- `scan_dirs` and `project_dirs` define cleanup discovery scope and must expand
  to absolute paths.
- Platform-aware defaults prune operating-system, package-manager, container,
  and VM storage before traversal; see the configuration reference for the
  exact macOS and Linux lists.
- `extra_excludes` is the normal way to add discovery exclusions.
- `override_excludes` is an advanced option that replaces editable discovery
  defaults; protected-storage cleanup gates remain independent.
- The v0.4 binary still accepts legacy `excludes` with a warning. Run
  `car-go-clean config migrate` before v0.5.
- Unknown keys, unset path variables, unterminated `${NAME` expressions, and
  an empty effective scope are configuration errors.
- Git-reported linked worktrees are discovered conservatively. A discovery
  failure blocks the affected primary/worktree set until a later success.
- Review before cleanup with `car-go-clean run --dry-run`.

See the [Configuration reference](docs/configuration.md) for the complete
safety, worktree, state, log, and scheduler behavior.

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

## Commands

| Command | Purpose |
| --- | --- |
| `car-go-clean daemon` | Long-running scheduler. |
| `car-go-clean scan` | Refresh the project cache. |
| `car-go-clean run` | Scan, then run one cleanup review/cycle now. |
| `car-go-clean health` | Validate config, Cargo availability, and state DB access. |
| `car-go-clean status` | Show cached project count, last saved safety review, scheduler timing, and last run summary. |
| `car-go-clean projects` | Refresh and summarize cached project cleanability decisions. |
| `car-go-clean stats` | Show recovered bytes and top projects. |
| `car-go-clean logs` | Tail logs or show recent stored errors. |
| `car-go-clean config` | Print effective config. |
| `car-go-clean version` | Print version. |

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

```bash
mise exec rust@1.95.0 -- cargo fmt -- --check
mise exec rust@1.95.0 -- cargo test
mise exec rust@1.95.0 -- cargo clippy --all-targets -- -D warnings
mise exec rust@1.95.0 -- cargo build
```

See [Fresh install validation](docs/fresh-install-validation.md) for the
source-checkout and released-binary smoke tests.

See `docs/superpowers/specs/` for the design.
