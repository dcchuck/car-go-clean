# Configuration reference

## Config file and defaults

Configuration is optional. Without a file, car-go-clean scans `$HOME`.

```text
$XDG_CONFIG_HOME/car-go-clean/config.toml
# or
$HOME/.config/car-go-clean/config.toml
```

```toml
scan_dirs = ["~"]
target_quiet_period = "2h"
clean_interval = "24h"
scan_interval = "1d"
log_level = "info"
```

`clean_interval` and `scan_interval` default to `24h`;
`target_quiet_period` defaults to `2h`. `log_level` accepts `debug`, `info`,
`warn`, or `error`. Tilde and environment variables expand in `scan_dirs` and
`project_dirs`.

### Default exclusions

The scanner always prunes `target` because it is build output. The editable
component defaults `.git` and `node_modules` apply wherever those directory
names occur.

The following editable defaults are anchored to `$HOME`:

- All supported hosts: `.cargo`, `.rustup`, `.cache`,
  `.bun/install/cache`, `go/pkg/mod`, `.colima`, `.lima`, and
  `.local/share/containers`.
- macOS: `Library`, `.Trash`, and `OrbStack`.
- Linux: `.local/share/docker`, `.docker/desktop`,
  `.local/share/rancher-desktop`, and `.local/share/Trash`.

Docker Desktop and Rancher Desktop data on macOS are covered by `Library`.
System-wide Docker Engine data on Linux normally lives outside `$HOME`.

An explicit `excludes` array replaces these editable defaults. Excluded
trees are pruned before filesystem or Git inspection. After a successful
scan, cached discovery candidates and active worktree-discovery state that
now match an exclusion are removed; project files and historical diagnostics
are retained.

## Scan scope

- `scan_dirs` lists roots to discover Rust projects. The default is `$HOME`.
- `project_dirs` lists explicit projects, including projects outside scan
  roots.
- `excludes` omits matching paths. Exclusions always win, including over an
  explicit `project_dirs` entry.

A discovery candidate is any directory containing `Cargo.toml`. It becomes a
valid cleanup target only when its direct, non-symlink `target/` exists and
all safety gates pass.

## Linked worktrees and discovery failures

When a scan finds a primary Git checkout, car-go-clean asks Git for linked
Rust worktrees within the configured scan roots or explicit project
directories, even when ignore rules hide them. Exclusions and the ordinary
cleaning safeguards still apply. A successful enumeration reconciles stale
cached candidates and replaces the exact primary's saved linked-worktree
association.

A discovery failure is recorded as a normal scan error. Separately, the
canonical primary and the linked worktrees saved for that primary remain
blocked until a later successful enumeration replaces that association. The
durable block normally does not spread to ancestors, siblings, or unrelated
projects. Trusted canonical failures normally block only those identified
paths. If the active discovery state cannot be trusted—for example, a legacy
failure or linked-worktree association without a trusted canonical identity,
or a saved blocked identity that no longer resolves canonically—car-go-clean
fails closed by blocking all cached projects until a successful discovery can
safely replace the association. Persisted identities are retained
conservatively: a changed alias cannot transfer or clear an old failure.

## Cleaning policy and overrides

A cached project is eligible only when its direct `project/target` exists, is
readable and measurable, has no newer non-symlink file than
`target_quiet_period`, is outside known managed cache/container storage, has
no related unreadable scan path, and has no running process inside the project
or `target/`. Canonicalization keeps cache/container classification physical
without rewriting immutable worktree provenance. Native non-UTF-8 Rust
compiler path options still protect the matching canonical project.

- `run --dry-run` refreshes and saves the review without deleting targets.
- `run --dry-run --all` lists every cleanable target.
- `run --include-managed-cache` and `run --include-active` expand the review
  policy for those named risks.
- `run --force` bypasses policy gates except the direct readable
  `project/target` requirement.
- `status --refresh`, `projects`, `projects --all`, `projects --risky`,
  `projects --active`, and `projects --json` expose the saved or refreshed
  review.
- `logs --errors-only` shows scan, review, and clean diagnostics.

## State, logs, and scheduling

State lives in `$XDG_STATE_HOME/car-go-clean` or
`$HOME/.local/state/car-go-clean`, including `state.db`, `daemon.lock`, and
newline-delimited JSON logs at `car-go-clean.log`. Logs rotate as
`car-go-clean.log.1`, `car-go-clean.log.2`, and later files. Unreadable
directories are skipped and recorded as scan errors. The daemon persists the
next scan and clean times, resuming that schedule after restart instead of
waiting for a full interval from process startup.
