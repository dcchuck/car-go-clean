# Cross-Platform Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `car-go-clean` v0.2.0 as verified public macOS and Linux binaries, installable through a Homebrew tap or a checksum-verifying shell installer, with daemon activation remaining an explicit CLI action.

**Architecture:** `cargo-dist` owns reproducible target archives, checksums, artifact attestations, GitHub Releases, and the Homebrew formula publication. A repository-owned POSIX shell installer consumes those release archives so it can offer the required `--version` and `--install-dir` interface. A new Rust service module embeds the existing launchd and systemd templates, renders the selected one with an absolute executable path, and routes lifecycle commands through a small, fakeable command boundary.

**Tech Stack:** Rust 1.95 / edition 2021, `clap`, `anyhow`, `tempfile`, POSIX `sh`, GitHub Actions, cargo-dist 0.32.0, Homebrew tap `dcchuck/homebrew-tap`.

## Global Constraints

- `Cargo.toml` remains the version source of truth; this implementation changes it from `0.1.0` to `0.2.0` and must not create or push the `v0.2.0` tag.
- Releases run only for a pushed annotated SemVer tag named `vX.Y.Z` whose version exactly matches `Cargo.toml`; ordinary `main` pushes never publish.
- Build exactly `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-musl`, and `x86_64-unknown-linux-musl`.
- Release archives, `SHA256SUMS`, and GitHub artifact provenance attestations are public GitHub Release assets.
- The shell installer uses HTTPS, verifies `SHA256SUMS`, and replaces only the binary atomically after verification; it never invokes `sudo`, changes config/state, or starts a daemon.
- Homebrew installs only the binary. `service install` is the sole opt-in action that enables and starts a daemon.
- Support only macOS and Linux in this release. On Linux, fail clearly when `systemctl --user` is unavailable; do not install another scheduler.
- Preserve `$XDG_CONFIG_HOME/car-go-clean/config.toml` / `$HOME/.config/car-go-clean/config.toml` and `$XDG_STATE_HOME/car-go-clean` / `$HOME/.local/state/car-go-clean` without rewriting them.
- Every service definition runs an absolute executable path; `service uninstall` touches only car-go-clean's own user-service file.

## File Map

- `Cargo.toml`: package release metadata, v0.2.0, package-local cargo-dist eligibility, and cargo-dist's generated release profile.
- `dist-workspace.toml`: cargo-dist 0.32's required workspace-level release targets, installers, GitHub attestation, tap, and publishing configuration.
- `.github/workflows/release.yml`: cargo-dist-generated tag-only build, archive, checksum, attestation, GitHub Release, Homebrew publishing pipeline, and calls to tracked reusable jobs.
- `.github/workflows/release-tag-gate.yml`: reusable pre-publish check that rejects lightweight, unprefixed, malformed, and Cargo-version-mismatched tags.
- `.github/workflows/release-preflight.yml`: cargo-dist host-stage verification before publishing.
- `.github/workflows/publish-shell-installer.yml`: cargo-dist publish-stage installer upload and provenance attestation.
- `.github/workflows/release-verify.yml`: cargo-dist post-announce checks of each released executable and the generated Homebrew formula.
- `.github/workflows/ci.yml`: non-publishing pull-request and `main` verification for Rust and installer contracts.
- `packaging/release/car-go-clean-installer.sh`: version-selectable POSIX installer attached to every release.
- `tests/installer.sh`: hermetic shell-installer contract tests with fake network tools and local archives.
- `Makefile`: a `test-installer` target included by `test` and CI.
- `src/service.rs`: service platform selection, escaping/rendering, path selection, lifecycle operations, and injectable process runner.
- `src/lib.rs`: exports the new service module for integration tests.
- `src/cli.rs`: `car-go-clean service {install,status,restart,uninstall}` parsing and human-readable result output.
- `packaging/launchd/com.dcchuck.car-go-clean.plist`: embedded macOS launchd template with only Rust-rendered placeholders.
- `packaging/systemd/car-go-clean.service`: embedded Linux systemd-user template with only Rust-rendered placeholders.
- `packaging/launchd/install.sh`: removed because service installation is now performed by the installed CLI.
- `tests/service.rs`: fake-runner lifecycle, rendering, absolute-binary, and unsupported-platform tests.
- `tests/cli.rs` and `tests/packaging.rs`: command parsing and repository packaging/release contract checks.
- `README.md`, `packaging/release/README.md`, and `docs/releasing.md`: end-user installation, explicit service activation, supported targets, and maintainer release procedure.

---

### Task 1: Establish v0.2.0 release metadata and generate the tag-only cargo-dist pipeline

**Files:**

- Modify: `Cargo.toml`
- Create: `dist-workspace.toml`
- Create: `.github/workflows/release.yml`
- Create: `.github/workflows/release-tag-gate.yml`
- Modify: `tests/packaging.rs`

**Interfaces:**

- Produces: a cargo-dist configuration that emits the four required target archives, `SHA256SUMS`, GitHub attestations, and a `dcchuck/homebrew-tap` formula.
- Produces: a tag workflow that accepts the unified cargo-dist announcement form `v<version>` and rejects any version mismatch during `dist plan --tag`.
- Consumes: the package version from `Cargo.toml`; no independent release-version file is introduced.

- [ ] **Step 1: Write failing release-configuration contract tests**

  Extend `tests/packaging.rs` with the following helpers and tests:

  ```rust
  #[test]
  fn cargo_dist_metadata_declares_the_public_release_contract() {
      let manifest = repo_file("Cargo.toml");
      let dist = repo_file("dist-workspace.toml");
      for value in [
          "version = \"0.2.0\"",
          "repository = \"https://github.com/dcchuck/car-go-clean\"",
          "homepage = \"https://github.com/dcchuck/car-go-clean\"",
      ] {
          assert!(manifest.contains(value), "missing {value}");
      }
      for value in [
          "cargo-dist-version = \"0.32.0\"",
          "aarch64-apple-darwin",
          "x86_64-apple-darwin",
          "aarch64-unknown-linux-musl",
          "x86_64-unknown-linux-musl",
          "github-attestations = true",
          "tap = \"dcchuck/homebrew-tap\"",
          "publish-jobs = [\"homebrew\"]",
      ] {
          assert!(dist.contains(value), "missing {value}");
      }
  }

  #[test]
  fn release_workflow_is_tag_only_and_uses_dist() {
      let workflow = repo_file(".github/workflows/release.yml");
      assert!(workflow.contains("push:"));
      assert!(workflow.contains("tags:"));
      assert!(!workflow.contains("pull_request:"));
      assert!(workflow.contains("dist plan"));
      assert!(workflow.contains("dist build"));
      assert!(workflow.contains("HOMEBREW_TAP_TOKEN"));
      assert!(workflow.contains("\"attestations\": \"write\""));
      assert!(workflow.contains("release-tag-gate"));
  }
  ```

- [ ] **Step 2: Run the packaging test to verify it fails**

  Run: `mise exec rust@1.95.0 -- cargo test --locked --test packaging`

  Expected: FAIL because the package is still v0.1.0 and no release workflow exists.

- [ ] **Step 3: Add release metadata and the cargo-dist configuration**

  Add the following package metadata to `Cargo.toml`, retaining existing dependencies unchanged:

  ```toml
  [package]
  version = "0.2.0"
  homepage = "https://github.com/dcchuck/car-go-clean"
  repository = "https://github.com/dcchuck/car-go-clean"
  readme = "README.md"

  [package.metadata.dist]
  dist = true
  ```

  cargo-dist 0.32 reads release-wide configuration only from its workspace config, so create `dist-workspace.toml` with the generated workspace member declaration plus this exact release contract:

  ```toml
  [workspace]
  members = ["cargo:."]

  [dist]
  cargo-dist-version = "0.32.0"
  ci = "github"
  installers = ["homebrew"]
  targets = [
      "aarch64-apple-darwin",
      "x86_64-apple-darwin",
      "aarch64-unknown-linux-musl",
      "x86_64-unknown-linux-musl",
  ]
  checksum = "sha256"
  github-attestations = true
  pr-run-mode = "skip"
  tap = "dcchuck/homebrew-tap"
  host-jobs = ["./release-tag-gate"]
  publish-jobs = ["homebrew"]
  ```

  Install the pinned dist binary outside the repository and generate the baseline workflow:

  ```bash
  curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/axodotdev/cargo-dist/releases/download/v0.32.0/cargo-dist-installer.sh | sh
  dist init --yes
  ```

  Keep the generated `.github/workflows/release.yml` tracked. Do not set `allow-dirty = ["ci"]`; later tasks add reusable-job configuration and regenerate this file from checked-in metadata instead of hand-editing it.

  Create `.github/workflows/release-tag-gate.yml` as a reusable workflow with a required string `plan` workflow-call input. It must check out the triggering tag with `fetch-depth: 0`, extract `TAG="$(jq -r '.announcement_tag' <<< "${{ inputs.plan }}")"`, and execute this gate before cargo-dist enters its host stage:

  ```bash
  case "$TAG" in
      v[0-9]*.[0-9]*.[0-9]*) ;;
      *) echo "release tag must be vX.Y.Z, got $TAG" >&2; exit 1 ;;
  esac
  VERSION="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "car-go-clean").version')"
  test "$TAG" = "v$VERSION" || { echo "tag $TAG does not match Cargo.toml version $VERSION" >&2; exit 1; }
  test "$(git cat-file -t "refs/tags/$TAG")" = tag || { echo "release tag must be annotated" >&2; exit 1; }
  ```

  Regenerate `.github/workflows/release.yml` with `dist init --yes` after adding the hook. This permits cargo-dist's normal tag planning while making an unprefixed, malformed, lightweight, or Cargo-version-mismatched tag fail before hosting or publishing.

- [ ] **Step 4: Verify the release plan selects only the matching v0.2.0 announcement**

  Run:

  ```bash
  dist plan --tag v0.2.0 --output-format=json > /tmp/car-go-clean-dist-plan.json
  jq -e '.announcement_tag == "v0.2.0"' /tmp/car-go-clean-dist-plan.json
  ! dist plan --tag v0.2.1 --output-format=json
  ```

  Expected: the first plan succeeds and lists all four required target triples; the second command fails because the tag version does not match `Cargo.toml`.

- [ ] **Step 5: Run the release-configuration test to verify it passes**

  Run: `mise exec rust@1.95.0 -- cargo test --locked --test packaging`

  Expected: PASS.

- [ ] **Step 6: Commit the release foundation**

  ```bash
  git add Cargo.toml Cargo.lock dist-workspace.toml .github/workflows/release.yml tests/packaging.rs
  git commit -m "feat: add v0.2.0 release pipeline"
  ```

### Task 2: Add the checksum-verifying, version-selectable shell installer

**Files:**

- Create: `packaging/release/car-go-clean-installer.sh`
- Create: `tests/installer.sh`
- Modify: `Makefile`
- Create: `.github/workflows/publish-shell-installer.yml`
- Modify: `Cargo.toml`
- Modify: `.github/workflows/release.yml`

**Interfaces:**

- Produces: `car-go-clean-installer.sh [--version X.Y.Z] [--install-dir PATH]`.
- Consumes: cargo-dist release assets named `car-go-clean-<version>-<target>.tar.xz` and their `SHA256SUMS` entry.
- Produces: a release asset named `car-go-clean-installer.sh`, uploaded after cargo-dist has published the tag's archives.
- Produces: `make test-installer`, which runs without network access.

- [ ] **Step 1: Write hermetic failing installer tests**

  Create `tests/installer.sh` with `set -eu`. It must create a temporary fixture archive containing an executable `car-go-clean`, prepend fake `uname`, `curl`, and `shasum` commands to `PATH`, and run the checked-in installer with `HOME` directed to the temporary directory. Include these assertions:

  ```sh
  run_installer --install-dir "$install_dir"
  test "$(cat "$install_dir/car-go-clean")" = "new binary"
  test "$(cat "$curl_log")" = "latest-meta v0.2.0 car-go-clean-0.2.0-aarch64-apple-darwin.tar.xz SHA256SUMS"

  run_installer --version 0.2.0 --install-dir "$versioned_dir"
  grep -qx 'v0.2.0 car-go-clean-0.2.0-aarch64-apple-darwin.tar.xz SHA256SUMS' "$curl_log"

  printf '%s' 'old binary' > "$failed_dir/car-go-clean"
  if CHECKSUM_MODE=wrong run_installer --install-dir "$failed_dir"; then
      exit 1
  fi
  test "$(cat "$failed_dir/car-go-clean")" = "old binary"
  ```

  Add an unsupported `uname -s` assertion that expects failure before a fake `curl` invocation, and an x86_64 Linux assertion that expects the `x86_64-unknown-linux-musl` archive.

- [ ] **Step 2: Run the shell test to verify it fails**

  Run: `sh tests/installer.sh`

  Expected: FAIL because `packaging/release/car-go-clean-installer.sh` does not exist.

- [ ] **Step 3: Implement the installer with fixed target and checksum rules**

  Create a POSIX `sh` script with this argument and target-selection structure:

  ```sh
  version=latest
  install_dir="$HOME/.local/bin"
  while [ "$#" -gt 0 ]; do
      case "$1" in
          --version) version=${2:?missing version}; shift 2 ;;
          --install-dir) install_dir=${2:?missing install directory}; shift 2 ;;
          *) echo "usage: $0 [--version X.Y.Z] [--install-dir PATH]" >&2; exit 2 ;;
      esac
  done

  case "$(uname -s):$(uname -m)" in
      Darwin:arm64) target=aarch64-apple-darwin ;;
      Darwin:x86_64) target=x86_64-apple-darwin ;;
      Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
      Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-musl ;;
      *) echo "unsupported platform: $(uname -s) $(uname -m)" >&2; exit 1 ;;
  esac

  case "$version" in
      latest)
          tag=$(curl --proto '=https' --tlsv1.2 -fsSIL -o /dev/null -w '%{url_effective}' \
              https://github.com/dcchuck/car-go-clean/releases/latest | sed -n 's#.*/tag/\(v[^/]*\)$#\1#p')
          [ -n "$tag" ] || { echo "could not resolve the latest release tag" >&2; exit 1; }
          ;;
      [0-9]*.[0-9]*.[0-9]*) tag="v$version" ;;
      *) echo "--version must be X.Y.Z" >&2; exit 2 ;;
  esac
  ```

  Set `release_version=${tag#v}`, `archive_name="car-go-clean-$release_version-$target.tar.xz"`, and `base_url="https://github.com/dcchuck/car-go-clean/releases/download/$tag"`. Download `SHA256SUMS` and `"$archive_name"` to a `mktemp -d` directory, and remove that directory with `trap 'rm -rf "$work_dir"' EXIT HUP INT TERM`. Extract the expected hash with `awk -v file="$archive_name" '$2 == file { print $1 }'`; require exactly one nonempty hash. Use `shasum -a 256` on macOS and `sha256sum` on Linux, compare exact digests, then extract with `tar -xJf`.

  Require exactly one extracted `car-go-clean` regular executable. Run `mkdir -p "$install_dir"`, write it as `"$install_dir/.car-go-clean.$$"` with `install -m 755`, and use `mv -f` only after extraction and checksum validation complete. Print the final binary path and the reminder:

  ```text
  Restart an explicitly installed daemon with: car-go-clean service restart
  ```

- [ ] **Step 4: Attach the installer and attest it through cargo-dist's publish hook**

  Create `.github/workflows/publish-shell-installer.yml` as a reusable workflow. Its `workflow_call` trigger accepts a required string input named `plan`. Its single job is named `publish-shell-installer`; cargo-dist grants it `contents: write`, `attestations: write`, and `id-token: write` through `github-custom-job-permissions`. Extract the tag with `jq -r '.announcement_tag' <<< "${{ inputs.plan }}"`, then execute:

  ```yaml
  - uses: actions/checkout@v4
    with:
      persist-credentials: false
  - name: Stage installer
    run: cp packaging/release/car-go-clean-installer.sh car-go-clean-installer.sh
  - name: Attest installer
    uses: actions/attest-build-provenance@v2
    with:
      subject-path: car-go-clean-installer.sh
  - name: Upload installer
    env:
      GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
    run: gh release upload "$TAG" car-go-clean-installer.sh --clobber
  ```

  Preserve the generated cargo-dist jobs and their `announce` job. This reusable publish hook must not push a tag or invoke Homebrew.

  Add these fields beneath the existing cargo-dist configuration and regenerate the workflow instead of editing it:

  ```toml
  publish-jobs = ["homebrew", "./publish-shell-installer"]
  github-custom-job-permissions = { "publish-shell-installer" = { contents = "write", attestations = "write", id-token = "write" } }
  ```

  ```bash
  dist init --yes
  ```

- [ ] **Step 5: Make shell tests part of normal local verification**

  Extend `Makefile` exactly as follows:

  ```make
  .PHONY: build test test-installer fmt clippy clean

  test: test-installer
	$(CARGO) test

  test-installer:
	sh tests/installer.sh
  ```

  Keep `CARGO ?= cargo`; do not make `test-installer` download a release.

- [ ] **Step 6: Run the installer test to verify it passes**

  Run: `make test-installer`

  Expected: PASS for default latest, pinned version, macOS and Linux target selection, rejected checksum, and unsupported platform behavior.

- [ ] **Step 7: Commit the direct installer**

  ```bash
  git add Cargo.toml packaging/release/car-go-clean-installer.sh tests/installer.sh Makefile \
    .github/workflows/publish-shell-installer.yml .github/workflows/release.yml
  git commit -m "feat: add verified shell installer"
  ```

### Task 3: Build the shared, testable service-management layer

**Files:**

- Create: `src/service.rs`
- Modify: `src/lib.rs`
- Modify: `packaging/launchd/com.dcchuck.car-go-clean.plist`
- Modify: `packaging/systemd/car-go-clean.service`
- Delete: `packaging/launchd/install.sh`
- Create: `tests/service.rs`
- Modify: `tests/packaging.rs`

**Interfaces:**

- Produces: `pub enum ServiceAction { Install, Status, Restart, Uninstall }` and `pub enum ServicePlatform { MacOs, Linux }`.
- Produces: `pub struct ServiceManager<R: CommandRunner>` with `install`, `status`, `restart`, `uninstall`, and `into_runner(self) -> R` methods returning `Result<ServiceStatus>` for lifecycle calls.
- Produces: `pub trait CommandRunner { fn run(&mut self, program: &Path, args: &[OsString]) -> Result<CommandOutput>; }` and `SystemCommandRunner` for production.
- Produces: `pub fn resolve_service_binary(argv0: &OsStr, path: Option<&OsStr>, current_exe: PathBuf) -> Result<PathBuf>`.
- Consumes: the embedded template files via `include_str!`; the runtime binary never depends on a source checkout.

- [ ] **Step 1: Write failing renderer and lifecycle tests behind a fake runner**

  Create `tests/service.rs` with a `FakeRunner` that records `(program, args)` calls and returns configurable success. Add these focused tests:

  ```rust
  #[test]
  fn mac_install_renders_an_escaped_absolute_binary_and_bootstraps_the_agent() {
      let work = tempfile::tempdir().unwrap();
      let binary = work.path().join("bin/car & go-clean");
      let mut manager = test_manager(ServicePlatform::MacOs, work.path(), binary.clone());

      manager.install().unwrap();
      let runner = manager.into_runner();

      let plist = fs::read_to_string(work.path()
          .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist")).unwrap();
      assert!(plist.contains(&binary.display().to_string().replace('&', "&amp;")));
      assert_eq!(runner.calls[0].1[0], format!("gui/{}", unsafe { libc::geteuid() }));
      assert_eq!(runner.calls[1].1[0], format!("gui/{}/com.dcchuck.car-go-clean", unsafe { libc::geteuid() }));
  }

  #[test]
  fn linux_install_writes_user_unit_and_enables_it_without_sudo() {
      let work = tempfile::tempdir().unwrap();
      let mut manager = test_manager(ServicePlatform::Linux, work.path(), work.path().join("bin/car-go-clean"));

      manager.install().unwrap();
      let runner = manager.into_runner();

      let unit = fs::read_to_string(work.path().join(".config/systemd/user/car-go-clean.service")).unwrap();
      assert!(unit.contains("ExecStart="));
      assert!(unit.contains("daemon"));
      assert!(runner.calls.iter().any(|(_, args)| args.iter().map(|value| value.to_string_lossy()).collect::<Vec<_>>() == ["--user", "enable", "--now", "car-go-clean.service"]));
      assert!(!runner.calls.iter().any(|(program, _)| program == Path::new("sudo")));
  }
  ```

  Add tests that `restart` uses `launchctl kickstart -k` or `systemctl --user restart`; `uninstall` stops/removes only the expected service file; `status` returns `installed: false` without running a platform command when the file is absent; a missing `systemctl --user show-environment` returns an error containing `systemd --user is unavailable`; and a PATH-resolved absolute binary wins over `current_exe`.

- [ ] **Step 2: Run the service test to verify it fails**

  Run: `mise exec rust@1.95.0 -- cargo test --locked --test service`

  Expected: FAIL because `car_go_clean::service` does not exist.

- [ ] **Step 3: Implement escaping, path selection, and command execution**

  In `src/service.rs`, embed the templates and keep all OS command invocation in `SystemCommandRunner`:

  ```rust
  const LABEL: &str = "com.dcchuck.car-go-clean";
  const LAUNCHD_TEMPLATE: &str = include_str!("../packaging/launchd/com.dcchuck.car-go-clean.plist");
  const SYSTEMD_TEMPLATE: &str = include_str!("../packaging/systemd/car-go-clean.service");

  pub trait CommandRunner {
      fn run(&mut self, program: &Path, args: &[OsString]) -> Result<CommandOutput>;
  }
  ```

  Implement `xml_escape` for `&`, `<`, `>`, `\"`, and `'`. Implement `systemd_quote` by double-quoting an argument and escaping `\\`, `\"`, and `%`; use it only for individual `ExecStart` arguments. Render the launchd plist by replacing `__CAR_GO_CLEAN_BIN__` and `__CAR_GO_CLEAN_LOG_DIR__`; render the systemd unit by replacing only `__CAR_GO_CLEAN_BIN__` with the quoted absolute binary argument. Reject a non-absolute binary before rendering.

  Implement `resolve_service_binary` in this order: an absolute `argv0` that exists, an absolute executable found for `argv0` in the supplied `PATH`, then `current_exe`. Convert a relative `argv0` to an absolute path from `current_dir` before checking it. Return a contextual error if the final path is not absolute or does not exist.

- [ ] **Step 4: Implement the macOS and Linux lifecycle operations**

  Use `libc::geteuid()` to construct `gui/<uid>` on macOS. `install` must atomically write `~/Library/LaunchAgents/com.dcchuck.car-go-clean.plist`, create `~/Library/Logs/car-go-clean`, attempt `launchctl bootout gui/<uid> <plist>` without treating an absent prior service as fatal, then run `launchctl bootstrap gui/<uid> <plist>` and `launchctl kickstart -k gui/<uid>/com.dcchuck.car-go-clean`. `restart` runs only the final `kickstart -k`; `uninstall` attempts `bootout`, then removes exactly that plist; `status` calls `launchctl print gui/<uid>/com.dcchuck.car-go-clean` only when that plist exists.

  On Linux, first require a successful `systemctl --user show-environment`. `install` atomically writes `~/.config/systemd/user/car-go-clean.service`, then runs `systemctl --user daemon-reload` followed by `systemctl --user enable --now car-go-clean.service`. `restart` runs `systemctl --user restart car-go-clean.service`. `uninstall` runs `systemctl --user disable --now car-go-clean.service`, accepts an already-missing service, removes exactly that unit, and finishes with `systemctl --user daemon-reload`. `status` runs `systemctl --user status --no-pager car-go-clean.service` only when the unit exists.

  Export the module in `src/lib.rs`:

  ```rust
  pub mod service;
  ```

- [ ] **Step 5: Replace the source-checkout launchd installer with embedded templates**

  Keep the launchd template's `ProgramArguments`, `RunAtLoad`, `KeepAlive`, and log paths. Remove the shell installer because no installed binary can depend on files under `packaging/`. Change the systemd template to this direct absolute-executable form:

  ```ini
  [Unit]
  Description=Run car-go-clean daemon
  Documentation=https://github.com/dcchuck/car-go-clean
  After=network.target

  [Service]
  Type=simple
  ExecStart=__CAR_GO_CLEAN_BIN__ daemon
  Restart=on-failure
  RestartSec=30s

  [Install]
  WantedBy=default.target
  ```

  Update `tests/packaging.rs` to assert the templates retain their placeholders and that `packaging/launchd/install.sh` is absent.

- [ ] **Step 6: Run focused service and packaging tests to verify they pass**

  Run:

  ```bash
  mise exec rust@1.95.0 -- cargo test --locked --test service
  mise exec rust@1.95.0 -- cargo test --locked --test packaging
  ```

  Expected: PASS, with no real launchd or systemd invocation.

- [ ] **Step 7: Commit the service module**

  ```bash
  git add src/service.rs src/lib.rs packaging/launchd/com.dcchuck.car-go-clean.plist \
    packaging/systemd/car-go-clean.service tests/service.rs tests/packaging.rs
  git rm packaging/launchd/install.sh
  git commit -m "feat: manage user services from the cli"
  ```

### Task 4: Expose explicit service commands through the CLI

**Files:**

- Modify: `src/cli.rs`
- Modify: `tests/cli.rs`

**Interfaces:**

- Consumes: `service::ServiceAction`, `service::ServiceManager`, and `service::resolve_service_binary`.
- Produces: `car-go-clean service install`, `status`, `restart`, and `uninstall`.
- Produces: stable output containing the platform, the resolved absolute binary path, the definition path, and the lifecycle state.

- [ ] **Step 1: Write failing CLI parsing and help tests**

  Add these tests to `tests/cli.rs`:

  ```rust
  #[test]
  fn service_help_lists_only_explicit_lifecycle_actions() {
      Command::cargo_bin("car-go-clean")
          .unwrap()
          .args(["service", "--help"])
          .assert()
          .success()
          .stdout(contains("install"))
          .stdout(contains("status"))
          .stdout(contains("restart"))
          .stdout(contains("uninstall"));
  }

  #[test]
  fn top_level_help_lists_service_management() {
      Command::cargo_bin("car-go-clean")
          .unwrap()
          .arg("--help")
          .assert()
          .success()
          .stdout(contains("service"));
  }
  ```

- [ ] **Step 2: Run the CLI test to verify it fails**

  Run: `mise exec rust@1.95.0 -- cargo test --locked --test cli service_help`

  Expected: FAIL because `service` is not a recognised command.

- [ ] **Step 3: Add the command group and dispatch it to the service module**

  Add these clap definitions alongside the existing command enums:

  ```rust
  #[derive(Debug, Subcommand)]
  enum ServiceCommands {
      Install,
      Status,
      Restart,
      Uninstall,
  }

  enum Commands {
      // existing variants
      Service {
          #[command(subcommand)]
          command: ServiceCommands,
      },
  }
  ```

  In `execute`, map each subcommand to its matching `ServiceAction`, resolve the binary through `std::env::args_os().next()`, `std::env::var_os("PATH")`, and `std::env::current_exe()`, then construct `ServiceManager<SystemCommandRunner>` for `std::env::consts::OS`. Print exactly these labels from the returned status:

  ```text
  Service
    Platform: macOS (launchd) | Linux (systemd --user)
    Binary: /absolute/path/to/car-go-clean
    Definition: /absolute/path/to/service-file
    State: installed | not installed | running | stopped
  ```

  For any other OS, return `car-go-clean service is supported only on macOS and Linux`. Never call a service operation from `install`, `scan`, `run`, `daemon`, or `version`.

- [ ] **Step 4: Run the CLI tests to verify they pass**

  Run: `mise exec rust@1.95.0 -- cargo test --locked --test cli service`

  Expected: PASS. The tests invoke only help text; lifecycle behavior remains covered by `tests/service.rs` with a fake runner.

- [ ] **Step 5: Commit the command interface**

  ```bash
  git add src/cli.rs tests/cli.rs
  git commit -m "feat: add service lifecycle commands"
  ```

### Task 5: Add publishing preflight, release verification, and the public Homebrew tap prerequisite

**Files:**

- Create: `.github/workflows/release-preflight.yml`
- Create: `.github/workflows/ci.yml`
- Create: `.github/workflows/release-verify.yml`
- Modify: `Cargo.toml`
- Modify: `.github/workflows/release.yml`
- Modify: `tests/packaging.rs`

**Interfaces:**

- Consumes: cargo-dist's generated plan and release stages plus the `host-jobs`, `publish-jobs`, and `post-announce-jobs` metadata configured in Task 1.
- Produces: a required host-stage preflight, a publish-stage shell-installer hook, and post-announce smoke checks without hand-editing cargo-dist's generated workflow.
- Produces: a public `dcchuck/homebrew-tap` repository, with formula updates published only by cargo-dist using `HOMEBREW_TAP_TOKEN`.

- [ ] **Step 1: Write failing workflow-contract assertions**

  Extend `tests/packaging.rs`:

  ```rust
  #[test]
  fn ci_and_release_verification_cover_installable_artifacts() {
      let ci = repo_file(".github/workflows/ci.yml");
      let release = repo_file(".github/workflows/release.yml");
      let preflight = repo_file(".github/workflows/release-preflight.yml");
      let verify = repo_file(".github/workflows/release-verify.yml");

      assert!(ci.contains("cargo test --locked"));
      assert!(ci.contains("cargo clippy --all-targets --locked -- -D warnings"));
      assert!(ci.contains("make test-installer"));
      assert!(preflight.contains("cargo fmt --all -- --check"));
      assert!(release.contains("publish-shell-installer"));
      assert!(release.contains("release-preflight"));
      assert!(verify.contains("health --skip-cargo"));
      assert!(verify.contains("brew audit --strict Formula/car-go-clean.rb"));
  }
  ```

- [ ] **Step 2: Run the packaging test to verify it fails**

  Run: `mise exec rust@1.95.0 -- cargo test --locked --test packaging ci_and_release`

  Expected: FAIL because neither CI workflow exists and the generated release workflow has no repository-specific verification jobs.

- [ ] **Step 3: Add pull-request CI and release preflight**

  Create `.github/workflows/ci.yml` for `pull_request` and pushes to `main`. Its single Ubuntu job must check out the repository, install Rust 1.95.0 and dist 0.32.0, then run, in order:

  ```bash
  cargo fmt --all -- --check
  cargo clippy --all-targets --locked -- -D warnings
  cargo test --locked
  make test-installer
  dist plan --tag v0.2.0 --output-format=json
  ```

  Create `.github/workflows/release-preflight.yml` as a reusable workflow with a required string `plan` workflow-call input. Its `preflight` job checks out the tagged commit, installs Rust 1.95.0 and dist 0.32.0, then runs the first four commands above followed by this exact tag check:

  ```bash
  TAG="$(jq -r '.announcement_tag' <<< "${{ inputs.plan }}")"
  dist plan --tag "$TAG" --output-format=json
  ```

  Because Task 1 registers this file in `host-jobs`, cargo-dist will require it before entering its host stage. This keeps the generated release workflow clean while preventing publishing when formatting, strict Clippy, locked tests, installer contracts, or the tag/version plan fail.

- [ ] **Step 4: Add post-publication architecture smoke checks and formula verification**

  Create `.github/workflows/release-verify.yml` as a reusable workflow with a required string `plan` workflow-call input. Define a matrix with these exact target/runner pairs:

  ```yaml
  matrix:
    include:
      - target: aarch64-apple-darwin
        runner: macos-14
      - target: x86_64-apple-darwin
        runner: macos-13
      - target: aarch64-unknown-linux-musl
        runner: ubuntu-24.04-arm
      - target: x86_64-unknown-linux-musl
        runner: ubuntu-24.04
  ```

  Each matrix job sets `TAG="$(jq -r '.announcement_tag' <<< "${{ inputs.plan }}")"` and `VERSION="${TAG#v}"`, downloads `car-go-clean-${VERSION}-${{ matrix.target }}.tar.xz` and `SHA256SUMS` from that GitHub Release, verifies the archive with `shasum -a 256 -c SHA256SUMS` on macOS or `sha256sum -c SHA256SUMS` on Linux, extracts it, then runs:

  ```bash
  ./car-go-clean version
  ./car-go-clean health --skip-cargo
  ```

  Add a formula job that clones `https://github.com/dcchuck/homebrew-tap`, runs `brew audit --strict Formula/car-go-clean.rb`, and checks the formula contains the current release tag and every SHA-256 listed for the macOS and Linux archive assets. The formula job must depend on all smoke jobs so the cargo-dist post-announce stage reports failure if any released binary or its generated formula fails verification.

  Register both reusable workflows and regenerate the cargo-dist workflow:

  ```toml
  host-jobs = ["./release-tag-gate", "./release-preflight"]
  post-announce-jobs = ["./release-verify"]
  ```

  ```bash
  dist init --yes
  ```

- [ ] **Step 5: Create and authorize the public tap before the first tag**

  Create the empty public repository once:

  ```bash
  gh repo create dcchuck/homebrew-tap --public --description "Homebrew formulae for dcchuck tools" --add-readme
  ```

  Before pushing `v0.2.0`, create a fine-grained GitHub personal access token limited to repository contents read/write for `dcchuck/homebrew-tap`, then store it without printing it:

  ```bash
  printf %s "$HOMEBREW_TAP_TOKEN" | gh secret set HOMEBREW_TAP_TOKEN --repo dcchuck/car-go-clean
  ```

  Do not store a token in the repository. cargo-dist's Homebrew publish job is the only process that writes `Formula/car-go-clean.rb`.

- [ ] **Step 6: Run the workflow-contract test to verify it passes**

  Run: `mise exec rust@1.95.0 -- cargo test --locked --test packaging ci_and_release`

  Expected: PASS.

- [ ] **Step 7: Commit automated verification**

  ```bash
  git add Cargo.toml .github/workflows/release.yml \
    .github/workflows/release-preflight.yml .github/workflows/ci.yml \
    .github/workflows/release-verify.yml tests/packaging.rs
  git commit -m "ci: verify released installers and formula"
  ```

### Task 6: Document installation, explicit daemon activation, and the v0.2.0 release procedure

**Files:**

- Modify: `README.md`
- Modify: `packaging/release/README.md`
- Create: `docs/releasing.md`
- Modify: `tests/packaging.rs`

**Interfaces:**

- Produces: README-first installation commands for Homebrew and the shell installer, followed by Cargo as the developer install route.
- Produces: an explicit service-management section that documents no background daemon starts during binary install or upgrade.
- Produces: a maintainer runbook that starts from a verified v0.2.0 commit and ends with an annotated pushed tag.

- [ ] **Step 1: Write failing documentation contract tests**

  Add this test to `tests/packaging.rs`:

  ```rust
  #[test]
  fn readme_documents_binary_installs_and_explicit_service_activation() {
      let readme = repo_file("README.md");
      assert!(readme.contains("brew install dcchuck/tap/car-go-clean"));
      assert!(readme.contains("car-go-clean-installer.sh"));
      assert!(readme.contains("car-go-clean service install"));
      assert!(readme.contains("car-go-clean service restart"));
      assert!(readme.contains("does not start the daemon"));
  }
  ```

- [ ] **Step 2: Run the documentation test to verify it fails**

  Run: `mise exec rust@1.95.0 -- cargo test --locked --test packaging readme_documents`

  Expected: FAIL because the README still presents source installation as the primary release path.

- [ ] **Step 3: Rewrite the install and service sections around released binaries**

  Place these commands immediately after the project description in `README.md`:

  ```sh
  brew install dcchuck/tap/car-go-clean
  brew upgrade car-go-clean
  ```

  ```sh
  curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh | sh
  ```

  Document `--version 0.2.0` and `--install-dir "$HOME/.local/bin"`, all four supported targets, and the `SHA256SUMS` verification behavior. State directly that both installation paths install or upgrade only the binary and do not start the daemon.

  Add a separate Explicit Service Activation section containing all four commands and the restart-after-upgrade rule:

  ```sh
  car-go-clean service install
  car-go-clean service status
  car-go-clean service restart
  car-go-clean service uninstall
  ```

  Keep `cargo install --path .` under a Developer Installation heading. Replace the obsolete source-checkout launchd-installer instructions with the CLI commands.

- [ ] **Step 4: Add the maintainer runbook**

  Create `docs/releasing.md`. Require a clean checkout, the full local verification suite, an existing public tap, and the `HOMEBREW_TAP_TOKEN` secret before release. Include this exact first-release sequence, but do not execute it as part of this task:

  ```bash
  mise exec rust@1.95.0 -- cargo fmt --all -- --check
  mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
  mise exec rust@1.95.0 -- cargo test --locked
  make test-installer
  dist plan --tag v0.2.0 --output-format=json
  git tag -a v0.2.0 -m "car-go-clean v0.2.0"
  git push origin main v0.2.0
  ```

  Document that the workflow publishes the four archives, `SHA256SUMS`, provenance attestations, `car-go-clean-installer.sh`, and the Homebrew formula; it does not publish to crates.io or enable any daemon. Link to the GitHub Release verification workflow and state that a failed post-publication check must be investigated before announcing the release.

- [ ] **Step 5: Update release packaging notes**

  Replace `packaging/release/README.md`'s statement that Cargo is the primary distribution channel with the tag-only GitHub Release, Homebrew tap, and checksum-verifying shell installer contract. Retain Cargo installation solely as the source/developer path. Include the exact supported target triples and explicit daemon-install policy.

- [ ] **Step 6: Run documentation contract tests and the full local suite**

  Run:

  ```bash
  mise exec rust@1.95.0 -- cargo test --locked --test packaging
  mise exec rust@1.95.0 -- cargo fmt --all -- --check
  mise exec rust@1.95.0 -- cargo clippy --all-targets --locked -- -D warnings
  mise exec rust@1.95.0 -- cargo test --locked
  make test-installer
  ```

  Expected: every command exits 0.

- [ ] **Step 7: Commit documentation and release operations**

  ```bash
  git add README.md packaging/release/README.md docs/releasing.md tests/packaging.rs
  git commit -m "docs: publish cross-platform installation guide"
  ```

## Plan Self-Review

### Spec coverage

- Public GitHub Release, four exact target triples, SHA-256 manifest, and provenance: Tasks 1 and 5.
- Public `dcchuck/homebrew-tap`, formula publication, and least-privilege secret handling: Tasks 1 and 5.
- HTTPS shell installer, OS/CPU selection, `--version`, `--install-dir`, checksum rejection, atomic replacement, no `sudo`, and no daemon start: Task 2.
- Explicit macOS launchd and Linux systemd-user lifecycle with no alternative Linux scheduler: Tasks 3 and 4.
- Existing XDG config/state preservation and absolute binary invocation: Tasks 3 and 4.
- PR CI, tag pre-publication verification, extracted-binary smoke checks, and formula checks: Task 5.
- User-facing install, restart-after-upgrade, and maintainer tag documentation: Task 6.

### Placeholder scan

The plan contains no unfinished-work markers or unspecified test steps. Each task names concrete files, interfaces, commands, and expected outcomes.

### Type consistency

`ServiceAction`, `ServicePlatform`, `CommandRunner`, `ServiceManager`, `ServiceStatus`, and `resolve_service_binary` are introduced in Task 3 and consumed by Task 4 with the same names. The release asset name `car-go-clean-installer.sh`, service label `com.dcchuck.car-go-clean`, and systemd unit name `car-go-clean.service` are used consistently throughout the plan.
