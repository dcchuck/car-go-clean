# car-go-clean — Daemon for Reclaiming Disk Space from Rust Projects

**Date:** 2026-05-13
**Status:** Draft (design)

## Problem

Rust's `target/` directories accumulate gigabytes of build artifacts. Users
who keep many Rust projects on disk want their build caches periodically
cleared without having to remember which projects exist or where they live.

## Goals

- Discover every Rust project on the user's machine that the user has
  pointed the tool at.
- Periodically (default daily) run `cargo clean` against each discovered
  project.
- Track disk space recovered over time.
- Surface its own health (configuration validity, recent errors) on demand.
- Be installable via Homebrew (with `brew services` integration) or `go install`.
- Be operable with no configuration on first run.

## Non-Goals

- Cleaning artifacts of other build systems (npm, Bazel, etc.).
- Triggering on filesystem events. Time-based scheduling only.
- A daemon-side HTTP/RPC API. CLI subcommands read state from disk.
- Centralized/remote reporting. State stays local.
- Cross-user / system-wide daemon. Each user runs their own.

## High-Level Architecture

A single Go binary, `car-go-clean`, with multiple subcommands. One of those
subcommands (`daemon`) is the long-running process that schedulers
(`launchd` on macOS, `systemd` on Linux) invoke. All other subcommands are
short-lived CLI utilities that read shared on-disk state (SQLite + config).

```
                         ┌──────────────────────────┐
launchd / systemd ──────►│ car-go-clean daemon      │
                         │  ├─ scheduler (tickers)  │
                         │  ├─ scanner              │
                         │  ├─ cleaner              │
                         │  └─ store (SQLite)       │
                         └──────────┬───────────────┘
                                    │ reads/writes
                                    ▼
                  ~/.local/state/car-go-clean/state.db
                                    ▲
                                    │ reads
                         ┌──────────┴───────────────┐
                         │ car-go-clean {health,    │
                         │   status, stats, logs,   │
                         │   scan, run, config}     │
                         └──────────────────────────┘
```

No inter-process communication is required: the daemon writes its progress
into the SQLite store, and human-facing subcommands read from it. This keeps
the CLI snappy and the daemon failure-isolated.

## Components

### `internal/config` — load and validate user configuration

- Reads `~/.config/car-go-clean/config.toml` if present.
- Returns a fully-defaulted `Config` struct even if no file exists.
- Validates configured paths but does not require them to exist (existence
  is a `health` concern, not a load concern, so the daemon can still start
  with a partially valid config and report the issue).
- Resolves `~` and environment variables in paths.

Fields:

| Key              | Type       | Default       | Notes                             |
|------------------|------------|---------------|-----------------------------------|
| `scan_dirs`      | `[]string` | `[$HOME]`     | Walked recursively for `Cargo.toml`. |
| `project_dirs`   | `[]string` | `[]`          | Treated as project roots; not scanned. |
| `excludes`       | `[]string` | sensible set¹ | Path-substring matches skipped during scan. |
| `clean_interval`| duration   | `24h`         | Time between clean cycles.        |
| `scan_interval` | duration   | `168h`        | Time between full filesystem scans. |
| `log_level`      | string     | `info`        | `debug` / `info` / `warn` / `error`. |

¹ Default excludes: `.git`, `node_modules`, `.cargo`, `target` (we never
descend into a `target` directory while scanning), `.rustup`, `Library/Caches`,
common cloud-sync caches.

### `internal/scanner` — find Rust projects

- Walks each `scan_dir` recursively.
- When a directory contains a `Cargo.toml`, it is recorded as a project
  root and the scanner does **not** descend further. This collapses
  workspace members into their workspace root naturally: `cargo clean` at
  the parent suffices.
- Skips directories matching `excludes`.
- Adds each path in `project_dirs` directly without scanning.
- Returns a stream of project roots (channel) so cache updates can happen
  incrementally for large filesystems.

Pure I/O surface is behind an `FS` interface so unit tests can use
in-memory trees.

### `internal/cache` — persistent project list

- Backed by the `projects` table in SQLite.
- Operations: `Upsert(path)`, `MarkSeen(path)`, `Remove(path)`, `All()`,
  `Verify(path)` (returns true if path exists and still contains a
  `Cargo.toml`).
- On the start of each clean cycle, the daemon calls `Verify` on every
  cached entry and removes the dead ones. This satisfies the spec's
  requirement that the cache is maintained as directories are deleted.

### `internal/cleaner` — run `cargo clean`, measure delta

- Inputs: a project path.
- Stats `target/` size (sum of file sizes, follows no symlinks).
- Runs `cargo clean` in the project directory with a configurable timeout
  (default 10 min per project) and captures stderr.
- Stats `target/` size again (typically zero).
- Returns a `CleanResult { Path, BytesBefore, BytesAfter, Duration,
  ExitCode, StderrExcerpt, Err }`.
- Cargo invocation is behind a `CommandRunner` interface; tests swap in a
  fake that records calls and returns canned results.
- Projects whose `target/` doesn't exist are short-circuited (nothing to
  do, no event recorded).

### `internal/store` — SQLite persistence

Schema:

```sql
CREATE TABLE projects (
  path             TEXT    PRIMARY KEY,
  discovered_at    TIMESTAMP NOT NULL,
  last_seen_at     TIMESTAMP NOT NULL,
  last_cleaned_at  TIMESTAMP
);

CREATE TABLE clean_events (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id          INTEGER NOT NULL REFERENCES runs(id),
  ts              TIMESTAMP NOT NULL,
  path            TEXT NOT NULL,
  bytes_before    INTEGER NOT NULL,
  bytes_after     INTEGER NOT NULL,
  duration_ms     INTEGER NOT NULL,
  exit_code       INTEGER NOT NULL,
  stderr_excerpt  TEXT
);

CREATE TABLE errors (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  ts        TIMESTAMP NOT NULL,
  category  TEXT NOT NULL, -- 'scan' | 'clean' | 'config' | 'cache'
  path      TEXT,
  message   TEXT NOT NULL
);

CREATE TABLE runs (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  started_at        TIMESTAMP NOT NULL,
  finished_at       TIMESTAMP,
  projects_cleaned  INTEGER  NOT NULL DEFAULT 0,
  bytes_recovered   INTEGER  NOT NULL DEFAULT 0,
  errors_count      INTEGER  NOT NULL DEFAULT 0
);

CREATE INDEX idx_clean_events_ts ON clean_events(ts);
CREATE INDEX idx_errors_ts ON errors(ts);
CREATE INDEX idx_runs_started_at ON runs(started_at);
```

- Uses `modernc.org/sqlite` (pure-Go SQLite driver, no CGO).
- All writes happen on the daemon side. The CLI commands open the DB
  read-only.
- Migrations applied automatically on startup using a tiny embedded
  versioning helper (one bump per change).

### `internal/daemon` — main loop

Pseudocode:

```
loadConfig()
openStore()
ticker_clean := every clean_interval
ticker_scan  := every scan_interval
if cache empty: scan immediately

select {
  case <-ticker_scan: runScan()
  case <-ticker_clean: runCleanCycle()
  case <-ctx.Done(): graceful shutdown
}
```

- `runScan`: invoke scanner, upsert each found path, mark scan complete.
- `runCleanCycle`: verify each cached path, remove dead ones, iterate
  surviving paths and clean each. Open one `runs` row at start, update
  totals as events complete, close it at end.
- Errors are caught, logged, and persisted to `errors`; the cycle does not
  abort.
- The daemon clock is injectable so tests can fast-forward time.

### `internal/logging` — structured logs

- `slog` (stdlib) writing to both stderr (captured by launchd/systemd) and
  to a rotating file at `~/.local/state/car-go-clean/car-go-clean.log` via
  `lumberjack`.
- Error-level entries are mirrored into the `errors` table by the daemon.

### `cmd/car-go-clean` — CLI dispatch

Subcommands:

| Command                | Purpose                                              |
|------------------------|------------------------------------------------------|
| `daemon`               | Long-running process (invoked by service manager).   |
| `health`               | Validate config, check `cargo` on PATH, surface recent errors. Exits non-zero if unhealthy. |
| `status`               | Last run time, next scheduled run, cached project count, total bytes recovered. |
| `stats [--since 7d] [--json]` | Disk recovered per period, per project; top-N projects. |
| `scan`                 | Trigger one-shot scan synchronously (writes cache). |
| `run`                  | Trigger one clean cycle synchronously.              |
| `config`               | Print effective config (defaults merged).            |
| `logs [--errors-only] [--tail N]` | Tail the log file / show errors from DB.  |
| `version`              | Print build version + commit.                        |

`scan` and `run` are usable standalone (without the daemon) — they share
the same code paths the daemon uses, so the user can do everything
manually if they prefer.

## Data Flow — A Clean Cycle

1. Daemon ticker fires `runCleanCycle`.
2. `INSERT INTO runs (started_at) VALUES (now)`.
3. `cache.All()` → list of paths.
4. For each path:
   - `cache.Verify(path)` — if invalid, `cache.Remove(path)` and continue.
   - `cleaner.Clean(path)` → `CleanResult`.
   - If `Err != nil`: `INSERT INTO errors (...)` and continue.
   - Else: `INSERT INTO clean_events (...)`; update running totals.
5. `UPDATE runs SET finished_at=now, projects_cleaned=…, bytes_recovered=…, errors_count=… WHERE id=?`.

## Error Handling

- Per-project failures never abort the cycle. They are recorded in
  `clean_events` (with non-zero `exit_code`) or `errors`.
- Config load errors at daemon startup are fatal — the daemon exits and
  the service manager will restart it (it'll keep failing visibly).
- `health` is the official surface for surfacing accumulated errors to the
  user: it queries `errors` for the last 24h and reports counts by category.
- Logs are append-only and rotated; the DB is the source of truth for
  structured queries.

## Testing Strategy

- **TDD throughout.** Each component lands with tests written first.
- **Pure-function preference.** Scanner takes an `FS`; cleaner takes a
  `CommandRunner`; daemon takes a `Clock`. All of these have fakes.
- **Unit tests** for config parsing, scanner traversal, cleaner size
  arithmetic, cache CRUD (in-memory SQLite via `:memory:`), and stats
  aggregations.
- **Integration tests** spin up a real temp-dir tree with fake
  `Cargo.toml` files, a stub `cargo` script, and drive the daemon for a
  few simulated ticks against a fake clock. Assert state DB contents.
- **End-to-end smoke test** in CI: build the binary, run
  `car-go-clean scan` and `car-go-clean run` against a checked-in test
  fixture directory, assert exit codes and `stats` output.
- `go test ./...` is the canonical test command. Race detector on in CI.

## Distribution

- `Makefile` targets: `build`, `test`, `install` (`go install ./...`),
  `release` (cross-compile via `goreleaser`).
- Goreleaser config emits darwin/amd64, darwin/arm64, linux/amd64,
  linux/arm64 archives plus a Homebrew formula that drops into a tap repo.
- Formula carries a `service` stanza so `brew services start car-go-clean`
  registers it with launchd.
- A `contrib/car-go-clean.service` systemd user-unit is shipped for Linux
  users who don't use Homebrew.

## Layout

```
.
├── cmd/car-go-clean/        # main + subcommand wiring
├── internal/
│   ├── config/
│   ├── scanner/
│   ├── cleaner/
│   ├── cache/
│   ├── store/
│   ├── daemon/
│   └── logging/
├── contrib/                 # service unit files
├── testdata/                # fixtures (synthetic Cargo workspaces)
├── docs/superpowers/specs/  # this document and successors
├── go.mod
├── go.sum
├── Makefile
└── README.md
```

## Risks & Open Questions

- **Walking $HOME may be slow** on large filesystems. Mitigation: the
  default `scan_interval` is a week (not daily), excludes list trims the
  obvious heavy directories, and we never descend into project subtrees.
  If still slow, a future optimization is parallel walks per `scan_dir`.
- **`cargo` not on the daemon's PATH** when launched by launchd. Mitigation:
  the daemon resolves `cargo` once at startup using common locations
  (`~/.cargo/bin/cargo`, `/usr/local/bin/cargo`, `$PATH`) and records the
  resolution in logs and in `health`.
- **Workspace ambiguity.** Our "stop at first `Cargo.toml`" rule treats a
  package living next to a sibling package (no workspace root above them)
  as two separate projects, which is correct. It can mis-handle the rare
  pattern of a `Cargo.toml` that is *not* a real workspace root but sits
  above other `Cargo.toml` files. We accept this trade-off: cleaning
  there is a no-op in the worst case.
- **Concurrent writers.** Only the daemon writes. CLIs that mutate (`scan`,
  `run`) refuse to proceed if a daemon-held lock is detected (a single
  advisory lock file in the state dir). This keeps the design simple
  without forbidding ad-hoc CLI use when no daemon is running.

## Success Criteria

- Fresh install with no config: daemon starts, scans `$HOME`, runs a clean
  cycle once a day, surfaces recovered bytes via `stats`.
- `health` returns non-zero exit when a configured directory does not
  exist, and zero when everything is fine.
- `stats --since 30d` returns a per-day breakdown of bytes recovered.
- Deleting a previously discovered project directory causes it to be
  dropped from `projects` on the next cycle.
- All tests pass under `go test -race ./...`.
