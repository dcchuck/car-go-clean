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
scan_dirs = ["~/code", "~/work"]
project_dirs = ["~/play/one-off-rust-project"]
clean_interval = "24h"
scan_interval = "168h"
log_level = "info"
```

## Commands

| Command | Purpose |
| --- | --- |
| `car-go-clean daemon` | Long-running scheduler. |
| `car-go-clean scan` | Refresh the project cache. |
| `car-go-clean run` | Run one clean cycle now. |
| `car-go-clean health` | Validate config, Cargo availability, and state DB access. |
| `car-go-clean status` | Show cached project count and last run summary. |
| `car-go-clean stats` | Show recovered bytes and top projects. |
| `car-go-clean logs` | Tail logs or show recent stored errors. |
| `car-go-clean config` | Print effective config. |
| `car-go-clean version` | Print version. |

State lives under `$XDG_STATE_HOME/car-go-clean`, falling back to
`$HOME/.local/state/car-go-clean`.

Daemon logs are newline-delimited JSON written to the state directory at
`car-go-clean.log`. Logs rotate automatically as `car-go-clean.log.1`,
`car-go-clean.log.2`, and so on.

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
