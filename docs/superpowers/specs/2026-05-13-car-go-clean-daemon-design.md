# car-go-clean - Rust Daemon for Reclaiming Rust Build Artifacts

**Date:** 2026-05-13
**Status:** Implemented baseline

## Problem

Rust `target/` directories accumulate large build artifacts across workspaces
and one-off projects. Users who keep many Rust projects on disk need a small
local tool that discovers those projects, periodically runs `cargo clean`, and
tracks how much space was reclaimed.

## Language Choice

`car-go-clean` is implemented in Rust. The tool targets Rust developers, works
with Cargo projects, and is naturally distributed through `cargo install` as a
single binary. Rust also gives the daemon a direct ecosystem fit for this
domain: `clap` for CLI parsing, `serde`/`toml` for configuration, `rusqlite`
for local state, and straightforward process/filesystem APIs for invoking
Cargo and measuring `target/` directories.

## Goals

- Discover Rust projects under configured roots by finding `Cargo.toml`.
- Add explicit project directories without scanning their parents.
- Periodically run `cargo clean` for cached projects.
- Skip projects without `target/` instead of recording noisy empty events.
- Remove cached projects that no longer exist or no longer contain
  `Cargo.toml`.
- Persist projects, clean events, runs, and recent errors in SQLite.
- Expose operational commands for health, status, stats, logs, scan, run,
  daemon, config, and version.

## Non-Goals

- Cleaning non-Cargo build systems.
- Watching filesystem events.
- Running a system-wide daemon.
- Exposing an HTTP/RPC daemon API.
- Deleting source files or editing Rust projects.

## Architecture

The repository builds one Rust binary, `car-go-clean`, backed by a library crate
with focused modules:

- `config`: loads TOML config, overlays defaults, expands `~` and environment
  variables, validates structural settings, and computes XDG config/state paths.
- `scanner`: recursively discovers directories that directly contain
  `Cargo.toml`, skipping excluded directories and never descending into a
  discovered project.
- `store`: owns the SQLite schema and typed accessors for projects, runs,
  clean events, errors, and aggregation queries.
- `cache`: verifies cached projects against the current filesystem and removes
  dead entries.
- `cleaner`: measures `target/`, invokes `cargo clean`, captures stderr/exit
  code, and measures reclaimed bytes.
- `daemon`: coordinates scan and clean cycles over the store/cache/scanner/
  cleaner components.
- `lockfile`: uses an advisory file lock so only one mutating command runs at a
  time.
- `cli`: wires user-facing subcommands with `clap`.

Read-only commands open the state DB directly. Mutating commands (`scan`,
`run`, and `daemon`) acquire the lock at
`$XDG_STATE_HOME/car-go-clean/daemon.lock` or
`$HOME/.local/state/car-go-clean/daemon.lock`.

## Configuration

Default config requires no file:

| Key | Default | Notes |
| --- | --- | --- |
| `scan_dirs` | `[$HOME]` | Recursively scanned for `Cargo.toml`. |
| `project_dirs` | `[]` | Treated as direct project roots. |
| `excludes` | `.git`, `node_modules`, `.cargo`, `.rustup`, `target`, `Library/Caches` | Directory names skipped during scan. |
| `clean_interval` | `24h` | Daemon clean loop sleep interval. |
| `scan_interval` | `168h` | Daemon rescan interval. |
| `log_level` | `info` | Validated structurally for future logging expansion. |

Config is read from `$XDG_CONFIG_HOME/car-go-clean/config.toml`, falling back to
`$HOME/.config/car-go-clean/config.toml`.

## State

SQLite state is stored at `$XDG_STATE_HOME/car-go-clean/state.db`, falling back
to `$HOME/.local/state/car-go-clean/state.db`.

Tables:

- `projects(path, discovered_at, last_seen_at, last_cleaned_at)`
- `runs(id, started_at, finished_at, projects_cleaned, bytes_recovered, errors_count)`
- `clean_events(id, run_id, ts, path, bytes_before, bytes_after, duration_ms, exit_code, stderr_excerpt)`
- `errors(id, ts, category, path, message)`
- `schema_version(version)`

Timestamps are stored as Unix seconds for simple, portable SQLite access.

## Commands

- `car-go-clean daemon`: long-running scheduler.
- `car-go-clean scan`: refresh project cache.
- `car-go-clean run`: run one clean cycle now.
- `car-go-clean health`: validate config, cargo availability, state DB access,
  and recent errors.
- `car-go-clean status`: show cached project count and last run summary.
- `car-go-clean stats`: show total bytes recovered and top projects.
- `car-go-clean logs`: tail the daemon log file or print recent DB errors.
- `car-go-clean config`: print effective TOML config.
- `car-go-clean version`: print package version.

## Testing

The baseline implementation is covered by Rust integration tests:

- Config defaults, file overlay, expansion, validation, and XDG path selection.
- Scanner traversal, excludes, direct project dirs, and project-subtree pruning.
- SQLite migrations, project upserts, run/event/error persistence, and stats
  aggregation.
- Cache verification and stale-project removal.
- Cleaner byte measurement, fake runner invocation, and no-target skip.
- Daemon scan and clean cycles.
- CLI version, health, and scan-run-stats smoke flow with a fake `cargo`.
