# Fresh Install Validation

Use a fresh macOS or Linux VM for a released-binary check. Keep the fixture
under one disposable directory, start with an empty state directory, and do
not run an explicit scan before the first dry run.

## Source checkout

The repository toolchain and declared minimum supported Rust version are both
Rust 1.95:

```sh
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
mise exec rust@1.95.0 -- cargo test --locked
mise exec rust@1.95.0 -- cargo install --path . --force
```

The `tests/msrv.sh` gate keeps `Cargo.toml` and `rust-toolchain.toml` aligned.

## Released binary

This is a fresh-install rehearsal. Verify absence before any replacement
command; an existing v0.2.0 or v0.3.0 must use the release's state-preserving
upgrade helper instead. Its `--method` must match the owner of the existing
visible binary. To change installation methods, uninstall explicitly and run a
separate fresh-install rehearsal; the v0.4 helper does not migrate ownership.

```sh
if command -v car-go-clean >/dev/null 2>&1
then
  car-go-clean version
  car-go-clean service status
  echo "not a fresh-install fixture" >&2
  exit 1
fi
```

On an empty machine, install through one official fresh-install route:

```sh
brew install dcchuck/tap/car-go-clean
```

or:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh | sh
export PATH="$HOME/.local/bin:$PATH"
hash -r 2>/dev/null || true
test "$(command -v car-go-clean)" = "$HOME/.local/bin/car-go-clean"
```

The shell installer does not modify `PATH`; the export and command-cache
refresh above are required before later bare commands. Binary installation
must not create or start a service. Confirm the version and the three
service-state dimensions:

```sh
car-go-clean version
car-go-clean service status
```

For a fresh install, status must report `Installed: no`, `Enabled: no`, and
`Running: no`.

## Disposable project and preserved dry run

Create and build one small Rust project:

```sh
validation_root="$HOME/car-go-clean-validation"
cargo new "$validation_root/sample"
cargo build --manifest-path "$validation_root/sample/Cargo.toml"
validation_config="$validation_root/config.toml"
validation_state="$validation_root/state"
printf 'scan_dirs = ["%s"]\ntarget_quiet_period = "1s"\n' \
  "$validation_root" > "$validation_config"
sleep 2
test -d "$validation_root/sample/target"
test ! -e "$validation_state/state.db"
```

Run the first scan and review as one dry run, capture its stable status, and
extract the persisted review ID:

```sh
set +e
preview_output=$(
  car-go-clean run --dry-run --all \
    --config "$validation_config" \
    --state-dir "$validation_state"
)
preview_status=$?
set -e
printf '%s\n' "$preview_output"
case "$preview_status" in 0|2) ;; *) exit "$preview_status" ;; esac
review_id=$(
  printf '%s\n' "$preview_output" |
    sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p'
)
test -n "$review_id"
test -d "$validation_root/sample/target"
```

On this controlled fixture, `preview_status` should be `0`. Exit `2` is still
a structurally valid preview and is accepted above so its incomplete origins
can be inspected; do not continue unless they are understood. The output must
name the sample as cleanable, print exactly one review ID, and leave the
target intact.

## Exact reviewed cleanup and JSON outcome

Execute only the captured plan. JSON cleanup output is NDJSON: target events
come first and the final line is the format-v1 terminal envelope.

```sh
set +e
car-go-clean run --review "$review_id" --json \
  --config "$validation_config" \
  --state-dir "$validation_state" \
  > "$validation_root/reviewed-run.ndjson"
review_status=$?
set -e
test "$review_status" -eq 0
test ! -d "$validation_root/sample/target"
tail -n 1 "$validation_root/reviewed-run.ndjson"
```

The target event must name only the disposable sample. The terminal envelope
must have `format_version: 1`, `command: "run"`, the captured `review_id`, and
`outcome.code: 0`. No target created after the preview may be added, and any
reviewed target that fails revalidation must be skipped.

Inspect the resulting authority and accounting:

```sh
car-go-clean status --json \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean stats --json --state-dir "$validation_state"
car-go-clean logs --errors-only --json --state-dir "$validation_state"
```

Each command must end with a format-v1 envelope whose process exit matches
`outcome.code`. A clean fixture reports no errors and records recovered bytes.

## Dynamic run and explicit discovery

Bare `run` deliberately selects and cleans a fresh dynamic target set. Test it
only inside this disposable fixture:

```sh
cargo build --manifest-path "$validation_root/sample/Cargo.toml"
sleep 2
car-go-clean run \
  --config "$validation_config" \
  --state-dir "$validation_state"
test ! -d "$validation_root/sample/target"
```

Rebuild once more to validate the explicit inspection commands:

```sh
cargo build --manifest-path "$validation_root/sample/Cargo.toml"
car-go-clean scan \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean projects --all \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean status --refresh \
  --config "$validation_config" \
  --state-dir "$validation_state"
```

Historical cache is not authority: `run --no-scan` still requires the matching
policy and discovery generation and never bypasses any cleanup gate.
