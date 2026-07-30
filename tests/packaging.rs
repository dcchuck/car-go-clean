use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;
use yaml_rust2::{Yaml, YamlLoader};

fn repo_file(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

fn workflow(path: &str) -> Yaml {
    let documents = YamlLoader::load_from_str(&repo_file(path)).unwrap();
    assert_eq!(documents.len(), 1);
    documents.into_iter().next().unwrap()
}

fn workflow_steps<'a>(document: &'a Yaml, job: &str) -> &'a [Yaml] {
    document["jobs"][job]["steps"].as_vec().unwrap()
}

fn run_command(step: &Yaml) -> Option<&str> {
    step["run"].as_str()
}

fn step_running<'a>(steps: &'a [Yaml], command: &str) -> (usize, &'a Yaml) {
    steps
        .iter()
        .enumerate()
        .find(|(_, step)| run_command(step).is_some_and(|run| run.trim() == command))
        .unwrap_or_else(|| panic!("workflow does not run `{command}`"))
}

fn named_step<'a>(steps: &'a [Yaml], name: &str) -> &'a Yaml {
    steps
        .iter()
        .find(|step| step["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("workflow does not contain step `{name}`"))
}

fn shell_block_containing(markdown: &str, required: &[&str]) -> String {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    let mut in_shell = false;

    for line in markdown.lines() {
        if !in_shell && matches!(line, "```sh" | "```bash") {
            in_shell = true;
            current.clear();
        } else if in_shell && line == "```" {
            blocks.push(current.join("\n"));
            in_shell = false;
        } else if in_shell {
            current.push(line);
        }
    }

    blocks
        .into_iter()
        .find(|block| required.iter().all(|needle| block.contains(needle)))
        .unwrap_or_else(|| panic!("no shell block contains {required:?}"))
}

#[test]
fn systemd_service_keeps_the_embedded_binary_placeholder() {
    let service = repo_file("packaging/systemd/car-go-clean.service");

    assert!(service.contains("ExecStart=__CAR_GO_CLEAN_BIN__ daemon"));
}

#[test]
fn launchd_plist_runs_daemon_with_configurable_paths() {
    let plist = repo_file("packaging/launchd/com.dcchuck.car-go-clean.plist");

    assert!(plist.contains("<key>ProgramArguments</key>"));
    assert!(plist.contains("__CAR_GO_CLEAN_BIN__"));
    assert!(plist.contains("__CAR_GO_CLEAN_LOG_DIR__"));
    assert!(plist.contains("daemon"));
    assert!(!plist.contains("/Users/charlesdanielsson"));
    assert!(!plist.contains("/usr/local/bin/car-go-clean"));
    assert!(!plist.contains("/tmp/car-go-clean.launchd"));
}

#[test]
fn source_checkout_launchd_installer_is_absent() {
    assert!(!Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("packaging/launchd/install.sh")
        .exists());
}

#[test]
fn readme_documents_binary_installs_and_explicit_service_activation() {
    let readme = repo_file("README.md");

    assert!(readme.contains("brew install dcchuck/tap/car-go-clean"));
    assert!(readme.contains("car-go-clean-installer.sh"));
    assert!(readme.contains("car-go-clean service install"));
    assert!(readme.contains("car-go-clean service restart"));
    assert!(readme.contains("does not start the daemon"));
}

#[test]
fn readme_uses_compact_logo_asset() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = repo_file("README.md");

    assert!(root.join("assets/car-go-clean-logo.png").is_file());
    assert!(root.join("assets/car-go-clean-logo-readme.png").is_file());
    assert!(readme.contains("assets/car-go-clean-logo-readme.png"));
    assert!(readme.contains("width=\"440\""));
    assert!(!readme.contains("width=\"640\""));
    assert!(readme.contains("</p>\n<h1>car-go-clean</h1>"));
}

#[test]
fn configuration_reference_preserves_operational_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = repo_file("README.md");
    let guide = repo_file("docs/configuration.md");
    let release = repo_file("docs/releases/v0.4.0.md");

    assert!(root.join("docs/configuration.md").is_file());
    assert!(readme.contains("[Configuration reference](docs/configuration.md)"));
    for value in [
        "scan_dirs",
        "project_dirs",
        "extra_excludes",
        "override_excludes",
        "legacy `excludes`",
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
    for value in [
        "removed in v0.5",
        "config migrate",
        "exit `0`",
        "exit `1`",
        "exit `2`",
    ] {
        assert!(release.contains(value), "missing {value}");
    }
}

#[test]
fn readme_diagnostic_json_example_executes_successfully() {
    let readme = repo_file("README.md");
    let example = shell_block_containing(
        &readme,
        &["car-go-clean health --json", "car-go-clean status --json"],
    );
    let work = tempdir().unwrap();
    let home = work.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let binary = Path::new(env!("CARGO_BIN_EXE_car-go-clean"));
    let mut path_entries = vec![binary.parent().unwrap().to_path_buf()];
    path_entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(path_entries).unwrap();

    let output = Command::new("sh")
        .args(["-eu", "-c", &example])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", work.path().join("config"))
        .env("XDG_STATE_HOME", work.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "documented diagnostic example failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn release_packaging_documents_tagged_binary_distribution() {
    let release = repo_file("packaging/release/README.md");

    assert!(release.contains("annotated `vX.Y.Z` Git tags"));
    assert!(release.contains("dcchuck/homebrew-tap"));
    assert!(release.contains("car-go-clean-installer.sh"));
    assert!(release.contains("aarch64-apple-darwin"));
    assert!(release.contains("x86_64-apple-darwin"));
    assert!(release.contains("aarch64-unknown-linux-musl"));
    assert!(release.contains("x86_64-unknown-linux-musl"));
    assert!(release.contains("Neither binary installation path enables or starts the daemon"));
    assert!(release.contains("cargo install --path ."));
}

#[test]
fn cargo_dist_metadata_declares_the_public_release_contract() {
    let manifest = repo_file("Cargo.toml");
    let dist = repo_file("dist-workspace.toml");
    for value in [
        "version = \"0.4.0\"",
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
        "publish-jobs = [\"./publish-shell-installer\", \"./publish-homebrew-formula\"]",
        "\"publish-shell-installer\" = { contents = \"write\", attestations = \"write\", id-token = \"write\" }",
        "\"publish-homebrew-formula\" = { contents = \"read\" }",
        "allow-dirty = [\"ci\"]",
    ] {
        assert!(dist.contains(value), "missing {value}");
    }
    assert!(!dist.contains("post-announce-jobs"));
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
    assert!(workflow.contains("Enforce annotated vX.Y.Z release tag"));
    assert!(workflow.contains("\n  release-preflight:\n"));
    assert!(workflow.contains("HOMEBREW_TAP_TOKEN is required"));
    assert!(workflow.contains("gh release create"));
    assert!(workflow.contains("--draft"));
    assert!(!workflow.contains("\n  publish-homebrew-formula:\n"));
    assert!(workflow.contains("\n  custom-publish-homebrew-formula:\n"));
    assert!(workflow.contains("\n  custom-release-verify:\n"));
    assert!(workflow.contains("needs.custom-release-verify.result == 'success'"));
    assert!(workflow.contains("gh release edit"));
    assert!(workflow.contains("--draft=false"));

    let host = workflow
        .split("\n  host:\n")
        .nth(1)
        .unwrap()
        .split("\n  custom-publish-shell-installer:\n")
        .next()
        .unwrap();
    assert!(host.contains("- release-preflight"));
    assert!(host.contains("needs.release-preflight.result == 'success'"));

    let verification = workflow
        .split("\n  custom-release-verify:\n")
        .nth(1)
        .unwrap()
        .split("\n  announce:\n")
        .next()
        .unwrap();
    assert!(verification.contains("- custom-publish-shell-installer"));
    assert!(verification.contains("- custom-publish-homebrew-formula"));
}

#[test]
fn release_workflow_composes_reviewed_notes_before_creating_the_draft() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(root.join("docs/releases/v0.4.0.md").is_file());
    assert!(root.join("scripts/compose-release-notes.sh").is_file());

    let release = workflow(".github/workflows/release.yml");
    let steps = workflow_steps(&release, "host");
    let compose = steps
        .iter()
        .enumerate()
        .find(|(_, step)| {
            run_command(step).is_some_and(|run| {
                run.lines()
                    .map(str::trim)
                    .any(|line| line.starts_with("scripts/compose-release-notes.sh "))
            })
        })
        .expect("host job does not compose reviewed release notes");
    let create = steps
        .iter()
        .enumerate()
        .find(|(_, step)| {
            run_command(step).is_some_and(|run| {
                run.lines()
                    .map(str::trim)
                    .any(|line| line.starts_with("gh release create "))
            })
        })
        .expect("host job does not create a release");

    assert!(compose.0 < create.0);
    assert!(compose.1["env"]["ANNOUNCEMENT_BODY"].as_str().is_some());
    assert!(create.1["env"]["ANNOUNCEMENT_BODY"].is_badvalue());
    assert!(run_command(create.1)
        .unwrap()
        .split_whitespace()
        .any(|word| word == "\"$RUNNER_TEMP/notes.txt\""));

    let runner_temp = tempdir().unwrap();
    let runnable = run_command(compose.1)
        .unwrap()
        .replace("${{ needs.plan.outputs.tag }}", "v0.4.0");
    let output = Command::new("sh")
        .args(["-eu", "-c", &runnable])
        .current_dir(root)
        .env("ANNOUNCEMENT_BODY", "generated workflow body")
        .env("RUNNER_TEMP", runner_temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "composition step failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let notes = fs::read_to_string(runner_temp.path().join("notes.txt")).unwrap();
    assert_eq!(notes.lines().next(), Some("# car-go-clean v0.4.0"));
    assert!(notes.lines().any(|line| line == "generated workflow body"));
}

#[test]
fn ci_runs_release_note_validation_after_installer_validation() {
    let ci = workflow(".github/workflows/ci.yml");
    let steps = workflow_steps(&ci, "verify");
    let installer = step_running(steps, "make test-installer");
    let release_notes = step_running(steps, "make test-release-notes");

    assert!(installer.0 < release_notes.0);
}

#[test]
fn ci_and_release_verification_cover_installable_artifacts() {
    let ci = repo_file(".github/workflows/ci.yml");
    let release = repo_file(".github/workflows/release.yml");
    let build_setup = repo_file(".github/release-build-setup.yml");
    let verify = repo_file(".github/workflows/release-verify.yml");

    assert!(ci.contains("cargo test --locked"));
    assert!(ci.contains("cargo clippy --all-targets --locked -- -D warnings"));
    assert!(ci.contains("make test-installer"));
    assert!(ci.contains("cargo metadata --no-deps --format-version 1"));
    assert!(ci.contains("dist plan --tag \"v$VERSION\" --output-format=json"));
    assert!(!ci.contains("dist plan --tag v0.3.0"));
    assert!(build_setup.contains("cargo fmt --all -- --check"));
    assert!(release.contains("publish-shell-installer"));
    assert!(release.contains("publish-homebrew-formula"));
    assert!(release.contains("Enforce annotated vX.Y.Z release tag"));
    assert!(verify.contains("health --skip-cargo"));
    assert!(verify.contains("brew tap --custom-remote \"$TAP\""));
    assert!(verify.contains("brew audit --strict \"$TAP/car-go-clean\""));
    assert!(verify.contains("gh release download"));
    assert!(verify.contains("formula/car-go-clean-$TAG"));
    assert!(!verify.contains("git clone https://github.com/dcchuck/homebrew-tap"));

    let formula = repo_file(".github/workflows/publish-homebrew-formula.yml");
    assert!(formula.contains("HOMEBREW_TAP_TOKEN"));
    assert!(formula.contains("formula/car-go-clean-$TAG"));
    assert!(formula.contains("gh pr create"));
    assert!(formula.contains("gh pr edit"));
    assert!(formula.contains("contents: read"));
    assert!(formula.contains("git push --set-upstream origin \"HEAD:refs/heads/$BRANCH\""));
    assert!(formula.contains("packaging/release/homebrew/car-go-clean.rb.in"));

    let formula_template = repo_file("packaging/release/homebrew/car-go-clean.rb.in");
    assert!(formula_template.contains("on_macos do"));
    assert!(formula_template.contains("on_linux do"));
    assert!(formula_template.contains("test do"));
}

#[test]
fn homebrew_formula_render_fails_before_output_when_checksums_are_missing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let publish = workflow(".github/workflows/publish-homebrew-formula.yml");
    let steps = workflow_steps(&publish, "publish-homebrew-formula");
    let render = named_step(steps, "Render standards-compliant formula");
    let run = run_command(render).unwrap();
    let work = tempdir().unwrap();

    fs::create_dir_all(work.path().join("dist-artifacts")).unwrap();
    fs::create_dir_all(work.path().join("packaging/release/homebrew")).unwrap();
    fs::copy(
        root.join("packaging/release/homebrew/car-go-clean.rb.in"),
        work.path()
            .join("packaging/release/homebrew/car-go-clean.rb.in"),
    )
    .unwrap();

    let output = Command::new("bash")
        .args(["--noprofile", "--norc", "-e", "-o", "pipefail", "-c", run])
        .current_dir(work.path())
        .env("TAG", "v0.4.0")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "formula rendering accepted missing checksums"
    );
    assert!(
        !work
            .path()
            .join("generated-formula/car-go-clean.rb")
            .exists(),
        "formula output was created after checksum validation failed"
    );
}

#[cfg(unix)]
#[test]
fn homebrew_upgrade_docs_preserve_service_state() {
    use std::os::unix::fs::PermissionsExt;

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn verify(markdown_path: &str, initial_state: Option<&str>) {
        let markdown = repo_file(markdown_path);
        let upgrade = shell_block_containing(
            &markdown,
            &[
                "service_was_active=",
                "brew upgrade dcchuck/tap/car-go-clean",
                "car-go-clean run --dry-run --all",
            ],
        );
        let work = tempdir().unwrap();
        let bin = work.path().join("bin");
        let calls = work.path().join("calls");
        let installed = work.path().join("installed");
        let state = work.path().join("service-state");
        let template = work.path().join("car-go-clean.template");
        fs::create_dir(&bin).unwrap();

        write_executable(
            &template,
            r#"#!/bin/sh
set -eu
printf 'car-go-clean %s\n' "$*" >> "$CALL_LOG"
case "$*" in
  "service status")
    if test -f "$SERVICE_STATE"; then
      service_state=$(cat "$SERVICE_STATE")
    else
      service_state="not installed"
    fi
    printf 'Service\n  State: %s\n' "$service_state"
    ;;
  "service stop") printf 'stopped\n' > "$SERVICE_STATE" ;;
  "service start") printf 'running\n' > "$SERVICE_STATE" ;;
  "run --dry-run --all") ;;
  "version") printf '0.4.0\n' ;;
  *) exit 64 ;;
esac
"#,
        );
        write_executable(
            &bin.join("brew"),
            r#"#!/bin/sh
set -eu
printf 'brew %s\n' "$*" >> "$CALL_LOG"
case "$1" in
  update) ;;
  list) test -f "$INSTALLED_MARKER" ;;
  install|upgrade)
    cp "$CAR_GO_CLEAN_TEMPLATE" "$FAKE_BIN/car-go-clean"
    chmod +x "$FAKE_BIN/car-go-clean"
    : > "$INSTALLED_MARKER"
    ;;
  *) exit 64 ;;
esac
"#,
        );

        if let Some(initial_state) = initial_state {
            fs::copy(&template, bin.join("car-go-clean")).unwrap();
            fs::set_permissions(bin.join("car-go-clean"), fs::Permissions::from_mode(0o755))
                .unwrap();
            fs::write(&state, format!("{initial_state}\n")).unwrap();
            fs::write(&installed, "").unwrap();
        }

        let output = Command::new("sh")
            .args(["-eu", "-c", &upgrade])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("CALL_LOG", &calls)
            .env("INSTALLED_MARKER", &installed)
            .env("SERVICE_STATE", &state)
            .env("CAR_GO_CLEAN_TEMPLATE", &template)
            .env("FAKE_BIN", &bin)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} upgrade block failed: {}",
            markdown_path,
            String::from_utf8_lossy(&output.stderr)
        );

        let calls = fs::read_to_string(&calls).unwrap();
        match initial_state {
            Some("running") => {
                assert_eq!(
                    calls.lines().collect::<Vec<_>>(),
                    vec![
                        "car-go-clean service status",
                        "car-go-clean service stop",
                        "brew update",
                        "brew list --versions car-go-clean",
                        "brew upgrade dcchuck/tap/car-go-clean",
                        "car-go-clean version",
                        "car-go-clean run --dry-run --all",
                        "car-go-clean service start",
                        "car-go-clean service status",
                    ]
                );
                assert_eq!(fs::read_to_string(&state).unwrap().trim(), "running");
            }
            Some("stopped") => {
                assert_eq!(
                    calls.lines().collect::<Vec<_>>(),
                    vec![
                        "car-go-clean service status",
                        "brew update",
                        "brew list --versions car-go-clean",
                        "brew upgrade dcchuck/tap/car-go-clean",
                        "car-go-clean version",
                    ]
                );
                assert_eq!(fs::read_to_string(&state).unwrap().trim(), "stopped");
            }
            Some("not installed") => {
                assert_eq!(
                    calls.lines().collect::<Vec<_>>(),
                    vec![
                        "car-go-clean service status",
                        "brew update",
                        "brew list --versions car-go-clean",
                        "brew upgrade dcchuck/tap/car-go-clean",
                        "car-go-clean version",
                    ]
                );
                assert_eq!(fs::read_to_string(&state).unwrap().trim(), "not installed");
            }
            None => {
                assert_eq!(
                    calls.lines().collect::<Vec<_>>(),
                    vec![
                        "brew update",
                        "brew list --versions car-go-clean",
                        "brew install dcchuck/tap/car-go-clean",
                        "car-go-clean version",
                    ]
                );
                assert!(!state.exists());
            }
            Some(other) => panic!("unsupported fixture state {other}"),
        }
    }

    for markdown_path in ["docs/releasing.md", "docs/releases/v0.4.0.md"] {
        verify(markdown_path, Some("running"));
        verify(markdown_path, Some("stopped"));
        verify(markdown_path, Some("not installed"));
        verify(markdown_path, None);
    }
}

#[test]
fn release_runbook_documents_the_guarded_draft_publication_flow() {
    let runbook = repo_file("docs/releasing.md");

    assert!(runbook.contains("HOMEBREW_TAP_TOKEN"));
    assert!(runbook.contains("gh secret set HOMEBREW_TAP_TOKEN"));
    assert!(runbook.contains("draft"));
    assert!(runbook.contains("formula-bump pull request"));
    assert!(runbook.contains("publishes the draft only after"));
    assert!(!runbook.contains("After GitHub has published the release"));
}
