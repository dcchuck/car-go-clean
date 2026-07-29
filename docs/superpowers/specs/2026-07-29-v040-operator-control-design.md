# v0.4 Operator Control Design

## Context

Independent review found that the documented preview and subsequent cleanup
do not share an immutable target set, scan errors are hidden from the command
that encountered them, and service lifecycle verbs do not describe persistent
enablement. The documented v0.2/v0.3 upgrade also invokes v0.4-only service
commands before upgrading the binary.

This design depends on the runtime safety foundation and supersedes the
service lifecycle and active-upgrade sections of the earlier v0.4 hardening
design.

## Goals

1. A reviewed cleanup executes only the targets the operator saw.
2. Execution-time safety may remove targets but never add them.
3. Dynamic one-shot cleanup remains available and names every attempted
   target.
4. Scan incompleteness and cleanup failures are visible in output and exit
   status.
5. Service start/stop semantics persist across login and reboot.
6. Upgrades from v0.2 and v0.3 preserve prior service state without invoking
   unsupported old CLI commands.
7. Install, uninstall, rollback, and agent guidance match real behavior.

## Non-goals

- Do not make a review plan bypass current safety checks.
- Do not silently resume a daemon after a v0.4 safety preview fails.
- Do not delete configuration or state during service uninstall.
- Do not enable a service merely because a binary is installed by Homebrew or
  the shell installer.

## Persisted Review Plans

`--all` is a display flag today, and it stays one. Making it the trigger for
plan persistence would overload a presentation switch with a semantic action,
and then require `run --all` to become an error purely to disambiguate the
overload.

So: **every** `run --dry-run` refreshes discovery, performs review, and
persists a review plan, printing its ID. `--all` continues to control only
whether the target listing is truncated, in both `run --dry-run` and
`projects`. `run --all` without `--dry-run` remains a CLI error, because the
flag has no meaning for a command that prints a summary rather than a listing.

A dry run is consequently a database writer, not a read-only command. It takes
the same lockfile as any other run.

`run --dry-run` prints:

- review ID;
- policy hash and discovery generation;
- creation/expiry time;
- every cleanable project and target;
- every skipped project and reason;
- scan errors and incomplete origins;
- total candidate bytes.

Each plan target stores canonical path, project/target identity, reviewed
bytes, and review decision. Plans expire after 30 minutes.

A plan is valid only when **both** bindings hold:

- its policy hash matches the current config, and
- the discovery generation it was built from is still the current authorized
  generation.

The policy hash alone is not sufficient. Config can be untouched while a
daemon scan in between revokes an observation — the project was deleted, moved
out of scope, or became excluded. A plan that checked only the policy hash
would still name it. When the generation has been superseded, the whole plan
is invalid and the command says so, naming the generation that replaced it;
per-target salvage would just be discovery by another route.

`run --review <ID>`:

1. Loads the exact plan and matching policy.
2. Requires the plan's generation to be current.
3. Revalidates identity and every current safety gate, and requires each
   target to still resolve to a currently authorized observation.
4. Removes targets that are no longer safe.
5. Never adds newly discovered or newly eligible targets.
6. Prints each target immediately before Cargo.
7. Records per-target success, skip, or error.

`run --review` does not perform discovery that can expand the plan.

Plans are pruned on every plan creation and on store open: expired plans are
deleted, as are plans whose policy hash or generation no longer matches, and
at most the 20 most recent are retained. A dry run in a loop cannot grow the
state database without bound.

Bare `run` remains an explicit dynamic one-shot operation. It scans by default,
prints every actual target, and uses the runtime authority foundation.
`--no-scan` retains its cache-only discovery meaning without bypassing scope.

## Exit and Output Contract

"Nonzero" is not specific enough to build on. A scan of `~` on macOS routinely
hits TCC-protected `Desktop`, `Documents`, and `Downloads`; incomplete coverage
is the *expected* state on a stock install, not a fault. Collapsing it into the
same exit status as a real failure means the upgrade flow below — which gates
on "the preview succeeded" — would treat a healthy Mac as a failed upgrade and
leave the operator's daemon stopped.

The runtime foundation defines the taxonomy; this design binds the CLI to it:

- `0` — complete run or dry-run, coverage complete. Safety skips alone still
  exit `0`.
- `2` — completed with incomplete coverage: scan errors, an origin that failed
  to enumerate, blocked cached rows, or a stale generation under `--no-scan`.
  Partial results are printed and are valid.
- `1` — failure: a nonzero Cargo exit, config or policy error, invalid or
  expired review plan, database error, or lock conflict. `1` outranks `2`.

A real run prints the complete summary and finishes all independent safe work
before exiting with the most severe code. Automation that wants "did it work"
accepts `0` and `2`; automation that wants "was coverage complete" accepts `0`
only.

JSON output exposes projects, decisions, scan errors, policy hash, generation,
review ID, and the exit code's reason without parsing presentation text.

The Agent Quick Start documents all three codes and uses JSON/project output
when explaining every skip.

## Persistent Service Contract

Service status distinguishes:

- definition installed;
- persistent enablement;
- process running.

Commands:

- `service install`: write the definition, enable it, and start it immediately.
- `service stop`: stop and persistently disable it.
- `service start`: persistently enable and start an installed definition.
- `service restart`: require an installed, enabled service and restart it.
- `service uninstall`: stop, disable, and remove only the definition.

On macOS, stop/start use `launchctl disable`/`enable` together with
bootout/bootstrap/kickstart as appropriate. On Linux they use
`systemctl --user disable --now` and `enable --now`. Idempotency and missing
definition behavior remain explicit.

Two platform details the current `bootout`-only implementation gets wrong, and
which this design must not reintroduce:

- macOS keeps the disabled flag in a per-user launchd database
  (`/var/db/com.apple.xpc.launchd/disabled.<uid>.plist`) that is independent of
  the plist file. It survives `service uninstall`, and `bootstrap` of a
  disabled label fails. `service install` therefore always issues `enable`
  before `bootstrap`, so an install following an uninstall-while-disabled
  produces a running service rather than a silently inert one. `start` must
  likewise `enable` before `bootstrap`/`kickstart`, never after.
- The reason `bootout` alone is insufficient is `RunAtLoad` in the plist:
  launchd re-bootstraps everything in `~/Library/LaunchAgents` at next login,
  so a booted-out service silently returns. `disable` is what makes stop stick.

On Linux, `systemctl --user enable` persists across reboot only for a user with
lingering enabled; otherwise the service runs only while the user has a
session. The design does not enable lingering on the operator's behalf.
Documentation states the requirement and shows `loginctl enable-linger $USER`
for users who want the daemon to run without being logged in.

`service install` also captures the supported manager/container root overrides
from the invoking environment into the rendered definition, per the runtime
foundation, so the daemon enforces the same protected roots as the terminal
that installed it.

Documentation states that `service install` starts immediately and that
`service stop` remains stopped across login/reboot until `service start`.
Configuration and state survive uninstall.

## v0.2/v0.3 Upgrade

The upgrade helper/recipe supports active, stopped, and absent service states
from both v0.2.0 and v0.3.0.

For an active old service:

1. Record old status using the old supported `service status`.
2. Temporarily stop it with platform-native commands that preserve its
   existing definition.
3. Register failure recovery that uses old supported behavior to restore the
   service if Homebrew fails before replacing the old binary.
4. Upgrade the binary and require exact `0.4.0`.
5. Run `car-go-clean config` under the new binary to surface a deprecated
   legacy `excludes` key before anything depends on config loading, and print
   the `config migrate` invocation. A deprecated key is a warning, not a stop.
6. Use v0.4 to keep the service disabled while generating a dry-run plan.
7. Treat preview exit `0` **and** exit `2` as success, printing the
   incomplete-coverage detail for `2`. Only exit `1` is a failed preview.
   Gating on "exit code is zero" here would strand every macOS operator whose
   home directory contains a TCC-protected folder.
8. If the preview succeeded, execute the operator-approved path and explicitly
   `service start`.

If Homebrew fails before replacing the binary, restore the previously active
old service and report the failure. If the binary is replaced but its version
check or v0.4 preview fails with exit `1`, leave the service stopped, print
diagnostics, and show explicit `service start` and rollback commands.

Stopped and absent services remain stopped/absent. Tests invoke real pinned
v0.2/v0.3 binaries or faithfully built fixtures from those tags; a fake old
binary may not expose v0.4 lifecycle commands.

## Documentation and Agent Guidance

README and versioned release notes include:

- the `excludes` → `override_excludes` rename, that the old key still works in
  v0.4 with a warning, that `config migrate` rewrites it, and that it is
  removed in v0.5;
- the `0`/`2`/`1` exit-code taxonomy, with the explicit note that `2` is
  ordinary on macOS with a home-directory scan root;
- install-and-run-once using a review ID;
- bare dynamic one-shot behavior;
- macOS privacy prompts and narrow-root recommendations;
- visible incomplete-scan handling;
- the two deliberate gates for managed-storage cleanup;
- service install/start/stop persistence;
- Homebrew and shell-installer uninstall;
- service-first teardown;
- retained config/state and optional manual removal;
- `loginctl enable-linger` for Linux users who want the daemon to run while
  logged out;
- pinned-version rollback ordering.

`extra_excludes` is the normal customization path.
`override_excludes` is labeled advanced and potentially broad.

## Testing

Behavioral tests cover:

- review creation and exact target persistence;
- plan policy mismatch and expiry;
- a plan whose policy hash still matches but whose generation was superseded
  by an intervening scan is rejected as a whole;
- plan pruning: expired, mismatched, and beyond-retention plans are deleted,
  and a repeated dry-run loop leaves the plan table bounded;
- `run --dry-run` without `--all` still persists a plan and prints its ID;
- a target becoming unsafe after review;
- a new target becoming eligible after review and remaining unexecuted;
- dynamic one-shot target reporting;
- `--all` misuse;
- JSON scan errors and exit-code behavior across `0`, `2`, and `1`, including
  `1` outranking `2` when a Cargo failure and a scan error occur in one run;
- the upgrade helper proceeding on a preview that exits `2` and stopping on
  one that exits `1`;
- an upgrade whose existing config uses legacy `excludes` completes, warns,
  and leaves a startable service;
- macOS persistent disable/enable across simulated login reload;
- macOS install after an uninstall-while-disabled produces a running service,
  proving install clears the launchd disabled record;
- Linux persistent disable/enable;
- service install immediately active;
- service install captures manager/container root overrides into the rendered
  definition, and a daemon started from it resolves the same protected roots
  as the installing shell;
- uninstall retaining config/state;
- `run --review` while the daemon holds the lockfile fails cleanly with exit
  `1` and executes no Cargo;
- v0.2 and v0.3 upgrades for active/stopped/absent states;
- Homebrew failure recovery;
- v0.4 preview failure leaving the service stopped;
- README and Agent Quick Start commands executing against real CLI output.

## Release Boundary

This work may use fake platform runners and disposable VMs. It must not alter
the real service, Homebrew installation, or user state on the development Mac.
It does not authorize a release tag.
