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

`run --dry-run --all` refreshes discovery, performs review, and persists a
review plan. The command prints:

- review ID;
- policy hash and discovery generation;
- creation/expiry time;
- every cleanable project and target;
- every skipped project and reason;
- scan errors and incomplete origins;
- total candidate bytes.

Each plan target stores canonical path, project/target identity, reviewed
bytes, and review decision. Plans expire after 30 minutes and are invalid when
their policy hash does not match the current config.

`run --review <ID>`:

1. Loads the exact plan and matching policy.
2. Revalidates identity and every current safety gate.
3. Removes targets that are no longer safe.
4. Never adds newly discovered or newly eligible targets.
5. Prints each target immediately before Cargo.
6. Records per-target success, skip, or error.

`run --review` does not perform discovery that can expand the plan.

Bare `run` remains an explicit dynamic one-shot operation. It scans by default,
prints every actual target, and uses the runtime authority foundation.
`--no-scan` retains its cache-only discovery meaning without bypassing scope.
`--all` without `--dry-run` is a CLI error.

## Exit and Output Contract

- Complete successful dry-run: exit 0.
- Dry-run with scan errors/incomplete origins: print partial results and exit
  nonzero.
- Real run with any scan or Cargo error: print the complete summary and exit
  nonzero after all independent safe work finishes.
- Safety skips alone do not make the command fail.
- JSON output exposes projects, decisions, scan errors, policy hash,
  generation, and review ID without parsing presentation text.

The Agent Quick Start uses JSON/project output when explaining every skip.

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
5. Use v0.4 to keep the service disabled while generating a dry-run plan.
6. If the preview succeeds, execute the operator-approved path and explicitly
   `service start`.

If Homebrew fails before replacing the binary, restore the previously active
old service and report the failure. If the binary is replaced but its version
check or v0.4 preview fails, leave the service stopped, print diagnostics, and
show explicit `service start` and rollback commands.

Stopped and absent services remain stopped/absent. Tests invoke real pinned
v0.2/v0.3 binaries or faithfully built fixtures from those tags; a fake old
binary may not expose v0.4 lifecycle commands.

## Documentation and Agent Guidance

README and versioned release notes include:

- install-and-run-once using a review ID;
- bare dynamic one-shot behavior;
- macOS privacy prompts and narrow-root recommendations;
- visible incomplete-scan handling;
- the two deliberate gates for managed-storage cleanup;
- service install/start/stop persistence;
- Homebrew and shell-installer uninstall;
- service-first teardown;
- retained config/state and optional manual removal;
- pinned-version rollback ordering.

`extra_excludes` is the normal customization path.
`override_excludes` is labeled advanced and potentially broad.

## Testing

Behavioral tests cover:

- review creation and exact target persistence;
- plan policy mismatch and expiry;
- a target becoming unsafe after review;
- a new target becoming eligible after review and remaining unexecuted;
- dynamic one-shot target reporting;
- `--all` misuse;
- JSON scan errors and nonzero exit behavior;
- macOS persistent disable/enable across simulated login reload;
- Linux persistent disable/enable;
- service install immediately active;
- uninstall retaining config/state;
- v0.2 and v0.3 upgrades for active/stopped/absent states;
- Homebrew failure recovery;
- v0.4 preview failure leaving the service stopped;
- README and Agent Quick Start commands executing against real CLI output.

## Release Boundary

This work may use fake platform runners and disposable VMs. It must not alter
the real service, Homebrew installation, or user state on the development Mac.
It does not authorize a release tag.
