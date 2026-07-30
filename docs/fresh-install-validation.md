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

## Fresh Tart release acceptance

The executable release harness turns the checks above into a two-guest,
pre-tag acceptance run. It is intentionally strict: it has no default VM
images, does not accept tags, and does not download release artifacts. Supply
one Apple Silicon macOS image and one Apple Silicon Linux image as complete
immutable GHCR references:

```sh
export CAR_GO_CLEAN_TART_MACOS_IMAGE='ghcr.io/cirruslabs/macos-sequoia-base@sha256:<64-lowercase-hex>'
export CAR_GO_CLEAN_TART_LINUX_IMAGE='ghcr.io/cirruslabs/ubuntu@sha256:<64-lowercase-hex>'
export CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0
export CAR_GO_CLEAN_ACCEPTANCE_SHA="$(git rev-parse HEAD)"
```

Replace each digest placeholder with the resolved digest for the image being
accepted. A movable `:latest` or other tag is rejected even when Tart could
resolve it. `CAR_GO_CLEAN_ACCEPTANCE_SHA` must be the exact 40-character Git
commit whose artifacts are under test.

Prepare one artifact directory copied from the exact-SHA rehearsal. It must
contain `SHA256SUMS` with safe relative paths and hashes for every supplied
file. For each guest architecture the acceptance steps require:

```text
car-go-clean-installer.sh
car-go-clean-upgrade.sh
car-go-clean-shell-assets.sha256
car-go-clean.rb
car-go-clean-<target>.tar.xz
car-go-clean-<target>.tar.xz.sha256
car-go-clean-v0.2.0-<target>
car-go-clean-v0.3.0-<target>
```

Here `<target>` is `aarch64-apple-darwin` or
`aarch64-unknown-linux-musl`. The two old-version files are executable,
faithfully built fixtures from the exact v0.2.0 and v0.3.0 tags; include their
hashes in `SHA256SUMS`. The formula is the exact locally rendered v0.4.0
formula. In the guest copy, the harness replaces exactly the current target's
v0.4.0 URL with the copied archive's `file://` URL and inserts an explicit
`version "0.4.0"`. macOS installs and tests that formula; Linux checks its Ruby
syntax while the shell installer is exercised on both guests.

Run from the repository checkout:

```sh
scripts/release/tart-rehearsal.sh \
  /absolute/path/to/rehearsal-artifacts \
  /absolute/path/to/release-evidence/tart
```

The orchestrator verifies `SHA256SUMS` on the host and again inside each
guest, explicitly pulls each immutable image, creates a fresh unique clone,
and copies only the supplied artifacts plus `acceptance.sh`. The documented
Tart images use the `admin` user and password; override
`CAR_GO_CLEAN_TART_SSH_USER` or `CAR_GO_CLEAN_TART_SSH_PASSWORD` only for an
equivalent private image.

The guest run uses a guest-local work root and records one milestone for every
required assertion: installer/formula, exact version and health, real Rust
build, preserved dry run, exact review and recovered bytes, cached
`--no-scan`, a formerly cached out-of-scope sentinel, Cargo exit `1`,
incomplete exit `2`, complete exit `0`, strict config failures, migration and
round trip, service lifecycle and retention, all v0.2/v0.3 ×
active/stopped/absent upgrades, and macOS Library/privacy behavior. Service
stop is completed in the pre-reboot phase. The host then issues an actual
guest reboot and the post-reboot phase proves the service stayed disabled
before start and uninstall.

No acceptance failure deletes a VM. Sanitized transcripts and host-side
launch/SSH/hash logs are copied to the evidence directory before the
orchestrator returns. The evidence `source-map.tsv` binds each fresh clone to
the exact image reference and digest.

## Tart inventory and irreversible cleanup

After evidence has been copied out, capture every local VM plus disk metrics:

```sh
scripts/release/tart-inventory.sh \
  /absolute/path/to/tart-inventory.tsv \
  /absolute/path/to/release-evidence/tart/source-map.tsv
```

Each non-comment row is:

```text
name<TAB>state<TAB>source_reference<TAB>source_digest
```

VMs not created by this rehearsal are printed as `UNKNOWN_SOURCE` and
`UNKNOWN_DIGEST`. Treat those rows as unrecoverable unless you separately know
their provenance. Comment rows record Tart storage bytes and host `df`
capacity.

Cleanup prints that exact concrete inventory before doing anything and is
inert without the literal confirmation:

```sh
CAR_GO_CLEAN_TART_DELETE_ALL=YES \
  scripts/release/tart-cleanup.sh \
  /absolute/path/to/tart-inventory.tsv
```

It stops and deletes only the names in the file. A VM that appeared after
inventory is not silently added to the deletion set; it causes the final
empty-inventory check to fail. After exact-name deletion, the script runs
cache-only `tart prune --entries caches --space-budget 0`—never
`--entries vms`—then requires the full `tart list --format json` result to be
empty and reports Tart bytes and host free space before and after. Deleted VMs
and unexported changes inside them cannot be recovered.
