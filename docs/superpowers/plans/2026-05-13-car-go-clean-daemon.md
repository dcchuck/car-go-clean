# car-go-clean Rust Implementation Plan

> **For agentic workers:** REQUIRED: Use `superpowers:executing-plans` for
> task execution and `superpowers:test-driven-development` for behavior changes.

**Goal:** Build `car-go-clean` as a Rust CLI/daemon that discovers Rust
projects, periodically runs `cargo clean`, records reclaimed disk space, and
surfaces health/status/stats through subcommands.

**Architecture:** A single Rust binary backed by a library crate. Mutating
commands share the same daemon cycle code and coordinate through an advisory
lock. State is local SQLite under the user's XDG state directory.

**Tech Stack:** Rust 2021, `clap`, `serde`, `toml`, `humantime-serde`,
`rusqlite` with bundled SQLite, `fs2`, `anyhow`, stdlib process/filesystem APIs,
and integration tests using `tempfile`/`assert_cmd`.

## Completed Baseline

- [x] Reverted the accidental Go implementation commits.
- [x] Created Rust crate metadata, library modules, binary entrypoint, and
  Makefile.
- [x] Added tests first for config, scanner, store, cache, cleaner, daemon, and
  CLI smoke behavior.
- [x] Implemented config loading/defaults/path expansion/validation.
- [x] Implemented scanner traversal with excludes and no descent into project
  roots.
- [x] Implemented SQLite migrations and typed accessors for projects, runs,
  clean events, errors, and stats.
- [x] Implemented cache verification/removal for deleted projects.
- [x] Implemented cleaner byte measurement and `cargo clean` invocation behind
  a fakeable runner.
- [x] Implemented daemon scan and run cycles.
- [x] Implemented advisory lockfile support.
- [x] Implemented CLI commands: `version`, `health`, `config`, `status`,
  `scan`, `run`, `daemon`, `stats`, and `logs`.
- [x] Added CLI smoke coverage for scan -> run -> stats with fake `cargo`.

## Completed Hardening

- [x] Added graceful `SIGINT`/`SIGTERM` shutdown handling for `daemon`.
- [x] Added rotating newline-delimited JSON logs.
- [x] Added service unit templates for launchd and systemd.
- [x] Added release packaging notes with `cargo install` as the primary
  distribution channel and Homebrew as the secondary channel.
- [x] Used the `ignore` crate for gitignore-aware traversal while preserving
  the existing no-descent behavior for project roots.

## Verification Commands

Run with the repository toolchain:

```bash
mise exec rust@1.95.0 -- cargo fmt -- --check
mise exec rust@1.95.0 -- cargo test
mise exec rust@1.95.0 -- cargo clippy --all-targets -- -D warnings
mise exec rust@1.95.0 -- cargo build
```
