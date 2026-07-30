<p align="center">
  <img src="assets/car-go-clean-logo-readme.png" alt="car-go-clean crab logo" width="440">
</p>
<h1>car-go-clean</h1>

`car-go-clean` is a Rust CLI/daemon that finds Rust projects on disk, runs
`cargo clean`, and tracks how much space was reclaimed.

## Install

Before installing anything, determine whether car-go-clean already exists:

```sh
if command -v car-go-clean >/dev/null 2>&1
then
  car-go-clean version
  car-go-clean service status
else
  echo "car-go-clean is not installed"
fi
```

If that reports v0.2.0 or v0.3.0, stop here and use the state-preserving
two-phase [v0.4 upgrade helper](docs/releases/v0.4.0.md#v02v03-upgrade-helper).
Do not run `brew upgrade`, `brew install`, or the ordinary shell installer
first; replacing the binary would bypass the helper's old-service detection
and recovery.

For a fresh macOS or Linux installation only, use Homebrew:

```sh
brew install dcchuck/tap/car-go-clean
car-go-clean version
```

Or use the checksum-verifying shell installer. It writes to
`$HOME/.local/bin` by default but does not edit your shell's `PATH`, so make
that directory discoverable before using bare `car-go-clean` commands:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh | sh
export PATH="$HOME/.local/bin:$PATH"
hash -r 2>/dev/null || true
car-go-clean version
```

The installer supports macOS on Apple Silicon (`aarch64-apple-darwin`) and
Intel (`x86_64-apple-darwin`), plus Linux ARM64
(`aarch64-unknown-linux-musl`) and x86_64 (`x86_64-unknown-linux-musl`). It
downloads the matching release archive and its `.sha256` file, verifies the
SHA-256 checksum before replacing the binary, and does not require `sudo`.

By default it installs to `$HOME/.local/bin`. After a release such as `v0.4.0`
has been published, a fresh installation can pin it:

```sh
VERSION=0.4.0
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh \
  | sh -s -- --version "$VERSION" --install-dir "$HOME/.local/bin"
export PATH="$HOME/.local/bin:$PATH"
hash -r 2>/dev/null || true
car-go-clean version
```

Both fresh-install paths install only the binary and do not start the daemon.
If another existing version was detected, read the target release's upgrade
instructions before replacing it. Activate daemon management explicitly only
after installation.

## Quick Start

For the complete mental model—from discovery authority and service behavior
through upgrades and release gates—read the
[Owner’s v0.4 product tour](docs/v0.4-owner-tour.md).

Check the binary and service state before creating a review:

```sh
car-go-clean version
car-go-clean service status
car-go-clean health
```

If status reports `Running: yes`, decide whether to pause the daemon before
continuing. `service stop` is a persistent disable across login and reboot, so
run it only after intentionally approving that service-state change. Remember
that the service was running so you can restore it after the reviewed flow:

```sh
car-go-clean service stop
```

If status reports stopped or not installed, do not change service state. Once
no daemon is running, create the review:

```sh
car-go-clean run --dry-run --all
```

The dry run scans automatically, applies every safety gate, invokes no Cargo
command, and prints a numeric `Review ID`. It also records the exact policy
hash, discovery generation, target list, and filesystem identities that you
reviewed. Execute that exact plan by replacing `REVIEW_ID` with the printed
number:

```sh
car-go-clean run --review REVIEW_ID
car-go-clean stats
```

If you stopped an originally running service, restore it after the reviewed
run (or after deciding not to execute the review):

```sh
car-go-clean service start
```

Review plans expire after 30 minutes, and only the newest 20 are retained. A
plan is rejected if its policy or discovery generation changed. Immediately
before Cargo runs, car-go-clean revalidates every persisted target. That check
may remove a target that became unsafe; it can never add a target that was not
in the review. `--all` only expands dry-run display—it cannot be used for a
destructive run.

Bare `car-go-clean run` remains available, but it is dynamic and destructive:
it scans and accepts the fresh target set in one operation. Use it only when
you intentionally accept that behavior. The reviewed two-command flow above
is the recommended manual path.

`service stop` is persistent across login and reboot. `service start`
re-enables and starts the installed definition.

For advanced cached-only inspection, `car-go-clean run --dry-run --no-scan`
skips discovery. It does not make historical cache rows authoritative and
never bypasses policy, generation, scope, exclusion, identity, activity,
quiet-period, or managed-storage checks.

One-shot commands exit `0` for complete coverage, `2` for valid results with
incomplete discovery coverage, and `1` for failures. A broad macOS home scan
can legitimately return `2` when privacy-protected directories cannot be
read. That preview may still contain a valid review ID; inspect its incomplete
origins before deciding whether its bounded target set is acceptable. Exit
`1` always outranks `2`.

Inspect the cleanup authority in human-readable or machine-readable form:

```sh
car-go-clean health --json || test $? -eq 2
car-go-clean status --json || test $? -eq 2
```

Exit `2` still writes a valid report. With `--json`, every command ends with a
format-v1 JSON envelope. A cleanup execution uses NDJSON: each actual target
is emitted as a `target` event before the terminal envelope. The envelope
contains `outcome.code`, `outcome.kind`, explicit reasons, policy and
generation context, scan errors, and command data. Service-state probe or
environment-divergence warnings are reported, but do not change the cleanup
authority outcome.

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
> `service status` commands before choosing an installation path. Also inspect
> configuration, how often Rust projects are built, and whether the user wants
> one-shot cleanup or an always-on per-user daemon.
>
> If the existing binary is v0.2.0 or v0.3.0, stop the ordinary install path
> and follow the repository's v0.4 state-preserving upgrade-helper
> instructions. Do not run a plain `brew upgrade`, `brew install`, or ordinary
> shell installer first. Choose the helper method that owns the existing
> visible command, not a desired migration method; the helper will verify
> Homebrew formula ownership or a safe shell-owned binary before any service
> stop. Before invoking helper phase one, use the legacy `service status`
> result: if it is running, explain that phase one persistently stops and
> disables it, replaces the binary, and creates the helper preview that opens
> the bounded review window. Ask approval to enter that window and to restore
> the originally running service after reviewed execution or cancellation.
> Invoke helper phase one only after that approval. If the legacy service is
> already stopped or absent, make no service-state change outside the helper.
> Treat the helper's preview as the review window described below; inspect it
> and ask separately before invoking helper phase two with its exact review ID.
> Do not create a second bare-command preview during this helper flow.
> Cross-method migration requires a separate explicit uninstall and fresh
> install. For any other existing version, read the target release's upgrade
> instructions before replacement. Only when no binary is installed should you
> use the README's fresh-install Homebrew or verified shell-installer command.
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
> After completing the correct fresh-install or upgrade flow, verify the
> installed version and inspect service state before any preview:
>
> ```sh
> car-go-clean version
> car-go-clean service status
> car-go-clean health
> ```
>
> Outside the legacy upgrade-helper flow, if the daemon is running, do not
> preview yet. Explain that `service stop`
> persistently disables it across login and reboot, then ask approval for this
> bounded review window: stop it, create and inspect a preview, leave it stopped
> while asking separately for reviewed execution approval, and restore the
> originally running service after execution or cancellation. Only after that
> approval run `car-go-clean service stop`. If the service is already stopped
> or absent, make no service change. Once no daemon is running, run:
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
> authorized by this prompt, except that an active legacy service still
> requires the approval above before helper phase one persistently stops it.
> Ask before:
>
> - Executing the exact preview with either helper phase two or
>   `car-go-clean run --review REVIEW_ID`, as appropriate to the flow.
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
