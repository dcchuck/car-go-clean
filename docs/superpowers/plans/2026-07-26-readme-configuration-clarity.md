# README clarity implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the README compact and scannable by using a cropped logo asset and moving advanced configuration semantics to a dedicated guide.

**Architecture:** Preserve the original artwork and create a README-only cropped derivative. Keep the README as a quick-start with concise configuration and safety summaries; move the detailed worktree-discovery, durable-blocking, state, and log semantics into `docs/configuration.md`. Packaging tests treat the README and guide as a documentation contract.

**Tech Stack:** Markdown, PNG artwork, ImageGen for the logo derivative, Rust integration tests in `tests/packaging.rs`.

## Global Constraints

- Preserve `assets/car-go-clean-logo.png` unchanged; the cropped asset is a separate file.
- The README header must use the cropped asset at a visibly smaller width and no blank Markdown line between its closing `</p>` and `# car-go-clean`.
- Do not change cleanup policy or weaken any safety guarantee while relocating prose.
- The README must retain the config location and minimal TOML example, then link relatively to `docs/configuration.md`.
- The guide must document `scan_dirs`, `project_dirs`, `excludes`, `clean_interval`, `scan_interval`, `target_quiet_period`, `log_level`, XDG state paths, log rotation, linked worktrees, discovery failure blocking, and review/override commands.
- Use relative repository links only; do not introduce new dependencies or runtime behavior.

---

### Task 1: Create the compact README logo header

**Files:**
- Create: `assets/car-go-clean-logo-readme.png`
- Modify: `README.md:1-6`
- Modify: `tests/packaging.rs`

**Interfaces:**
- Consumes: original `assets/car-go-clean-logo.png` artwork.
- Produces: a README header that references `assets/car-go-clean-logo-readme.png` at `width="440"` and leaves the original asset intact.
- Produces: `readme_uses_compact_logo_asset`, a documentation contract test.

- [ ] **Step 1: Write the failing header-contract test**

  Add this test to `tests/packaging.rs`:

  ```rust
  #[test]
  fn readme_uses_compact_logo_asset() {
      let root = Path::new(env!("CARGO_MANIFEST_DIR"));
      let readme = repo_file("README.md");

      assert!(root.join("assets/car-go-clean-logo.png").is_file());
      assert!(root.join("assets/car-go-clean-logo-readme.png").is_file());
      assert!(readme.contains("assets/car-go-clean-logo-readme.png"));
      assert!(readme.contains("width=\"440\""));
      assert!(!readme.contains("width=\"640\""));
      assert!(readme.contains("</p>\n# car-go-clean"));
  }
  ```

- [ ] **Step 2: Run the focused test to verify it fails**

  Run:

  ```bash
  mise exec rust@1.95.0 -- cargo test --locked --test packaging readme_uses_compact_logo_asset
  ```

  Expected: FAIL because `assets/car-go-clean-logo-readme.png` and its README
  reference do not exist.

- [ ] **Step 3: Create the cropped derivative and update the header**

  Use ImageGen with `assets/car-go-clean-logo.png` as the reference image. Ask
  for a tightly cropped PNG containing the existing crab and `car-go-clean`
  wordmark only, with a small even margin and no changes to lettering, colors,
  illustration, or style. Save the result as
  `assets/car-go-clean-logo-readme.png`.

  Inspect the generated asset visually. It must retain the complete crab and
  wordmark, but remove the broad empty canvas that causes GitHub to separate
  the artwork from the title.

  Replace the first six README lines with:

  ```html
  <p align="center">
    <img src="assets/car-go-clean-logo-readme.png" alt="car-go-clean crab logo" width="440">
  </p>
  # car-go-clean
  ```

- [ ] **Step 4: Verify the compact header**

  Run:

  ```bash
  mise exec rust@1.95.0 -- cargo test --locked --test packaging readme_uses_compact_logo_asset
  sips -g pixelWidth -g pixelHeight assets/car-go-clean-logo-readme.png
  ```

  Expected: the Rust test passes and the new file is a readable PNG whose
  dimensions are smaller than the original `1536x1024` canvas.

- [ ] **Step 5: Commit the header change**

  ```bash
  git add assets/car-go-clean-logo-readme.png README.md tests/packaging.rs
  git commit -m "docs: compact README header"
  ```

### Task 2: Split advanced configuration detail into a reference guide

**Files:**
- Create: `docs/configuration.md`
- Modify: `README.md:81-208`
- Modify: `tests/packaging.rs`

**Interfaces:**
- Consumes: the existing README's configuration, worktree-discovery,
  safe-cleaning, state, and log guarantees plus `src/config.rs` defaults.
- Produces: a concise README Configuration section and a complete linked
  configuration reference.
- Produces: `configuration_reference_preserves_operational_contract`, a test
  that asserts both documents expose the required user-facing terms.

- [ ] **Step 1: Write the failing documentation-contract test**

  Add this test to `tests/packaging.rs`:

  ```rust
  #[test]
  fn configuration_reference_preserves_operational_contract() {
      let root = Path::new(env!("CARGO_MANIFEST_DIR"));
      let readme = repo_file("README.md");
      let guide = repo_file("docs/configuration.md");

      assert!(root.join("docs/configuration.md").is_file());
      assert!(readme.contains("[Configuration reference](docs/configuration.md)"));
      for value in [
          "scan_dirs",
          "project_dirs",
          "excludes",
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
  }
  ```

- [ ] **Step 2: Run the focused test to verify it fails**

  Run:

  ```bash
  mise exec rust@1.95.0 -- cargo test --locked --test packaging configuration_reference_preserves_operational_contract
  ```

  Expected: FAIL because `docs/configuration.md` and the README reference are
  absent.

- [ ] **Step 3: Write the detailed configuration reference**

  Create `docs/configuration.md` with these sections and user-facing content:

  ````markdown
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
  `warn`, or `error`. Tilde and environment variables expand in `scan_dirs`
  and `project_dirs`.

  ## Scan scope

  - `scan_dirs` lists roots to discover Rust projects. The default is `$HOME`.
  - `project_dirs` lists explicit projects, including projects outside scan
    roots.
  - `excludes` omits matching paths. Exclusions always win, including over an
    explicit `project_dirs` entry.

  ## Linked worktrees and discovery failures

  When a scan finds a primary Git checkout, car-go-clean asks Git for linked
  Rust worktrees within the configured scan roots, even when ignore rules hide
  them. Exclusions and the ordinary cleaning safeguards still apply. A successful
  enumeration reconciles stale cached candidates and replaces the exact
  primary's saved linked-worktree association.

  A failed enumeration is recorded as a normal scan error. Separately, the
  canonical primary and the linked worktrees saved for that primary remain
  blocked until a later successful enumeration replaces that association. The
  durable block normally does not spread to ancestors, siblings, or unrelated
  projects. Persisted primary and linked identities are retained conservatively:
  a changed alias cannot transfer or clear an old failure, and unresolved or
  noncanonical legacy associations remain blocked rather than being inferred
  from a reused path spelling.

  ## Cleaning policy and overrides

  A cached project is eligible only when its direct `project/target` exists,
  is readable and measurable, has no newer non-symlink file than
  `target_quiet_period`, is outside known managed cache/container storage, has
  no related unreadable scan path, and has no running process inside the
  project or `target/`. Canonicalization keeps cache/container classification
  physical without rewriting immutable worktree provenance. Native non-UTF-8
  Rust compiler path options still protect the matching canonical project.

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
  ````

  Replace the existing multi-paragraph README Configuration and Safe Cleaning
  detail with a compact Configuration section containing the existing config
  paths and TOML example, followed by these bullets:

  ```markdown
  - `scan_dirs` controls discovery roots; the default is `$HOME`.
  - `project_dirs` can add explicit projects outside those roots; `excludes`
    always wins.
  - Git-reported linked worktrees are discovered conservatively. A discovery
    failure blocks the affected primary/worktree set until a later success.
  - Review before cleanup with `car-go-clean run --dry-run`.

  See the [Configuration reference](docs/configuration.md) for the complete
  safety, worktree, state, log, and scheduler behavior.
  ```

  Retain the commands table, installation instructions, service section, and
  all existing cleanup behavior. Keep only a short Safe Cleaning Model lead-in
  plus its safety-gate bullet list in the README; move canonicalization and
  non-UTF-8 parser details to the guide.

- [ ] **Step 4: Run documentation and full checks**

  Run:

  ```bash
  mise exec rust@1.95.0 -- cargo test --locked --test packaging
  mise exec rust@1.95.0 -- cargo fmt --all -- --check
  mise exec rust@1.95.0 -- cargo test --locked
  git diff --check
  ```

  Expected: all commands exit 0, the README is a quick-start, and the guide
  contains the detailed configuration contract.

- [ ] **Step 5: Commit the documentation split**

  ```bash
  git add README.md docs/configuration.md tests/packaging.rs
  git commit -m "docs: add configuration reference"
  ```

## Plan self-review

### Spec coverage

- Cropped derivative preserves the original artwork and tightens the README
  header: Task 1.
- README keeps common configuration information but removes dense operational
  prose: Task 2.
- Configuration, worktree, safety, state, and log guarantees remain documented
  in a dedicated guide: Task 2.
- Documentation behavior is protected by repository tests: Tasks 1 and 2.

### Placeholder scan

No task contains TBD work, unspecified tests, or unnamed files. The guide
outline names every required configuration behavior, and each test uses the
actual file paths and terms it validates.

### Type consistency

The only new test names are `readme_uses_compact_logo_asset` and
`configuration_reference_preserves_operational_contract`; both live in
`tests/packaging.rs` and use the existing `repo_file` helper.
