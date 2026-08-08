<p align="center">
  <img src="assets/car-go-clean-logo-readme.png" alt="car-go-clean crab logo" width="440">
</p>
<h1>car-go-clean</h1>

`car-go-clean` is a Rust CLI/daemon that finds Rust projects on disk, runs
`cargo clean`, and tracks how much space was reclaimed.

## Quick Start

Install with Homebrew:

```sh
brew install dcchuck/tap/car-go-clean
```

Or use the checksum-verifying installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh | sh
export PATH="$HOME/.local/bin:$PATH"
hash -r 2>/dev/null || true
```

The installer supports macOS and Linux on Apple Silicon/ARM64 and x86_64. It
downloads the matching release archive, verifies its SHA-256 checksum, and
does not require `sudo`.

Check the install, then create and review a cleanup plan:

```sh
car-go-clean version
car-go-clean run --dry-run --all

# After reviewing the preview:
car-go-clean run --review REVIEW_ID
car-go-clean stats
```

The dry run scans automatically, invokes no Cargo command, and prints the
numeric `REVIEW_ID`. Replace `REVIEW_ID` with that number. Reviews expire
after 30 minutes; immediately before cleanup, car-go-clean revalidates each
target and can remove newly unsafe targets, but never add one you did not
review.

Installation does not start the daemon. See [Background Service
(Optional)](#background-service-optional) if you want scheduled cleanup.

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
> Before any install or replacement, inspect this machine's operating system,
> architecture, available package manager, Cargo availability, and whether
> `car-go-clean` already exists. If it does, run its `version` and
> `service status` commands and read the target release's upgrade notes before
> replacing it. Also inspect configuration, how often Rust projects are built,
> and whether the user wants one-shot cleanup or an always-on per-user daemon.
>
> If the shell installer is selected, it does not edit `PATH`. After it
> succeeds, run:
>
> ```sh
> export PATH="$HOME/.local/bin:$PATH"
> hash -r 2>/dev/null || true
> command -v car-go-clean
> car-go-clean version
> ```
>
> Do not use later bare commands until `command -v` resolves the installed
> binary. Recommend one-shot or daemon operation based on the user's Rust
> usage and explain the choice.
>
> After installation or upgrade, verify the installed version and inspect
> service state before any preview:
>
> ```sh
> car-go-clean version
> car-go-clean service status
> car-go-clean health
> ```
>
> If the daemon is running, do not preview yet. Explain that `service stop`
> persistently disables it across login and reboot, then ask approval to stop
> it, create and inspect a preview, leave it stopped while separately asking
> to execute the review, and restore the originally running service after
> execution or cancellation. If the service is already stopped or absent, make
> no service change. Once no daemon is running, run:
>
> ```sh
> car-go-clean run --dry-run --all
> ```
>
> Record whether each command exits `0` (complete), `2` (valid but incomplete),
> or `1` (failed). Treat ordinary macOS privacy/TCC scan denials as incomplete,
> not success or failure; explain their origins. From the preview, capture the
> numeric review ID, policy hash, discovery generation, expiry, cleanable
> targets, skipped targets, and managed-storage decisions. Explain that the
> plan lasts 30 minutes, is one of at most 20 retained plans, and execution
> can remove newly unsafe targets but never add new ones.
>
> Inspection, installation or upgrade, health checks, and a preview are
> authorized by this prompt. Ask before:
>
> - Executing `car-go-clean run --review REVIEW_ID`.
> - Installing or enabling the background service.
> - Changing configuration or exclusions.
> - Using `--force`, `--include-active`, or `--include-managed-cache`.
> - Cloning and building from source.
>
> Do not weaken safety checks, manually delete `target/` directories, or work
> around scan errors or process locks. Never execute dynamic bare
> `car-go-clean run` on the user's behalf. If daemon operation is approved,
> use `car-go-clean service install`, re-check installed/enabled/running state,
> and explain any environment-recapture warning. If the service was running
> before the approved review window, restore it with `car-go-clean service
> start` after reviewed execution or cancellation, as already authorized by
> that window. Report blockers, exit codes, service state, and final results
> clearly.

## Background Service (Optional)

`car-go-clean` uses a per-user launchd service on macOS and a per-user systemd
service on Linux. Homebrew and the shell installer only install the binary;
they do not create, enable, or start a service. Manage it explicitly:

```sh
car-go-clean service install
car-go-clean service status
car-go-clean service stop
car-go-clean service refresh
car-go-clean service start
car-go-clean service restart
car-go-clean service uninstall
```

`service install` writes the per-user definition, captures the supported
manager/root environment used by cleanup policy, enables the definition, and
starts it. Status reports `Installed`, `Enabled`, and `Running` separately.
`stop` disables and stops persistently; `start` re-enables and starts.
`refresh` rewrites an existing definition with the current binary and stable
physical manager-root environment without enabling or starting it.
`uninstall` removes only the service definition and retains configuration,
state, logs, and cleanup history.

If the current shell resolves protected roots differently from the captured
service environment, `status` and `health` warn about the divergence. After
reviewing the new roots, stop the service and use `service refresh` to
recapture while leaving it disabled/stopped, or use `service install` only when
enabling and starting is also intended. Relative or otherwise ambiguous
manager-root overrides are rejected before policy hashing or definition
changes.

On Linux, a systemd user service may stop when the login session ends unless
login lingering is enabled. car-go-clean never enables it automatically. If
daemon operation without an active login is desired, opt in manually:

```sh
loginctl enable-linger "$USER"
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

- `scan_dirs` and `project_dirs` define cleanup discovery scope and must expand
  to absolute paths.
- Platform-aware defaults prune operating-system, package-manager, container,
  and VM storage before traversal; see the configuration reference for the
  exact macOS and Linux lists.
- `extra_excludes` is the normal way to add discovery exclusions.
- `override_excludes` is an advanced option that replaces editable discovery
  defaults; protected-storage cleanup gates remain independent.
- The v0.4 binary still accepts legacy `excludes` with a warning. Run
  `car-go-clean config migrate` before upgrading to v0.5, where the legacy key
  is removed.
- Unknown keys, unset path variables, unterminated `${NAME` expressions, and
  an empty effective scope are configuration errors.
- Git-reported linked worktrees are discovered conservatively. A discovery
  failure blocks the affected primary/worktree set until a later success.
- Review before cleanup with `car-go-clean run --dry-run --all`, then execute
  only the printed plan with `car-go-clean run --review REVIEW_ID`.

See the [Configuration reference](docs/configuration.md) for the complete
safety, worktree, state, log, and scheduler behavior.

## Safe Cleaning Model

By default, `car-go-clean` is safe against a broad `~` scan. It only runs
`cargo clean` for projects with current cleanup authority that pass all safety
gates:

- Cached projects are history, not authority. Cleanup requires a current
  discovery generation created under the exact current policy hash.
- `--no-scan` skips discovery only. It never bypasses generation, scope,
  exclusion, identity, activity, quiet-period, or protected-storage checks.
- `project/target` exists directly under the cached project path.
- The direct target directory can be read and measured.
- The newest non-symlink file under `target/` is at least
  `target_quiet_period` old.
- A managed cache or container target must first be admitted into discovery
  scope by configuration and must also receive explicit plan-time approval
  with `run --dry-run --include-managed-cache`. A reviewed run carries only
  that persisted approval; it has no flag that can expand the plan.
- No recent scan recorded a physically related unreadable ancestor or
  descendant path for the project.
- No running process has a cwd or command argument inside the project or
  `target/`.
- Project and target filesystem identities are checked during review and
  again immediately before Cargo runs. This narrows, but cannot eliminate,
  the residual time-of-check/time-of-use race.

A database migrated from an older path-only schema retains project and
recovery history but grants no current cleanup authority. A cached-only run
returns exit `2` until a successful scan creates a matching generation.
Optional default exclusion roots that are absent on a machine are normal and
do not make a scan incomplete.

## Commands

| Command | Purpose |
| --- | --- |
| `car-go-clean daemon` | Long-running scheduler. |
| `car-go-clean scan` | Refresh the project cache. |
| `car-go-clean run --dry-run --all` | Scan and persist a complete displayed review without cleaning. |
| `car-go-clean run --review ID` | Execute only the persisted, still-safe targets in one review. |
| `car-go-clean run` | Dynamically scan and clean a fresh target set; destructive and intentionally unreviewed. |
| `car-go-clean health` | Validate config, Cargo availability, and state DB access; add `--json` for authority diagnostics. |
| `car-go-clean status` | Show authority, cache, review, scheduler, and recovery state; add `--json` for the shared diagnostic shape. |
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
cargo run -- run --dry-run --all
cargo run -- run --review REVIEW_ID
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
