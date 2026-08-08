# Upgrading from v0.2 or v0.3 to v0.4

This guide is only for an installed `v0.2.0` or `v0.3.0` binary. Use the
v0.4 migration helper below to preserve its configuration, state, service,
logs, and cleanup history.

## v0.2/v0.3 upgrade helper

Use the released helper for a state-preserving upgrade. Download it and the
shared checksum inventory into an empty directory:

```sh
curl --proto '=https' --tlsv1.2 -fsSLO \
  https://github.com/dcchuck/car-go-clean/releases/download/v0.4.0/car-go-clean-upgrade.sh
curl --proto '=https' --tlsv1.2 -fsSLO \
  https://github.com/dcchuck/car-go-clean/releases/download/v0.4.0/car-go-clean-shell-assets.sha256
awk '$2 == "car-go-clean-upgrade.sh" { print }' \
  car-go-clean-shell-assets.sha256 > car-go-clean-upgrade.sha256
if command -v sha256sum >/dev/null 2>&1
then
  sha256sum -c car-go-clean-upgrade.sha256
else
  shasum -a 256 -c car-go-clean-upgrade.sha256
fi
chmod +x car-go-clean-upgrade.sh
```

Start phase one with the method that owns the existing command returned by
`command -v car-go-clean`:

```sh
./car-go-clean-upgrade.sh --version 0.4.0 --method homebrew
# or:
./car-go-clean-upgrade.sh --version 0.4.0 --method shell
```

The helper verifies ownership before stopping a service or replacing anything.
`homebrew` requires the visible command to resolve to the installed formula's
exact binary; `shell` requires a writable, absolute, non-symlink shell
installation and rejects a Homebrew-managed command. The existence of some
Homebrew formula is not enough when a shell installation shadows it on
`PATH`. Deliberate cross-method migration is unsupported by this v0.4 helper:
uninstall the old method and perform a separate fresh install instead.

The helper accepts exactly v0.2.0 or v0.3.0 when an old binary is present and
records whether its service was active, stopped, or absent. Every installed
definition is persistently disabled and stopped with launchctl/systemd before
replacement, so login, reboot, or manager recreation cannot auto-resume the
old definition during review. An absent service remains absent. A failure
before replacement may restore only a service that was originally active,
because the exact old binary is still known to be installed.

Replacement first disarms automatic old-service restoration. The helper then
resolves and validates the exact v0.4.0 binary path; a wrong version or any
later pre-approval failure leaves the service persistently disabled/stopped,
persists recovery state, and prints exact rollback guidance. After successful
validation, the exact v0.4 binary runs `service refresh` to render the new
definition and captured physical manager roots without enabling or starting
it. Config validation and `run --dry-run --all` happen only after that refresh,
while the service remains disabled. Preview exit `0` and `2` are valid; exit
`1` stops with recovery/rollback guidance. The mode-0600 session persists the
absolute binary path with the method, old state, phase, and review ID.
Re-running the same phase-one command resumes without replacing the binary or
re-resolving a different command through `PATH`.

Inspect the complete preview and its incomplete origins. Then execute the
exact ID printed by the helper:

```sh
./car-go-clean-upgrade.sh \
  --version 0.4.0 \
  --method homebrew \
  --execute-review REVIEW_ID
```

Use exactly the same ownership method in phase two. Reviewed exits `0` and `2`
are accepted, and version validation plus reviewed execution use the exact
path persisted in phase one even if `PATH` or a shell command cache changes.
Reviewed execution also runs while disabled. Only after explicit
`--execute-review` approval and successful completion is an originally active
service re-enabled and started. An originally stopped service remains
installed, disabled, and stopped; an absent service remains absent. If
execution may have begun but did not finish, the helper fails closed with an
`executing` session and tells you to inspect state and logs; do not repeat
cleanup blindly. If cleanup completed but service restoration failed, resume
the same `--execute-review` command to finish restoration without cleaning
twice.

Rollback validates the exact restored old binary, restores the saved old
definition, reloads the manager where needed, and only then uses
platform-native launchctl/systemd enable/bootstrap/start operations. It never
calls the nonexistent v0.2/v0.3 `service start` verb. Rollback or uninstall of
the service definition retains configuration, state, logs, review diagnostics,
and cleanup history.

## Configuration migration

The legacy `excludes` key still loads in v0.4 with a deprecation warning and
the advanced replacement semantics of `override_excludes`. It is removed in
v0.5. There is no date-based promise; migrate before that version:

```sh
car-go-clean config migrate
# or:
car-go-clean config migrate --config /absolute/path/config.toml
```

Migration validates the config, shows a key-only diff, and atomically rewrites
that file. A conflict between `excludes` and `override_excludes` is rejected.
