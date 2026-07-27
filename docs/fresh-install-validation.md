# Fresh Install Validation

Use a fresh macOS or Linux VM for a released-binary check. The one-shot test
must begin with an empty state directory and must not run an explicit scan
first.

## Source checkout

From the repository:

```sh
mise exec rust@1.95.0 -- cargo fmt --all -- --check
mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
mise exec rust@1.95.0 -- cargo test --locked
mise exec rust@1.95.0 -- cargo install --path . --force
```

## Released binary

Install through one official route:

```sh
brew install dcchuck/tap/car-go-clean
```

or:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh | sh
```

Verify that installation alone did not enable the per-user background
service, then confirm the released version:

```sh
car-go-clean version
car-go-clean service status
```

## Fresh-state one-shot flow

Create and build a small Rust project:

```sh
validation_root="$HOME/car-go-clean-validation"
cargo new "$validation_root/sample"
cargo build --manifest-path "$validation_root/sample/Cargo.toml"
validation_config="$validation_root/config.toml"
validation_state="$validation_root/state"
printf 'scan_dirs = ["%s"]\ntarget_quiet_period = "0s"\n' \
  "$validation_root" > "$validation_config"
```

Do not run `car-go-clean scan`. Start with the absent
`$validation_state/state.db` and run:

```sh
car-go-clean health \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean run --dry-run --all \
  --config "$validation_config" \
  --state-dir "$validation_state"
test -d "$validation_root/sample/target"
```

The dry run must print `Scan complete`, report the sample as cleanable, and
leave its target directory intact. After reviewing that output:

```sh
car-go-clean run \
  --config "$validation_config" \
  --state-dir "$validation_state"
test ! -d "$validation_root/sample/target"
car-go-clean stats --state-dir "$validation_state"
```

The real run must print `Scan complete`, clean the sample target, and record
recovered bytes.

## Explicit discovery and diagnostics

Rebuild the sample, then validate the still-supported explicit discovery and
inspection commands:

```sh
cargo build --manifest-path "$validation_root/sample/Cargo.toml"
car-go-clean scan \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean status --state-dir "$validation_state"
car-go-clean projects --all \
  --config "$validation_config" \
  --state-dir "$validation_state"
car-go-clean logs --errors-only --state-dir "$validation_state"
```

`status` must show cached projects and the saved review. `projects --all`
must explain every decision. `logs --errors-only` may be empty on a clean
fixture; any entry must name its category and path.
