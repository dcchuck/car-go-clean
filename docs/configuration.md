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
package-manager and container storage has two independent gates:

1. The project must be admitted by the configured scan/project scope and must
   not match the effective discovery exclusions.
2. The dry run that creates the plan must explicitly use
   `--include-managed-cache`.

`run --review ID` carries only the managed-storage approval persisted in that
plan. It has no option that can broaden the plan during execution. Removing a
default exclusion with `override_excludes` satisfies only the first gate.

Default and platform-specific exclusion roots are speculative: most machines
do not have every supported package manager, container runtime, or VM manager
installed. A missing optional absolute exclusion is normal. An absolute
exclusion that exists but cannot be canonicalized is an authority error and
blocks that cycle.

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
- `run --dry-run --include-managed-cache` and
  `run --dry-run --include-active` expand that persisted review for the named
  risks.
- `run --force` bypasses scan-error, activity, and quiet-period gates; it does
  not bypass the direct readable-target requirement or managed-storage
  authorization.
- `status --refresh`, `projects`, `projects --all`, `projects --risky`,
  `projects --active`, and `projects --json` expose the saved or refreshed
  review.
- `logs --errors-only` shows scan, review, and clean diagnostics.

## Cleanup authority and diagnostics

The historical `projects` table is not cleanup authority. Before a project can
reach Cargo, car-go-clean requires:

- A policy hash built from the effective config source, canonical scan and
  explicit-project roots, lexical and canonical exclusions, protected roots
  with provenance, quiet period, and scan interval.
- A current discovery generation whose policy hash matches exactly.
- A completed origin that authorized the project in that generation.
- Matching project and target filesystem identity during review and again
  immediately before cleanup.

`run --no-scan` skips only the discovery step. It still requires the matching
policy and generation and applies every other safety gate. State migrated from
an older path-only schema keeps historical projects, events, errors, and
recovery totals, but it deliberately has no current generation. A cached-only
run therefore exits `2` without invoking Cargo until a successful scan creates
current authority.

Use either command to inspect the same authority facts:

```sh
car-go-clean health --json || test $? -eq 2
car-go-clean status --json || test $? -eq 2
```

Exit `2` means the report is valid but cleanup authority is incomplete. Text
output contains the same cleanup-authority section. The format-v1 JSON
envelope includes `outcome`, `scan_errors`, and these fields under `data`:

- `config_source` and `canonical_scope_roots`;
- `policy_hash` and `current_generation`;
- `protected_roots`, including each root's kind and provenance;
- `incomplete_origins` from the current generation;
- `service_environment_divergence`, which is `null` when the installed
  definition does not expose enough captured environment to compare.

Service definitions may run with different manager-root variables than the
current shell. When that comparison is knowable, diagnostics report the
divergence instead of inventing provenance. Missing optional/default roots are
not reported as errors.

The final identity and safety check happens immediately before Cargo. It
substantially narrows path-replacement races, but no user-space check can
eliminate the residual time-of-check/time-of-use window after validation and
before Cargo opens the project.

## Review plans and execution

The recommended manual flow separates target selection from cleanup:

```sh
car-go-clean run --dry-run --all
car-go-clean run --review REVIEW_ID
```

Every dry run with a valid current discovery generation persists a review
plan and prints its ID, policy hash, generation, creation time, expiry, and
candidate bytes. Plans expire after 30 minutes; creation and store open prune
expired and superseded-generation plans. Creating or loading a plan under the
current authority also removes policy/generation mismatches, and only the
newest 20 plans are kept.

A reviewed run requires the exact current policy hash and discovery
generation. It does not discover or append targets. Each persisted target is
revalidated for current policy, path, identity, activity, scan errors, quiet
period, and direct-target safety immediately before Cargo. Targets that became
unsafe are removed from execution; newly eligible targets are never added.

Bare `car-go-clean run` is intentionally different: it scans, reviews, and
destructively cleans the fresh dynamic target set in one command. Use it only
when that changing target set is explicitly acceptable. `--all` controls only
dry-run display and is rejected without `--dry-run`.

`--no-scan` skips discovery but grants no authority. Historical cache rows are
diagnostic history, not permission to clean. A current matching generation
and all normal gates are still required.

## Outcomes, state, logs, and scheduling

One-shot `scan`, `run`, `status --refresh`, and `projects` commands use this
outcome taxonomy:

- exit `0`: complete coverage. Safety skips alone stay at exit `0`.
- exit `2`: valid results with incomplete discovery coverage, such as an
  unreadable privacy-protected directory during a broad macOS home scan or
  migrated path-only state used with `run --no-scan`.
- exit `1`: a failure, including configuration errors, lock or state errors,
  or a nonzero Cargo clean attempt. Exit `1` outranks exit `2` when both occur.

On macOS, privacy/TCC denial of an ordinary home-scan origin is a normal
example of exit `2`. The report and bounded review plan may still be usable;
the operator must inspect the incomplete origins and target list. A
service-status or captured-environment warning is diagnostic and does not
change the authority outcome.

Machine-readable commands use format version 1. A command that has no stream
events writes one JSON envelope. Cleanup writes newline-delimited JSON:
one `target` event before each actual Cargo invocation, followed by the
terminal envelope. Its `outcome` contains the stable code, kind, and reason
list; the envelope also carries policy hash, generation, review ID, scan
errors, and command-specific data.

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

## Service state and captured roots

Installing or upgrading the binary never installs or starts the daemon.
`car-go-clean service install` writes the per-user definition, captures the
supported root environment used by policy construction, enables the
definition, and starts it. `service status`, `status`, and `health` distinguish
installed, enabled, and running state and report manager-root divergence when
it can be determined.

`service stop` disables and stops persistently across login and reboot.
`service refresh` rewrites an installed definition with the current binary and
recaptures stable absolute physical roots without enabling or starting it.
`service start` re-enables and starts. `service uninstall` removes only the
definition and leaves configuration, state, logs, reviews, and history in
place. Use `service stop` followed by `service refresh` after reviewing changed
roots when the service must remain disabled; use `service install` only when
enabling and starting is intentional. Relative and otherwise ambiguous root
overrides are rejected before policy hashing or manager calls.

Linux systemd user services may require lingering to run without an active
login. car-go-clean does not change that account policy. Enable it manually
only when desired:

```sh
loginctl enable-linger "$USER"
```
