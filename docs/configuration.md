# Configuration reference

## Config file, defaults, and strict loading

Configuration is optional. Without a file, car-go-clean scans `$HOME` with its
default settings. The default path is:

```text
$XDG_CONFIG_HOME/car-go-clean/config.toml
# or
$HOME/.config/car-go-clean/config.toml
```

When a config file exists, it is a strict optional overlay on the defaults:
omitting a key retains its default value rather than clearing it. Unknown keys
are errors. A complete normal configuration can contain these keys:

```toml
scan_dirs = ["~"]
project_dirs = []
extra_excludes = ["generated"]
# Advanced: replaces the editable default exclusions when present.
# override_excludes = [".git", "node_modules"]
clean_interval = "24h"
scan_interval = "24h"
target_quiet_period = "2h"
log_level = "info"
```

- `scan_dirs` defaults to `[$HOME]`; `project_dirs` defaults to an empty list.
  Together they must leave a non-empty effective scope.
- `clean_interval` and `scan_interval` default to `24h`; `target_quiet_period`
  defaults to `2h`. Each must be a positive [humantime][] duration, such as
  `250ms`, `2h`, `1d`, or `1 week`.
- `log_level` defaults to `info` and accepts only `debug`, `info`, `warn`, or
  `error`.

[humantime]: https://docs.rs/humantime/latest/humantime/fn.parse_duration.html

Every path-bearing value—`scan_dirs`, `project_dirs`, `extra_excludes`,
`override_excludes`, and the legacy `excludes`—expands `~`, `$NAME`, and
`${NAME}`. Expanded scan and project roots must be absolute. Relative
exclusions remain relative lexical patterns that match that directory name
wherever it occurs; absolute exclusions remain absolute paths after expansion.

Configuration loading fails for unknown keys, unset or non-Unicode path
variables, unterminated `${NAME` expressions, empty variable names, zero or
invalid durations, invalid log levels, relative roots, and an empty effective
scope.

`car-go-clean config > config.toml` prints the effective supported
configuration as TOML and is a supported configuration round trip.

## Discovery exclusions and protected storage

The scanner always prunes `target` because it is build output. Its editable
discovery defaults include `.git` and `node_modules` everywhere, plus these
paths anchored to `$HOME`:

- All supported hosts: `.cargo`, `.rustup`, `.cache`, `.bun/install/cache`,
  `go/pkg/mod`, `.colima`, `.lima`, and `.local/share/containers`.
- macOS: `Library`, `.Trash`, and `OrbStack`.
- Linux: `.local/share/docker`, `.docker/desktop`,
  `.local/share/rancher-desktop`, and `.local/share/Trash`.

Docker Desktop and Rancher Desktop data on macOS are covered by `Library`.
System-wide Docker Engine data on Linux normally lives outside `$HOME`.

Use `extra_excludes` for normal customization: its entries are additive to the
selected editable base. `override_excludes` is an advanced option that
replaces the editable discovery defaults, then still receives
`extra_excludes`. Exclusions win over both scan roots and explicit
`project_dirs`; after a successful scan, matching cached discovery candidates
and active worktree-discovery state are removed while project files and
historical diagnostics remain.

Discovery exclusions do not authorize cleanup in protected storage. Managed
package-manager and container storage remains a separate cleanup gate and is
skipped unless the applicable command explicitly uses
`--include-managed-cache`.

## Legacy key and migration

In v0.4, legacy `excludes` still loads with a deprecation warning. It has the
advanced replacement semantics of `override_excludes`; setting both keys is a
configuration error. The legacy key is removed in v0.5.

Migrate a file before upgrading:

```sh
car-go-clean config migrate
# or
car-go-clean config migrate --config /absolute/path/config.toml
```

The command validates the same strict configuration, prints a unified
key-only diff, then atomically replaces that same file. It preserves comments
where TOML editing permits, rejects conflicting or invalid configuration, and
does nothing when no legacy key exists.

## Scan scope, worktrees, and safety gates

A discovery candidate is a directory containing `Cargo.toml`. It becomes a
cleanup target only when its direct, non-symlink `target/` exists and all
safety gates pass: the target is readable and measurable, its newest
non-symlink file is older than `target_quiet_period`, it is outside protected
storage, it has no related unreadable scan path, and no process is active in
the project or target. Canonicalization keeps cache and container
classification physical without rewriting immutable worktree provenance.

When a scan finds a primary Git checkout, car-go-clean asks Git for linked
worktrees within the configured scan roots or explicit project directories,
even when ignore rules hide them. Exclusions and ordinary cleaning safeguards
still apply. A successful enumeration reconciles stale cached candidates and
replaces that primary's saved linked-worktree association.

A discovery failure is recorded as a scan error. The canonical primary and
its saved linked worktrees remain blocked until a later successful enumeration
replaces that association. If that saved state cannot be trusted,
car-go-clean fails closed by blocking all cached projects until discovery can
safely replace it.

The durable block normally does not spread to ancestors, siblings, or
unrelated projects. Trusted canonical failures normally block only the saved
paths. Persisted identities are retained conservatively: a changed alias
cannot transfer or clear an old failure.

- `run --dry-run` refreshes and saves the review without deleting targets.
- `run --dry-run --all` lists every cleanable target.
- `run --include-managed-cache` and `run --include-active` expand the review
  policy for those named risks.
- `run --force` bypasses scan-error, activity, and quiet-period gates; it does
  not bypass the direct readable-target requirement or managed-storage
  authorization.
- `status --refresh`, `projects`, `projects --all`, `projects --risky`,
  `projects --active`, and `projects --json` expose the saved or refreshed
  review.
- `logs --errors-only` shows scan, review, and clean diagnostics.

## Outcomes, state, logs, and scheduling

One-shot `scan`, `run`, `status --refresh`, and `projects` commands use this
outcome taxonomy:

- exit `0`: complete coverage. Safety skips alone stay at exit `0`.
- exit `2`: valid results with incomplete discovery coverage, such as an
  unreadable privacy-protected directory during a broad macOS home scan.
- exit `1`: a failure, including configuration errors, lock or state errors,
  or a nonzero Cargo clean attempt. Exit `1` outranks exit `2` when both occur.

Every Cargo invocation is audited. Only successful invocations contribute to
`stats` recovered-byte totals, top projects, successful-clean counts, and
recovery totals. Failed attempts are counted separately by `stats`, recorded
as clean errors visible through `logs --errors-only`, and leave the project
eligible for a later retry.

State lives in `$XDG_STATE_HOME/car-go-clean` or
`$HOME/.local/state/car-go-clean`, including `state.db`, `daemon.lock`, and
newline-delimited JSON logs at `car-go-clean.log`. Logs rotate as
`car-go-clean.log.1`, `car-go-clean.log.2`, and later files. The daemon
persists the next scan and clean times, resuming that schedule after restart
instead of waiting for a full interval from process startup.
