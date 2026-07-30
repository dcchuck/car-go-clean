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
pre-tag acceptance run. It accepts neither tags nor an open artifact
directory. Start from the exact clean checkout and the downloaded aggregate
artifact produced by that commit's successful `rehearse-release` workflow.
The aggregate directory must contain `aggregate-status.txt`,
`aggregate-inventory.json`, and the workflow's `jobs/` evidence.

Supply Apple Silicon macOS and Linux base images as complete immutable GHCR
references:

```sh
export CAR_GO_CLEAN_TART_MACOS_IMAGE='ghcr.io/cirruslabs/macos-sequoia-base@sha256:<64-lowercase-hex>'
export CAR_GO_CLEAN_TART_LINUX_IMAGE='ghcr.io/cirruslabs/ubuntu-runner-arm64@sha256:<64-lowercase-hex>'
export CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0
export CAR_GO_CLEAN_ACCEPTANCE_SHA="$(git rev-parse HEAD)"
```

Replace each digest placeholder with the resolved digest for the image being
accepted. A movable `:latest` or other tag is rejected even when Tart could
resolve it. `CAR_GO_CLEAN_ACCEPTANCE_SHA` must be the exact 40-character Git
commit whose artifacts are under test. The harness runs
`scripts/validate-release-inputs.sh` before pulling a VM, so HEAD must equal
that commit, the checkout must be clean and contained by `origin/main`, and
the version/tag preconditions must still hold.

The bases do not need Rust preinstalled. macOS must provide Python 3,
Homebrew at `/opt/homebrew/bin/brew`, and a working user launchd domain.
Linux must provide Python 3, `cc`, and a working user systemd manager. The
harness installs the exact Rust/Cargo 1.95.0 minimal profile inside each
disposable clone and uses an explicit non-login PATH. It does not run
`apt-get` or `brew install`; a missing base prerequisite fails before guest
acceptance. Cirrus's minimal `ubuntu` image does not provide `cc`; the
`ubuntu-runner-arm64` example above does.

Prepare one closed artifact directory. Excluding `SHA256SUMS`, it must contain
exactly these 17 regular, non-symlink files at the top level:

```text
acceptance.sh
car-go-clean-installer.sh
car-go-clean-upgrade.sh
car-go-clean-shell-assets.sha256
car-go-clean.rb
car-go-clean-aarch64-apple-darwin.tar.xz
car-go-clean-aarch64-apple-darwin.tar.xz.sha256
car-go-clean-aarch64-unknown-linux-musl.tar.xz
car-go-clean-aarch64-unknown-linux-musl.tar.xz.sha256
car-go-clean-v0.2.0-aarch64-apple-darwin
car-go-clean-v0.3.0-aarch64-apple-darwin
car-go-clean-v0.2.0-aarch64-unknown-linux-musl
car-go-clean-v0.3.0-aarch64-unknown-linux-musl
rustup-init-aarch64-apple-darwin
rustup-init-aarch64-apple-darwin.sha256
rustup-init-aarch64-unknown-linux-gnu
rustup-init-aarch64-unknown-linux-gnu.sha256
```

Fetch each rustup binary and its publisher proof without rewriting the proof:

```sh
for target in aarch64-apple-darwin aarch64-unknown-linux-gnu
do
  url="https://static.rust-lang.org/rustup/dist/$target/rustup-init"
  curl --proto '=https' --tlsv1.2 -fsSLo \
    "/absolute/path/to/rehearsal-artifacts/rustup-init-$target" "$url"
  curl --proto '=https' --tlsv1.2 -fsSLo \
    "/absolute/path/to/rehearsal-artifacts/rustup-init-$target.sha256" \
    "$url.sha256"
done
```

Each publisher proof still names `rustup-init`; the harness compares that
digest with the corresponding renamed binary, then also requires both files
in the outer `SHA256SUMS`. That outer manifest must name every one of the 17
files exactly once and no others. Subdirectories, symlinks, nested names,
unlisted files, missing entries, and hash mismatches are rejected before a VM
pull.

`acceptance.sh`, the installer, and the upgrade helper must be byte-identical
to the exact checkout. The shell-asset proof must agree with the outer
manifest. The formula must be the deterministic render of the exact checkout
template using the four archive hashes in the exact-SHA build/smoke evidence.
The two local ARM archive hashes must also equal their aggregate hashes. Every
required validate, build, smoke, hosted-runner, and tap outcome must be the
expected success (with only the documented Apple
`linux_dependencies=skipped` exception).

The four old-version fixtures are provenance-sensitive inputs, not convenient
stand-ins. Extract the exact target binaries from the published v0.2.0 and
v0.3.0 release archives, or reproducibly build the corresponding exact tags.
Record each tag/commit, published archive URL, published archive checksum, and
the extracted binary's outer-manifest hash in the Task 8 readiness evidence.
Never substitute the current v0.4 binary, a wrapper, or a fake implementation:
the matrix exists to exercise the real old service and upgrade behavior.

Run from the repository checkout:

```sh
scripts/release/tart-rehearsal.sh \
  /absolute/path/to/rehearsal-artifacts \
  /absolute/path/to/aggregate-rehearsal-evidence \
  /absolute/path/to/release-evidence/tart
```

The output evidence path must not already exist. The orchestrator verifies the
closed input set on the host and again inside each guest, explicitly pulls
each immutable image, creates a fresh unique clone, and copies only the
manifest-bound directory. The documented Tart images use the `admin` user and
password; override
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
guest reboot. It copies the complete pre-reboot transcript and milestones
before issuing that reboot, records the Linux boot ID or macOS boot time on
both sides, and requires the identity to change before post-reboot acceptance.
The service lifecycle installs and starts explicitly, persistently stops,
refreshes the definition and captured physical manager roots without enabling
or starting, and then crosses manager recreation and reboot. The post-reboot
phase proves the service stayed disabled before explicit start and uninstall,
and proves uninstall retained config, state, logs, and history. Pre- and
post-reboot transcripts are separate files.

No acceptance failure deletes a VM. Sanitized transcripts and host-side
launch, SSH, hash, normalized tool-version, and boot-identity evidence are
copied before the orchestrator returns. Rustup's raw bootstrap chatter is not
preserved on success; a failure diagnostic replaces the guest home with
`$HOME`. The evidence `source-map.tsv` binds each fresh clone to the exact
image reference and digest, and `verified-input-bindings.tsv` binds SHA,
version, aggregate readiness, every artifact hash, and the rustup source URLs.

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
capacity. Both measurements follow Tart's supported `TART_HOME` when it is
set, otherwise `$HOME/.tart`. `CAR_GO_CLEAN_TART_HOME` is reserved for the
isolated test harness and takes precedence only when explicitly supplied.

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
