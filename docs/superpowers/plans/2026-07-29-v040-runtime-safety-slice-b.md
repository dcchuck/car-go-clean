# v0.4.0 Runtime Safety Slice B Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make configuration strict and safely migratable, make failed Cargo operations honest in audit and recovery data, and give every one-shot command the shared `0`/`1`/`2` outcome contract without changing runtime authority.

**Architecture:** Parse configuration into a private optional overlay, apply it to `Config::default`, and serialize only the supported public keys so `car-go-clean config` remains round-trippable. Keep Cargo process results as audit records, but let the daemon classify nonzero exits as failed work and let a small `CommandOutcome` value merge failure and incomplete-coverage signals at the CLI boundary. This plan implements Runtime Safety Slice B only; Slice A will feed policy-hash, discovery-generation, exclusion-snapshot, and identity-boundary incompleteness into the same outcome seam.

**Tech Stack:** Rust 2021, minimum supported Rust 1.88, pinned development toolchain 1.95.0, Clap 4.5, Serde, TOML 0.8, `toml_edit` 0.22, `similar` 2, rusqlite 0.32, assert_cmd, predicates, tempfile.

## Global Constraints

- Work from the reviewed design commit `22e692c` and preserve unrelated user changes.
- Do not implement Runtime Safety Slice A, operator review plans, persistent service control, or release-publication workflow changes in this slice.
- Do not create or push `v0.4.0`, publish a GitHub Release, modify the Homebrew tap, upgrade the locally installed binary, or restart the real installed daemon.
- `config` stdout must contain configuration-only valid TOML that strict loading accepts unchanged; warnings go to stderr.
- Missing environment variables are errors in `scan_dirs`, `project_dirs`, `extra_excludes`, `override_excludes`, and legacy `excludes`.
- Both `${NAME}` and bare `$NAME` are supported; an unterminated `${NAME` is an error.
- Expanded `scan_dirs` and `project_dirs` must be absolute, and the effective scope may not be empty.
- Legacy `excludes` is accepted through v0.4 as a deprecated alias for `override_excludes`; the two keys together are an error.
- Protected-storage cleanup classification is independent of editable discovery exclusions.
- A nonzero Cargo exit is audited, increments errors, does not increment successful cleans, does not update `last_cleaned_at`, and contributes zero recovery bytes to every aggregate.
- Exit `0` means complete coverage, exit `2` means valid but incomplete coverage, and exit `1` means failure; `1` outranks `2`.
- Safety skips alone remain exit `0`.
- Continue cleaning independently authorized targets after one Cargo process exits nonzero.
- Keep every stderr excerpt at or below 4096 bytes and start it on a UTF-8 character boundary.
- Make one focused commit after each task passes its targeted tests.

---

## File Structure

- Modify `src/config.rs`: own raw-overlay parsing, expansion, strict validation, effective exclusions, supported TOML serialization, and migration preparation/application.
- Modify `src/cli.rs`: expose `config migrate`, print deprecation notices without contaminating TOML stdout, report failed clean attempts, and propagate command outcomes.
- Create `src/outcome.rs`: define the shared severity-ordered `CommandOutcome`.
- Modify `src/lib.rs`: export `outcome`.
- Modify `src/main.rs`: translate `CommandOutcome` and `anyhow::Error` into process exit codes.
- Modify `src/cleaner.rs`: truncate stderr on a valid UTF-8 boundary.
- Modify `src/daemon.rs`: return scan coverage information and treat nonzero Cargo exits as failed audit events.
- Modify `src/store.rs`: exclude failed events from recovery queries and expose failed-attempt counts.
- Modify `Cargo.toml` and `Cargo.lock`: add direct `toml_edit` and `similar` dependencies without updating unrelated packages.
- Modify `tests/config.rs`: pin overlay, expansion, strict-validation, serialization, and migration behavior.
- Modify `tests/cache_cleaner_daemon.rs`: pin nonzero Cargo accounting and run-level recovery behavior.
- Modify `tests/store.rs`: pin successful-only all-time, windowed, ranking, and failure-count queries.
- Modify `tests/cli.rs`: pin migration UX, round-tripping, and the `0`/`1`/`2` process contract.
- Modify `tests/packaging.rs`: require the supported v0.4 configuration and exit-code documentation.
- Modify `README.md`, `docs/configuration.md`, and `docs/releases/v0.4.0.md`: document strict configuration, migration, failed-clean accounting, and exit meanings.

---

### Task 1: Replace permissive config deserialization with a strict overlay

**Files:**

- Modify: `src/config.rs:1-201`
- Modify: `src/cli.rs:870-944`
- Modify: `tests/config.rs:1-173`
- Modify: `tests/cli.rs:59-118`

**Interfaces:**

- Consumes: `storage::current_home_dir`, `storage::protected_roots_for`, `HostPlatform`, and the existing interval defaults.
- Produces:
  - `pub struct Config` with public effective runtime fields plus private exclusion-source state.
  - `pub enum ConfigWarning { LegacyExcludes }`.
  - `pub fn load(path: impl AsRef<Path>) -> Result<Config>`.
  - `pub fn effective_excludes(&self) -> Vec<String>`.
  - `pub fn warnings(&self) -> &[ConfigWarning]`.
  - `pub fn to_toml(&self) -> Result<String>`.
  - `fn expand_path(path: PathBuf, field: &str) -> Result<PathBuf>`.
  - `fn expand_env_vars(input: &str, field: &str) -> Result<String>`.

- [ ] **Step 1: Write failing strict-overlay and compatibility tests**

Replace the old expectation that an explicit `excludes` silently becomes the only config state. Add focused tests with these bodies in `tests/config.rs`:

```rust
#[test]
fn partial_file_overlays_defaults_instead_of_emptying_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "target_quiet_period = \"30m\"\n").unwrap();

    let cfg = load(&path).unwrap();

    assert_eq!(cfg.scan_dirs, Config::default().scan_dirs);
    assert_eq!(cfg.project_dirs, Config::default().project_dirs);
    assert_eq!(cfg.target_quiet_period, Duration::from_secs(30 * 60));
    assert_eq!(cfg.effective_excludes(), Config::default().effective_excludes());
}

#[test]
fn strict_config_rejects_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "scan_dris = [\"/tmp\"]\n").unwrap();

    let error = format!("{:#}", load(&path).unwrap_err());

    assert!(error.contains("scan_dris"), "{error}");
    assert!(error.contains("unknown field"), "{error}");
}

#[test]
fn strict_config_rejects_empty_effective_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "scan_dirs = []\nproject_dirs = []\n").unwrap();

    let error = format!("{:#}", load(&path).unwrap_err());

    assert!(error.contains("scan_dirs and project_dirs cannot both be empty"));
}

#[test]
fn strict_config_rejects_relative_scan_and_project_paths() {
    for body in [
        "scan_dirs = [\"relative/root\"]\n",
        "scan_dirs = []\nproject_dirs = [\"relative/project\"]\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, body).unwrap();

        let error = format!("{:#}", load(&path).unwrap_err());

        assert!(error.contains("must be absolute"), "{error}");
    }
}

#[test]
fn strict_config_expands_bare_and_braced_variables_in_every_path_field() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::env::set_var("CGC_SCOPE_ROOT", dir.path());
    fs::write(
        &path,
        r#"
scan_dirs = ["$CGC_SCOPE_ROOT/scan"]
project_dirs = ["${CGC_SCOPE_ROOT}/project"]
extra_excludes = ["$CGC_SCOPE_ROOT/extra"]
override_excludes = ["relative-pattern", "${CGC_SCOPE_ROOT}/absolute"]
"#,
    )
    .unwrap();

    let cfg = load(&path).unwrap();

    assert_eq!(cfg.scan_dirs, vec![dir.path().join("scan")]);
    assert_eq!(cfg.project_dirs, vec![dir.path().join("project")]);
    assert_eq!(
        cfg.effective_excludes(),
        vec![
            "relative-pattern".to_string(),
            dir.path().join("absolute").to_string_lossy().into_owned(),
            dir.path().join("extra").to_string_lossy().into_owned(),
        ]
    );
}

#[test]
fn strict_config_rejects_unset_or_unterminated_variables_in_every_path_field() {
    std::env::remove_var("CGC_DEFINITELY_UNSET");
    for body in [
        "scan_dirs = [\"$CGC_DEFINITELY_UNSET/root\"]\n",
        "project_dirs = [\"${CGC_DEFINITELY_UNSET}/project\"]\n",
        "extra_excludes = [\"$CGC_DEFINITELY_UNSET/cache\"]\n",
        "override_excludes = [\"${CGC_DEFINITELY_UNSET}/cache\"]\n",
        "excludes = [\"$CGC_DEFINITELY_UNSET/cache\"]\n",
        "scan_dirs = [\"${CGC_DEFINITELY_UNSET/root\"]\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, body).unwrap();

        assert!(load(&path).is_err(), "{body}");
    }
}

#[test]
fn legacy_excludes_loads_as_a_warned_override_but_conflicts_with_new_override() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy.toml");
    fs::write(&legacy, "scan_dirs = [\"/tmp\"]\nexcludes = [\"vendor\"]\n").unwrap();

    let cfg = load(&legacy).unwrap();

    assert_eq!(cfg.effective_excludes(), vec!["vendor".to_string()]);
    assert_eq!(cfg.warnings(), &[ConfigWarning::LegacyExcludes]);

    let conflict = dir.path().join("conflict.toml");
    fs::write(
        &conflict,
        "scan_dirs = [\"/tmp\"]\nexcludes = []\noverride_excludes = []\n",
    )
    .unwrap();
    let error = format!("{:#}", load(&conflict).unwrap_err());
    assert!(error.contains("excludes and override_excludes cannot both be set"));
}

#[test]
fn config_output_round_trips_through_strict_loading() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.toml");
    let output = dir.path().join("output.toml");
    fs::write(
        &input,
        r#"
scan_dirs = ["/tmp/work"]
project_dirs = ["/opt/explicit"]
extra_excludes = ["generated"]
target_quiet_period = "45m"
"#,
    )
    .unwrap();
    let first = load(&input).unwrap();

    fs::write(&output, first.to_toml().unwrap()).unwrap();
    let second = load(&output).unwrap();

    assert_eq!(second, first);
    assert!(second.warnings().is_empty());
}
```

Import `ConfigWarning` beside `Config`. Replace direct `Config` struct literals in this file with a mutable default followed by the field assignment under test:

```rust
let mut cfg = Config::default();
cfg.clean_interval = Duration::ZERO;
assert!(cfg.validate().is_err());
```

Apply the same pattern for `scan_interval`, `target_quiet_period`, and `log_level`, because the effective config's exclusion-source state is intentionally private. Replace direct `cfg.excludes` assertions with `cfg.effective_excludes()`. Update the cached-exclusion CLI fixture at `tests/cli.rs:77` to use a nonempty absolute scan root; strict empty-scope rejection must not be weakened to preserve an old fixture.

Add `config_command_keeps_warning_off_round_trippable_stdout` to `tests/cli.rs`. Give it a valid absolute `scan_dirs` plus legacy `excludes`, run `car-go-clean config --config <path>`, and assert:

```rust
let output = Command::cargo_bin("car-go-clean")
    .unwrap()
    .args(["config", "--config"])
    .arg(&input)
    .output()
    .unwrap();
assert!(output.status.success());
assert!(String::from_utf8_lossy(&output.stderr).contains("deprecated"));
let stdout = String::from_utf8(output.stdout).unwrap();
assert!(stdout.contains("override_excludes"));
assert!(!stdout.lines().any(|line| line.starts_with("excludes =")));
fs::write(&round_trip, stdout).unwrap();
assert!(load(&round_trip).unwrap().warnings().is_empty());
```

- [ ] **Step 2: Run the config tests and confirm the current parser fails**

Run:

```bash
cargo test --test config
```

Expected: failures show that partial TOML empties `scan_dirs`, unknown keys are accepted, unset variables become empty strings, the new keys and methods do not exist, and relative roots are accepted.

- [ ] **Step 3: Introduce the raw overlay and explicit effective config**

Replace direct `Deserialize` on `Config` with these shapes in `src/config.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigWarning {
    LegacyExcludes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub scan_dirs: Vec<PathBuf>,
    pub project_dirs: Vec<PathBuf>,
    pub clean_interval: Duration,
    pub scan_interval: Duration,
    pub target_quiet_period: Duration,
    pub log_level: String,
    editable_default_excludes: Vec<String>,
    extra_excludes: Vec<String>,
    override_excludes: Option<Vec<String>>,
    warnings: Vec<ConfigWarning>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    scan_dirs: Option<Vec<PathBuf>>,
    project_dirs: Option<Vec<PathBuf>>,
    extra_excludes: Option<Vec<String>>,
    override_excludes: Option<Vec<String>>,
    excludes: Option<Vec<String>>,
    clean_interval: Option<String>,
    scan_interval: Option<String>,
    target_quiet_period: Option<String>,
    log_level: Option<String>,
}

#[derive(Serialize)]
struct ConfigOutput<'a> {
    scan_dirs: &'a [PathBuf],
    project_dirs: &'a [PathBuf],
    extra_excludes: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    override_excludes: Option<&'a [String]>,
    clean_interval: String,
    scan_interval: String,
    target_quiet_period: String,
    log_level: &'a str,
}
```

Give `Config::default()` one captured `default_excludes()` value in `editable_default_excludes`, empty `extra_excludes`, `None` for `override_excludes`, and no warnings. Capturing the defaults once prevents an environment mutation after load from changing the effective scanner inputs. Add these methods:

```rust
impl Config {
    pub fn effective_excludes(&self) -> Vec<String> {
        let mut values = self
            .override_excludes
            .clone()
            .unwrap_or_else(|| self.editable_default_excludes.clone());
        values.extend(self.extra_excludes.iter().cloned());
        values
    }

    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(&ConfigOutput {
            scan_dirs: &self.scan_dirs,
            project_dirs: &self.project_dirs,
            extra_excludes: &self.extra_excludes,
            override_excludes: self.override_excludes.as_deref(),
            clean_interval: humantime::format_duration(self.clean_interval).to_string(),
            scan_interval: humantime::format_duration(self.scan_interval).to_string(),
            target_quiet_period: humantime::format_duration(self.target_quiet_period).to_string(),
            log_level: &self.log_level,
        })
        .context("serialize effective configuration")
    }
}
```

`extra_excludes` is applied after the selected base. With neither override key, the base is the platform-aware default list. `override_excludes`, or legacy `excludes`, replaces that editable base. Protected-path classification in `storage.rs` and `safety.rs` remains untouched.

- [ ] **Step 4: Make loading, expansion, and validation return contextual errors**

Implement `load` as a parse/apply/validate pipeline:

```rust
pub fn load(path: impl AsRef<Path>) -> Result<Config> {
    let path = path.as_ref();
    if !path.exists() {
        let cfg = Config::default();
        cfg.validate()?;
        return Ok(cfg);
    }

    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let raw: RawConfig =
        toml::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    apply_overlay(raw).with_context(|| format!("validate {}", path.display()))
}
```

Use `humantime::parse_duration` for each optional duration and report the field name. Resolve the exclusion source exactly once:

```rust
if raw.excludes.is_some() && raw.override_excludes.is_some() {
    return Err(anyhow!(
        "excludes and override_excludes cannot both be set"
    ));
}
let legacy = raw.excludes.is_some();
let override_excludes = raw.override_excludes.or(raw.excludes);
```

Expand all root and exclusion entries with a fallible character scanner. For `${NAME}`, require a closing `}` and a nonempty name. For bare `$NAME`, consume ASCII alphanumeric and underscore characters; if no name follows `$`, retain a literal `$`. Resolve each name with `env::var` and include both the variable and field in the error:

```rust
let value = env::var(&name)
    .with_context(|| format!("{field}: environment variable {name} is not set or not Unicode"))?;
```

Expand `~` and `~/...` after environment variables. Reject every expanded root for which `Path::is_absolute()` is false:

```rust
fn require_absolute(paths: &[PathBuf], field: &str) -> Result<()> {
    for path in paths {
        if !path.is_absolute() {
            return Err(anyhow!("{field} entry {} must be absolute", path.display()));
        }
    }
    Ok(())
}
```

Extend `Config::validate` with:

```rust
if self.scan_dirs.is_empty() && self.project_dirs.is_empty() {
    return Err(anyhow!(
        "scan_dirs and project_dirs cannot both be empty"
    ));
}
require_absolute(&self.scan_dirs, "scan_dirs")?;
require_absolute(&self.project_dirs, "project_dirs")?;
```

Keep relative exclusion entries relative. If an exclusion starts absolute before expansion, assert it remains absolute afterward; an unset variable must already have produced an error instead of collapsing the path.

- [ ] **Step 5: Switch scanner construction and config printing to the supported interface**

In `src/cli.rs`, replace `cfg.excludes.clone()` with:

```rust
excludes: cfg.effective_excludes(),
```

Replace direct Serde serialization in the `Commands::Config` branch with:

```rust
let cfg = load_config(config)?;
print!("{}", cfg.to_toml()?);
Ok(())
```

In `load_config`, emit the legacy notice only to stderr:

```rust
if cfg.warnings().contains(&ConfigWarning::LegacyExcludes) {
    eprintln!(
        "warning: `excludes` is deprecated in v0.4; run `car-go-clean config migrate` to rename it to `override_excludes` before v0.5"
    );
}
```

Import `ConfigWarning` at the top of `src/cli.rs`. In `health`, also print this presentation warning after `OK`:

```rust
if cfg.warnings().contains(&ConfigWarning::LegacyExcludes) {
    println!("WARN: legacy `excludes` is deprecated; run `car-go-clean config migrate`");
}
```

- [ ] **Step 6: Run focused and regression tests**

Run:

```bash
cargo test --test config
cargo test --test cli run_no_scan_prunes_physically_excluded_cached_alias_before_review
cargo test --test scanner
```

Expected: all pass. `car-go-clean config` stdout is valid TOML with `extra_excludes` and, only when selected, `override_excludes`; no output contains the deprecated `excludes` key as an emitted key.

- [ ] **Step 7: Commit the strict overlay**

```bash
git add docs/superpowers/plans/2026-07-29-v040-runtime-safety-slice-b.md src/config.rs src/cli.rs tests/config.rs tests/cli.rs
git commit -m "feat: enforce strict configuration overlays"
```

---

### Task 2: Add a comment-preserving `config migrate` command

**Files:**

- Modify: `Cargo.toml:15-29`
- Modify: `Cargo.lock`
- Modify: `src/config.rs`
- Modify: `src/cli.rs:156-346`
- Modify: `tests/config.rs`
- Modify: `tests/cli.rs`

**Interfaces:**

- Consumes: strict `RawConfig` and `load` from Task 1.
- Produces:
  - `pub struct ConfigMigration`.
  - `pub fn prepare_migration(path: impl AsRef<Path>) -> Result<Option<ConfigMigration>>`.
  - `pub fn unified_diff(&self) -> String`.
  - `pub fn apply(self) -> Result<()>`.
  - `enum ConfigCommands { Migrate }` in the CLI.

- [ ] **Step 1: Add failing migration tests**

Add to `tests/config.rs`:

```rust
#[test]
fn migration_renames_only_the_legacy_key_and_preserves_comments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"# operator scope
scan_dirs = ["/tmp/work"]

# intentionally broad legacy override
excludes = [
  "vendor", # generated source
]
"#,
    )
    .unwrap();

    let migration = prepare_migration(&path).unwrap().unwrap();
    let diff = migration.unified_diff();

    assert!(diff.contains("--- "));
    assert!(diff.contains("+++ "));
    assert!(diff.contains("-excludes = ["));
    assert!(diff.contains("+override_excludes = ["));
    assert!(fs::read_to_string(&path).unwrap().contains("excludes = ["));

    migration.apply().unwrap();
    let migrated = fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("# operator scope"));
    assert!(migrated.contains("# intentionally broad legacy override"));
    assert!(migrated.contains("# generated source"));
    assert!(migrated.contains("override_excludes = ["));
    assert!(!migrated.lines().any(|line| line.starts_with("excludes =")));
    assert!(load(&path).unwrap().warnings().is_empty());
}

#[test]
fn migration_is_a_noop_without_a_legacy_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "scan_dirs = [\"/tmp\"]\nextra_excludes = [\"vendor\"]\n",
    )
    .unwrap();

    assert!(prepare_migration(&path).unwrap().is_none());
}

#[test]
fn migration_refuses_conflicting_or_unknown_configuration() {
    let dir = tempfile::tempdir().unwrap();
    for body in [
        "scan_dirs = [\"/tmp\"]\nexcludes = []\noverride_excludes = []\n",
        "scan_dirs = [\"/tmp\"]\nexclude = []\n",
    ] {
        let path = dir.path().join(format!("{}.toml", body.len()));
        fs::write(&path, body).unwrap();
        assert!(prepare_migration(&path).is_err());
    }
}
```

Add a Unix CLI test to `tests/cli.rs` that executes:

```rust
Command::cargo_bin("car-go-clean")
    .unwrap()
    .args(["config", "migrate", "--config"])
    .arg(&config)
    .assert()
    .success()
    .stdout(contains("--- "))
    .stdout(contains("+++ "))
    .stdout(contains("-excludes = ["))
    .stdout(contains("+override_excludes = ["));
```

After the process, assert the file loads without `ConfigWarning::LegacyExcludes`. Add a second invocation assertion with stdout containing `No migration needed`.

- [ ] **Step 2: Run the migration tests and confirm the API and command are absent**

Run:

```bash
cargo test --test config migration_
cargo test --test cli config_migrate
```

Expected: compilation fails because `prepare_migration` and `config migrate` do not exist.

- [ ] **Step 3: Add direct edit and diff dependencies**

Add:

```toml
similar = "2"
toml_edit = "0.22"
```

to `[dependencies]` in `Cargo.toml`, preserving alphabetical order. Run:

```bash
cargo check
```

Inspect `git diff -- Cargo.lock`. Expected: `similar` becomes a direct package dependency, `toml_edit` becomes a direct root dependency, and unrelated package versions and checksums do not change.

- [ ] **Step 4: Implement prepare, diff, and atomic apply**

Add:

```rust
#[derive(Debug)]
pub struct ConfigMigration {
    path: PathBuf,
    before: String,
    after: String,
}

impl ConfigMigration {
    pub fn unified_diff(&self) -> String {
        similar::TextDiff::from_lines(&self.before, &self.after)
            .unified_diff()
            .header(
                &format!("{} (legacy)", self.path.display()),
                &format!("{} (migrated)", self.path.display()),
            )
            .to_string()
    }

    pub fn apply(self) -> Result<()> {
        let current = fs::read_to_string(&self.path)
            .with_context(|| format!("re-read {}", self.path.display()))?;
        if current != self.before {
            return Err(anyhow!(
                "{} changed after migration preview; refusing to overwrite it",
                self.path.display()
            ));
        }
        write_atomic(&self.path, self.after.as_bytes())
    }
}
```

Implement `prepare_migration` by reading the file, parsing `RawConfig` first, passing it through `apply_overlay` to enforce every strict rule before any rewrite, and parsing `toml_edit::DocumentMut`. Remove the root entry as its formatted `(Key, Item)` pair, construct the renamed key with the old key's decoration, and insert it in the same table:

```rust
pub fn prepare_migration(path: impl AsRef<Path>) -> Result<Option<ConfigMigration>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let before =
        fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let raw: RawConfig =
        toml::from_str(&before).with_context(|| format!("parse {}", path.display()))?;
    if raw.excludes.is_some() && raw.override_excludes.is_some() {
        return Err(anyhow!(
            "excludes and override_excludes cannot both be set"
        ));
    }
    if raw.excludes.is_none() {
        return Ok(None);
    }
    apply_overlay(raw)?;

    let mut document = before
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("edit {}", path.display()))?;
    let (old_key, item) = document
        .as_table_mut()
        .remove_entry("excludes")
        .ok_or_else(|| anyhow!("legacy excludes key disappeared during migration"))?;
    let new_key = toml_edit::Key::new("override_excludes")
        .with_leaf_decor(old_key.leaf_decor().clone())
        .with_dotted_decor(old_key.dotted_decor().clone());
    document
        .as_table_mut()
        .insert_formatted(&new_key, item);
    let after = document.to_string();
    let migrated: RawConfig =
        toml::from_str(&after).context("validate migrated configuration")?;
    if migrated.override_excludes.is_none() || migrated.excludes.is_some() {
        return Err(anyhow!("migrated configuration did not preserve exclusions"));
    }
    apply_overlay(migrated)?;

    Ok(Some(ConfigMigration {
        path: path.to_path_buf(),
        before,
        after,
    }))
}
```

Implement `write_atomic` with a sibling temporary file using `OpenOptions::create_new(true)`, copy the source file permissions to it, call `write_all` and `sync_all`, then `fs::rename` it over the exact file. Name the temporary file with the current process ID:

```rust
let temp_path = path.with_extension(format!("car-go-clean-migrate-{}.tmp", std::process::id()));
```

On any write or rename error, call `fs::remove_file(&temp_path)` and return the original contextual error. Do not follow or delete any other path.

- [ ] **Step 5: Add the nested Clap command and preserve machine-readable stdout**

Change `Commands::Config` to:

```rust
Config {
    #[command(subcommand)]
    command: Option<ConfigCommands>,
    #[arg(long, global = true)]
    config: Option<PathBuf>,
},
```

Add:

```rust
#[derive(Debug, Subcommand)]
enum ConfigCommands {
    /// Rename deprecated configuration keys in place.
    Migrate,
}
```

Dispatch with:

```rust
Commands::Config { command, config } => match command {
    None => {
        let cfg = load_config(config)?;
        print!("{}", cfg.to_toml()?);
        Ok(())
    }
    Some(ConfigCommands::Migrate) => {
        let path = config.unwrap_or_else(default_path);
        match prepare_migration(&path)? {
            Some(migration) => {
                print!("{}", migration.unified_diff());
                migration.apply()?;
                println!("Migrated {}", path.display());
            }
            None => println!("No migration needed for {}", path.display()),
        }
        Ok(())
    }
},
```

The diff is printed before `apply`. Ordinary `config` keeps TOML on stdout and the legacy warning on stderr.

- [ ] **Step 6: Run migration, config, and CLI tests**

Run:

```bash
cargo test --test config
cargo test --test cli config
cargo test --test packaging configuration_reference_preserves_operational_contract
```

Expected: all pass. The second migration invocation is idempotent, and a failed conflict check leaves the original bytes unchanged.

- [ ] **Step 7: Commit the migration command**

```bash
git add Cargo.toml Cargo.lock src/config.rs src/cli.rs tests/config.rs tests/cli.rs
git commit -m "feat: migrate legacy exclusion config"
```

---

### Task 3: Make stderr excerpts UTF-8 boundary safe

**Files:**

- Modify: `src/cleaner.rs:177-182`

**Interfaces:**

- Consumes: `MAX_STDERR_EXCERPT = 4096`.
- Produces: `fn stderr_excerpt(stderr: &str) -> String` that returns the tail, uses at most 4096 bytes, and never slices inside a Unicode scalar value.

- [ ] **Step 1: Add a failing unit test beside the private helper**

Append to `src/cleaner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_excerpt_starts_on_a_utf8_boundary() {
        let stderr = format!("prefix:{}", "€".repeat(2_000));

        let excerpt = stderr_excerpt(&stderr);

        assert!(excerpt.len() <= MAX_STDERR_EXCERPT);
        assert!(stderr.ends_with(&excerpt));
        assert!(excerpt.chars().all(|character| character == '€'));
    }

    #[test]
    fn stderr_excerpt_preserves_short_input() {
        assert_eq!(stderr_excerpt("cargo failed: λ"), "cargo failed: λ");
    }
}
```

- [ ] **Step 2: Run the focused test and observe the panic**

Run:

```bash
cargo test cleaner::tests::stderr_excerpt_starts_on_a_utf8_boundary -- --exact
```

Expected: FAIL with a byte-index/character-boundary panic at the existing raw slice.

- [ ] **Step 3: Advance the start offset to a character boundary**

Replace the raw slice with:

```rust
fn stderr_excerpt(stderr: &str) -> String {
    if stderr.len() <= MAX_STDERR_EXCERPT {
        return stderr.to_string();
    }

    let mut start = stderr.len() - MAX_STDERR_EXCERPT;
    while !stderr.is_char_boundary(start) {
        start += 1;
    }
    stderr[start..].to_string()
}
```

- [ ] **Step 4: Run cleaner and daemon tests**

Run:

```bash
cargo test cleaner::tests
cargo test --test cache_cleaner_daemon cleaner_
```

Expected: all pass; the multibyte test returns a valid suffix no larger than 4096 bytes.

- [ ] **Step 5: Commit the panic fix**

```bash
git add src/cleaner.rs
git commit -m "fix: truncate cargo stderr on character boundaries"
```

---

### Task 4: Make nonzero Cargo exits failed work, not recovered work

**Files:**

- Modify: `src/daemon.rs:65-297`
- Modify: `src/store.rs:1000-1029`
- Modify: `src/cli.rs:830-860`
- Modify: `tests/cache_cleaner_daemon.rs:932-988`
- Modify: `tests/cache_cleaner_daemon.rs:1471-1581`
- Modify: `tests/store.rs:943-1000`
- Modify: `tests/cli.rs`

**Interfaces:**

- Consumes: `CleanResult.exit_code`, `CleanResult.stderr_excerpt`, `Store::record_clean_event`, `Store::record_error`, and `Store::finish_run`.
- Produces:
  - `RunCycleResult` where `cleaned`, `bytes_recovered`, and the persisted run contain successful events only.
  - `pub fn failed_clean_attempts(&self, since: SystemTime) -> Result<i64>`.
  - Stats text field `Failed clean attempts` and JSON field `failed_clean_attempts`.

- [ ] **Step 1: Add a daemon test for partial deletion plus nonzero exit**

Extend `FakeRunner` with:

```rust
delete_relative_path: Option<PathBuf>,
```

and, before returning `CleanOutcome`, execute:

```rust
if let Some(relative) = &self.delete_relative_path {
    fs::remove_file(dir.join("target").join(relative)).unwrap();
}
```

Add this test in `tests/cache_cleaner_daemon.rs`:

```rust
#[test]
fn failed_cargo_clean_is_audited_without_success_or_recovery_accounting() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("proj");
    write_file(&project.join("Cargo.toml"), b"[package]\n");
    write_file(&project.join("target/removed.bin"), &[0; 2048]);
    write_file(&project.join("target/retained.bin"), &[0; 1024]);
    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path().join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&project, SystemTime::now()).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let daemon = Daemon::new(
        &store,
        Cache::new(&store),
        Scanner::new(ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        }),
        Cleaner::new(
            "cargo",
            FakeRunner {
                delete_relative_path: Some(PathBuf::from("removed.bin")),
                exit_code: 7,
                stderr: "cargo metadata failed".to_string(),
                ..FakeRunner::default()
            },
            Duration::from_secs(60),
        ),
        DaemonOptions {
            target_quiet_period: Duration::from_millis(1),
            ..DaemonOptions::default()
        },
    );

    let result = daemon
        .run_cycle_with_safety(
            SafetyOptions {
                target_quiet_period: Duration::from_millis(1),
                include_managed_cache: false,
                include_active: false,
                force: true,
            },
            &NoopProcessInspector,
        )
        .unwrap();

    assert_eq!(result.cleaned, 0);
    assert_eq!(result.bytes_recovered, 0);
    assert_eq!(result.errors, 1);
    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 0);
    assert_eq!(run.bytes_recovered, 0);
    assert_eq!(run.errors_count, 1);
    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].exit_code, 7);
    assert!(events[0].bytes_before > events[0].bytes_after);
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        0
    );
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert!(errors.iter().any(|error| {
        error.category == "clean"
            && error.message.contains("cargo clean exited 7")
            && error.message.contains("cargo metadata failed")
    }));
    assert!(store.all_projects().unwrap()[0].last_cleaned_at.is_none());
}
```

- [ ] **Step 2: Add mixed-event aggregate tests**

Change `records_runs_clean_events_errors_and_stats` in `tests/store.rs` so the second event has `ts: t0 + Duration::from_secs(10)` and `exit_code: 9`, then assert:

```rust
assert_eq!(
    store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
    900
);
assert_eq!(
    store
        .total_bytes_recovered(t0 + Duration::from_secs(5))
        .unwrap(),
    0
);
let top = store
    .top_projects_by_bytes(SystemTime::UNIX_EPOCH, 10)
    .unwrap();
assert_eq!(top.len(), 1);
assert_eq!(top[0].path, "/a");
assert_eq!(top[0].bytes, 900);
assert_eq!(
    store.failed_clean_attempts(SystemTime::UNIX_EPOCH).unwrap(),
    1
);
```

Keep `run.bytes_recovered` at `900` in this fixture because the caller now persists successful-only recovery.

- [ ] **Step 3: Run the focused tests and confirm current false-success behavior**

Run:

```bash
cargo test --test cache_cleaner_daemon failed_cargo_clean_is_audited_without_success_or_recovery_accounting
cargo test --test store records_runs_clean_events_errors_and_stats
```

Expected: failures show `cleaned=1`, nonzero run recovery, an updated `last_cleaned_at`, and aggregates that include the failed event.

- [ ] **Step 4: Branch daemon accounting on the real Cargo exit code**

In `run_cycle_with_safety`, record every non-skipped `CleanResult` before classifying it:

```rust
Ok(result) => {
    let now = SystemTime::now();
    self.store.record_clean_event(&CleanEvent {
        id: 0,
        run_id,
        ts: now,
        path: project.path.clone(),
        bytes_before: result.bytes_before,
        bytes_after: result.bytes_after,
        duration_ms: result.duration.as_millis() as i64,
        exit_code: result.exit_code,
        stderr_excerpt: result.stderr_excerpt.clone(),
    })?;

    if result.exit_code == 0 {
        projects_cleaned += 1;
        bytes_recovered += (result.bytes_before - result.bytes_after).max(0);
        self.store.mark_project_cleaned(&project.path, now)?;
    } else {
        errors_count += 1;
        let detail = if result.stderr_excerpt.is_empty() {
            format!("cargo clean exited {}", result.exit_code)
        } else {
            format!(
                "cargo clean exited {}: {}",
                result.exit_code, result.stderr_excerpt
            )
        };
        self.store.record_error(&ErrorRecord {
            id: 0,
            ts: now,
            category: "clean".to_string(),
            path: Some(project.path.clone()),
            message: detail,
        })?;
    }
}
```

Leave the loop running after this branch so a later project can still succeed. A `CommandRunner` I/O error remains the existing `Err(err)` arm and also increments the run error count.

- [ ] **Step 5: Filter every persisted recovery query and expose failure count**

Change both SQL queries in `src/store.rs`:

```sql
SELECT COALESCE(SUM(bytes_before - bytes_after), 0)
FROM clean_events
WHERE ts >= ?1 AND exit_code = 0
```

and:

```sql
SELECT path, SUM(bytes_before - bytes_after) AS recovered
FROM clean_events
WHERE ts >= ?1 AND exit_code = 0
GROUP BY path
ORDER BY recovered DESC
LIMIT ?2
```

Add:

```rust
pub fn failed_clean_attempts(&self, since: SystemTime) -> Result<i64> {
    self.conn
        .query_row(
            "SELECT COUNT(*) FROM clean_events WHERE ts >= ?1 AND exit_code <> 0",
            [to_epoch(since)?],
            |row| row.get(0),
        )
        .map_err(Into::into)
}
```

In `stats`, load this count. Add `"failed_clean_attempts": failed_clean_attempts` to JSON and:

```rust
println!("Failed clean attempts: {failed_clean_attempts}");
```

to text output. `logs --errors-only` already reads the `clean` error inserted by the daemon.

- [ ] **Step 6: Add a real CLI-process failure test**

In a Unix-only `tests/cli.rs` test, build a fake `cargo` that removes one target file, writes a multibyte message to stderr, and exits `7`:

```sh
#!/bin/sh
rm -f target/removed.bin
printf 'cargo failed: λ\n' >&2
exit 7
```

Run `car-go-clean run --force` with an absolute config root and isolated state. At this task boundary, assert `.failure()` rather than exact code; Task 5 pins code `1`. Assert stdout contains:

```text
Run complete: cleaned=0
```

and:

```text
errors=1
```

Open the state store and assert the failed event, error record, zero run recovery, zero lifetime recovery, and unchanged `last_cleaned_at`.

Run `car-go-clean stats --state-dir <isolated-state>` afterward and assert the text output contains:

```text
Bytes recovered: 0
Failed clean attempts: 1
```

Run the JSON form and assert `total_bytes` is `0` and `failed_clean_attempts` is `1`.

- [ ] **Step 7: Run accounting regressions**

Run:

```bash
cargo test --test cache_cleaner_daemon
cargo test --test store
cargo test --test cli cargo_failure
```

Expected: all pass. Existing successful events retain their current totals; only nonzero events are excluded.

- [ ] **Step 8: Commit honest Cargo accounting**

```bash
git add src/daemon.rs src/store.rs src/cli.rs tests/cache_cleaner_daemon.rs tests/store.rs tests/cli.rs
git commit -m "fix: exclude failed cargo cleans from recovery"
```

---

### Task 5: Implement the shared `0`/`1`/`2` command outcome

**Files:**

- Create: `src/outcome.rs`
- Modify: `src/lib.rs:1-13`
- Modify: `src/main.rs:1-3`
- Modify: `src/daemon.rs:65-178`
- Modify: `src/cli.rs:279-346`
- Modify: `src/cli.rs:441-598`
- Modify: `tests/cache_cleaner_daemon.rs`
- Modify: `tests/cli.rs`

**Interfaces:**

- Consumes: Task 4's `RunCycleResult.errors`, scanner `ScanReport.errors`, current scan-error records, and durable worktree-discovery blocks.
- Produces:
  - `pub enum CommandOutcome { Complete, Failed, Incomplete }`.
  - `pub fn merge(self, other: Self) -> Self`.
  - `pub fn code(self) -> u8`.
  - `pub struct ScanCycleResult { pub errors: usize }`.
  - `pub fn scan_cycle(&self) -> Result<ScanCycleResult>`.
  - `RunCycleResult { pub coverage_incomplete: bool, ... }`.
  - `struct ReviewBatch { reviews: Vec<ProjectReview>, coverage_incomplete: bool }`.
  - `fn project_reviews(...) -> Result<ReviewBatch>`.
  - `pub fn cli::run() -> std::process::ExitCode`.

- [ ] **Step 1: Add unit tests for severity and scan reporting**

Create `src/outcome.rs` with a test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_outranks_incomplete_and_incomplete_outranks_complete() {
        assert_eq!(
            CommandOutcome::Complete.merge(CommandOutcome::Incomplete),
            CommandOutcome::Incomplete
        );
        assert_eq!(
            CommandOutcome::Incomplete.merge(CommandOutcome::Failed),
            CommandOutcome::Failed
        );
        assert_eq!(
            CommandOutcome::Failed.merge(CommandOutcome::Complete),
            CommandOutcome::Failed
        );
    }

    #[test]
    fn public_codes_are_zero_one_two() {
        assert_eq!(CommandOutcome::Complete.code(), 0);
        assert_eq!(CommandOutcome::Failed.code(), 1);
        assert_eq!(CommandOutcome::Incomplete.code(), 2);
    }
}
```

Add a daemon test using a missing absolute scan root:

```rust
let result = daemon.scan_cycle().unwrap();
assert_eq!(result.errors, 1);
```

The store must contain the scan error even though scanning returned a usable result.

- [ ] **Step 2: Add CLI tests for all three codes and precedence**

Add these named process-level cases to `tests/cli.rs`:

1. `exit_code_zero_for_complete_scan`: a readable root with no scanner errors; `scan` exits `0`.
2. `exit_code_two_for_incomplete_scan`: a missing absolute root; `scan` exits `2`, prints `Scan complete: errors=1`, and records the error.
3. `exit_code_two_after_cleaning_with_incomplete_scan`: a valid project root plus a missing root; `run --force` cleans the valid target, prints the full summary, and exits `2`.
4. `exit_code_one_for_cargo_failure`: a fake Cargo exit `7` with otherwise complete scanning; the full summary prints and the command exits `1`.
5. `exit_code_one_outranks_incomplete_scan`: a fake Cargo exit `7` plus a missing scan root; the full summary prints and the command exits `1`.
6. `exit_code_two_for_no_scan_with_durable_discovery_block`: a durable worktree-discovery block with `run --dry-run --no-scan`; the review prints and exits `2`.
7. `exit_code_zero_for_safety_only_skip`: a quiet-period skip with complete scanning; the command exits `0`.
8. `exit_code_one_for_config_and_lock_failures`: an unknown config key and a held lockfile each exit `1` and invoke no Cargo process.

Use `assert_cmd`'s exact assertions:

```rust
.assert().code(0)
.assert().code(1)
.assert().code(2)
```

Update existing tests at `tests/cli.rs:651-676` and `tests/cli.rs:722-767`: commands whose output contains an active `scan_error` now expect code `2`, including `--force`, because force may authorize a target but does not make discovery coverage complete.

- [ ] **Step 3: Run the new tests and confirm the binary collapses outcomes**

Run:

```bash
cargo test outcome::tests
cargo test --test cli exit_code_
```

Expected: compilation fails for the absent outcome type, and current scan-error commands incorrectly exit `0`.

- [ ] **Step 4: Implement the severity-ordered outcome type**

Create:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    Complete,
    Failed,
    Incomplete,
}

impl CommandOutcome {
    pub fn merge(self, other: Self) -> Self {
        use CommandOutcome::{Complete, Failed, Incomplete};
        match (self, other) {
            (Failed, _) | (_, Failed) => Failed,
            (Incomplete, _) | (_, Incomplete) => Incomplete,
            (Complete, Complete) => Complete,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Complete => 0,
            Self::Failed => 1,
            Self::Incomplete => 2,
        }
    }
}
```

Export it with `pub mod outcome;` in `src/lib.rs`.

- [ ] **Step 5: Return explicit scan and run coverage**

In `src/daemon.rs`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanCycleResult {
    pub errors: usize,
}
```

Change `scan_cycle` to retain `let error_count = report.errors.len();`, record every error as it does now, complete all reconciliation, and return:

```rust
Ok(ScanCycleResult {
    errors: error_count,
})
```

Update daemon scheduler call sites to ignore the successful value explicitly:

```rust
if let Err(err) = self.scan_cycle() {
```

No scanner error becomes `Err`; persistence, database, and internal scanner failures remain `Err`.

Add `coverage_incomplete: bool` to `RunCycleResult`. In `run_cycle_with_safety`, compute it from the authoritative information this slice currently has:

```rust
let coverage_incomplete = !scan_errors.is_empty() || !discovery_blocks.is_empty();
```

Return and log it. Slice A will OR in policy/generation mismatch, origin authority, and blocked cached-row state; it must not replace this field or reinterpret Cargo failures.

Change `project_reviews` to return the reviews and their global coverage state together:

```rust
#[derive(Debug)]
struct ReviewBatch {
    reviews: Vec<ProjectReview>,
    coverage_incomplete: bool,
}
```

After loading `scan_errors` and `discovery_blocks`, preserve the existing review construction and return:

```rust
Ok(ReviewBatch {
    reviews,
    coverage_incomplete: !scan_errors.is_empty() || !discovery_blocks.is_empty(),
})
```

All presentation call sites read `batch.reviews`; `run --dry-run`, `projects`, and `status --refresh` merge `Incomplete` when `batch.coverage_incomplete` is true. `ReviewSummary.scan_error` remains part of printed breakdowns, not a substitute for the global flag, because an unreadable origin can yield no cached project to mark.

- [ ] **Step 6: Make CLI dispatch return outcomes and main own error rendering**

Change:

```rust
fn execute(cli: Cli) -> Result<CommandOutcome>
```

Commands without a coverage result map successful `Result<()>` to `CommandOutcome::Complete`. Change `scan_and_report` to:

```rust
fn scan_and_report(store: &Store, cfg: &Config) -> Result<CommandOutcome> {
    let result = daemon_for_scan(store, cfg).scan_cycle()?;
    println!("Scan complete: errors={}", result.errors);
    Ok(if result.errors == 0 {
        CommandOutcome::Complete
    } else {
        CommandOutcome::Incomplete
    })
}
```

Change `run_once` to initialize `let mut outcome = CommandOutcome::Complete;`, merge the scan outcome when scanning is enabled, and merge review/run state before returning. For a real run:

```rust
outcome = outcome.merge(if result.errors > 0 {
    CommandOutcome::Failed
} else if result.coverage_incomplete {
    CommandOutcome::Incomplete
} else {
    CommandOutcome::Complete
});
Ok(outcome)
```

For dry-run and `projects`, use recent scan-error paths, durable discovery blocks, and `ReviewSummary.scan_error` to return `Incomplete` while still printing results. `status --refresh` uses the same review outcome. Plain informational `version`, `config`, `status`, `stats`, `logs`, `health`, and successful service commands return `Complete`.

Change the public entry point:

```rust
pub fn run() -> std::process::ExitCode {
    match execute(Cli::parse()) {
        Ok(outcome) => std::process::ExitCode::from(outcome.code()),
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::ExitCode::from(CommandOutcome::Failed.code())
        }
    }
}
```

and `src/main.rs`:

```rust
fn main() -> std::process::ExitCode {
    car_go_clean::cli::run()
}
```

This preserves a printed summary for Cargo failures because they return `CommandOutcome::Failed`, while configuration, database, and lock errors render through the `Err` branch. An `Err` after an incomplete scan naturally exits `1`.

- [ ] **Step 7: Update all affected assertions and daemon call sites**

Change tests that compare `scan_cycle().unwrap()` to unit so they either ignore the result or assert its error count:

```rust
let scan = daemon.scan_cycle().unwrap();
assert_eq!(scan.errors, 0);
```

Replace exact `"Scan complete\n"` output assertions with `"Scan complete: errors=0\n"`. Change only tests with genuine incomplete coverage from `.success()` to `.code(2)`; safety-only skips remain `.success()` or `.code(0)`.

- [ ] **Step 8: Run outcome and full CLI/daemon suites**

Run:

```bash
cargo test outcome::tests
cargo test --test cli
cargo test --test cache_cleaner_daemon
```

Expected: all pass. Exact code cases demonstrate `0`, `2`, `1`, and combined scan/Cargo failure `1`.

- [ ] **Step 9: Commit the exit contract**

```bash
git add src/outcome.rs src/lib.rs src/main.rs src/daemon.rs src/cli.rs tests/cache_cleaner_daemon.rs tests/cli.rs
git commit -m "feat: expose complete incomplete and failed outcomes"
```

---

### Task 6: Document Slice B and run the repository gate

**Files:**

- Modify: `README.md:144-177`
- Modify: `docs/configuration.md:1-111`
- Modify: `docs/releases/v0.4.0.md:1-56`
- Modify: `tests/packaging.rs:114-139`

**Interfaces:**

- Consumes: the exact commands, keys, warnings, audit semantics, and codes implemented in Tasks 1-5.
- Produces: human and agent guidance that distinguishes incomplete coverage from failure and gives a safe legacy-key migration path.

- [ ] **Step 1: Strengthen the documentation contract test**

Update `configuration_reference_preserves_operational_contract` so the required terms include:

```rust
for value in [
    "scan_dirs",
    "project_dirs",
    "extra_excludes",
    "override_excludes",
    "config migrate",
    "unknown keys",
    "absolute",
    "exit `0`",
    "exit `1`",
    "exit `2`",
    "clean_interval",
    "scan_interval",
    "target_quiet_period",
    "log_level",
    "XDG_STATE_HOME",
    "linked worktrees",
    "discovery failure",
    "run --dry-run",
    "run --force",
    "car-go-clean.log",
] {
    assert!(guide.contains(value), "missing {value}");
}
```

Add assertions that `docs/releases/v0.4.0.md` contains `removed in v0.5`, `config migrate`, and the three exit-code meanings.

- [ ] **Step 2: Run the packaging test and confirm the current docs are stale**

Run:

```bash
cargo test --test packaging configuration_reference_preserves_operational_contract
```

Expected: FAIL because the docs still present `excludes` as the current primary key and do not describe strict loading or the exit taxonomy.

- [ ] **Step 3: Update the README summary**

Replace the configuration bullets with concise guidance:

```markdown
- `scan_dirs` and `project_dirs` define cleanup discovery scope and must expand
  to absolute paths.
- `extra_excludes` is the normal way to add discovery exclusions.
- `override_excludes` is an advanced option that replaces editable discovery
  defaults; protected-storage cleanup gates remain independent.
- The v0.4 binary still accepts legacy `excludes` with a warning. Run
  `car-go-clean config migrate` before v0.5.
- Unknown keys, unset path variables, unterminated `${NAME` expressions, and
  an empty effective scope are configuration errors.
```

Add a compact outcome note near one-shot usage:

```markdown
One-shot commands exit `0` for complete coverage, `2` for valid results with
incomplete discovery coverage, and `1` for failures. A macOS home scan can
legitimately return `2` when privacy-protected directories cannot be read.
```

- [ ] **Step 4: Rewrite the configuration reference sections precisely**

Document:

- the optional raw overlay over defaults;
- all supported keys and accepted duration/log-level values;
- expansion of `~`, `$NAME`, and `${NAME}` in every path-bearing field;
- absolute root requirements and relative-versus-absolute exclusion behavior;
- unknown-key, unset-variable, unterminated-variable, and empty-scope failures;
- `extra_excludes` as additive and `override_excludes` as advanced replacement;
- protected-storage classification as an independent cleanup gate;
- the v0.4 legacy warning, conflict rule, exact migration command, and v0.5 removal;
- `car-go-clean config > config.toml` as a supported round trip;
- successful-only recovery totals and failed attempt visibility in `stats` and `logs`;
- exit `0`, exit `2`, exit `1`, severity precedence, and safety skips staying `0`.

Use this migration example:

```sh
car-go-clean config migrate
# or
car-go-clean config migrate --config /absolute/path/config.toml
```

State that the command prints a unified diff before atomically replacing the same file and does nothing when no legacy key exists.

- [ ] **Step 5: Update the v0.4.0 release notes**

Add a `Configuration migration` section with:

```markdown
`excludes` still loads in v0.4 with a deprecation warning, but it is removed
in v0.5. It has the advanced replacement semantics of `override_excludes`.
Run `car-go-clean config migrate` to preview the key-only diff and rewrite the
file while preserving comments where TOML editing permits.
```

Add an `Automation and accounting` section that states:

- exit `0` is complete;
- exit `2` is valid but incomplete coverage and is expected for some broad macOS scans;
- exit `1` is a failure and outranks `2`;
- nonzero Cargo attempts are audited and visible, but never counted as successful cleans or recovered bytes.

Do not change the release tag or publication instructions in this task.

- [ ] **Step 6: Run formatting, linting, tests, and repository hygiene checks**

Run:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
make test
git diff --check
git status --short
```

Expected:

- rustfmt exits `0`;
- clippy exits `0` with no warnings;
- installer tests, release-note tests, unit tests, and integration tests all pass;
- `git diff --check` prints nothing;
- status lists only the intentional Slice B files.

- [ ] **Step 7: Perform the Slice B design audit**

Run:

```bash
rg -n "extra_excludes|override_excludes|config migrate|exit `0`|exit `1`|exit `2`|removed in v0.5" README.md docs/configuration.md docs/releases/v0.4.0.md
rg -n "exit_code = 0|exit_code <> 0" src/store.rs
rg -n "mark_project_cleaned|projects_cleaned|bytes_recovered|cargo clean exited" src/daemon.rs
git diff --stat 22e692c
git tag --list v0.4.0
```

Expected:

- current keys, migration, exit meanings, and v0.5 removal appear in operator docs;
- both recovery aggregates filter successful events and failed attempts have a count query;
- daemon success accounting occurs only in the `exit_code == 0` branch;
- the diff is limited to Slice B implementation, tests, and docs;
- `git tag --list v0.4.0` prints nothing.

- [ ] **Step 8: Commit the documentation and final gate**

```bash
git add README.md docs/configuration.md docs/releases/v0.4.0.md tests/packaging.rs
git commit -m "docs: explain strict config and command outcomes"
```

- [ ] **Step 9: Record the execution handoff without publishing**

Run:

```bash
git status --short --branch
git log --oneline --decorate -7
```

Expected: the worktree is clean and the six Slice B commits follow `22e692c`. Report the exact commit IDs, test commands, and outcomes. Stop before switching branches, pushing, tagging, releasing, changing the Homebrew tap, upgrading the local installation, or touching the installed daemon.

---

## Deferred Slice A Integration Points

Slice B deliberately leaves these concrete seams for the separately planned authority work:

- `RunCycleResult.coverage_incomplete` must OR in missing/mismatched policy generations, blocked cached rows, and origin-authority failures.
- `run --no-scan` must return `Incomplete` when no discovery generation matches the current policy hash.
- `ScanCycleResult.errors` remains the immediate scanner-error input; Slice A adds generation and origin results rather than changing scanner errors into fatal errors.
- The `CommandOutcome::merge` precedence remains unchanged when policy/generation results are added.
- `Config::effective_excludes()` supplies lexical inputs to the Slice A policy tuple; Slice A adds canonical exclusion identities and manager-root provenance.
- `Config::to_toml()` remains configuration-only after policy hash and protected-root diagnostics are added to `health` and `status`.
