# car-go-clean

`car-go-clean` is a Rust CLI/daemon that finds Rust projects on disk, runs
`cargo clean`, and tracks how much space was reclaimed.

## Install From Source

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
scan_interval = "7d"
```

## Safe Cleaning Model

By default, `car-go-clean` is safe against a broad `~` scan. It only runs
`cargo clean` for cached projects that pass all safety gates:

- `project/target` exists directly under the cached project path.
- The direct target directory can be read and measured.
- The newest non-symlink file under `target/` is at least
  `target_quiet_period` old.
- The project is not under a known managed cache or container storage path.
- The latest scan did not record a related unreadable path for the project.
- No running process has a cwd or command argument inside the project or
  `target/`.

The default `target_quiet_period` is `2h`.

Use these commands to review or override the default policy:

- `car-go-clean run --dry-run` prints the clean plan without deleting any
  `target/` directories.
- `car-go-clean run --include-managed-cache` includes known managed cache and
  container storage paths in the review policy.
- `car-go-clean run --include-active` includes projects with active process
  matches in the review policy.
- `car-go-clean run --force` bypasses policy gates except the direct,
  readable `project/target` requirement.
- `car-go-clean projects` lists cached projects and decisions.
- `car-go-clean projects --risky` previews decisions with managed cache and
  container storage paths included.
- `car-go-clean projects --active` previews decisions with active process paths
  included.
- `car-go-clean projects --json` emits structured project review data.
- `car-go-clean logs --errors-only` shows scan and clean diagnostics, including
  unreadable directories.

## Commands

| Command | Purpose |
| --- | --- |
| `car-go-clean daemon` | Long-running scheduler. |
| `car-go-clean scan` | Refresh the project cache. |
| `car-go-clean run` | Run one clean cycle now. |
| `car-go-clean health` | Validate config, Cargo availability, and state DB access. |
| `car-go-clean status` | Show safe cleaning summary and last run summary. |
| `car-go-clean projects` | Show cached projects with cleanable/skipped decisions. |
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
car-go-clean projects | head -50
car-go-clean projects --json > /tmp/car-go-clean-projects.json
car-go-clean run --dry-run
car-go-clean logs --errors-only
```

Validation points:

- `status` should show cached project count, cleanable project count, skipped
  project count, and cleanable bytes.
- `projects` should show why each cached project is cleanable or skipped.
- Unreadable directories such as protected macOS library folders should appear
  in `logs --errors-only`.
- `run --dry-run` should not delete any `target/` directories.
- A real `run` should clean only rows reported as `cleanable` by the same
  review policy.

## Services And Packaging

Service templates live in `packaging/systemd/` and `packaging/launchd/`.
Release notes and distribution-channel decisions live in `packaging/release/`;
the primary install path is currently `cargo install`.

## Development

```bash
mise exec rust@1.95.0 -- cargo fmt -- --check
mise exec rust@1.95.0 -- cargo test
mise exec rust@1.95.0 -- cargo clippy --all-targets -- -D warnings
mise exec rust@1.95.0 -- cargo build
```

See `docs/superpowers/specs/` for the design.
