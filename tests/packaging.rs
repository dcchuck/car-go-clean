use std::collections::BTreeSet;
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

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn terminal_json(stdout: &[u8], command: &str) -> serde_json::Value {
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(stdout).lines().last().unwrap()).unwrap();
    assert_eq!(report["format_version"], 1);
    assert_eq!(report["command"], command);
    assert!(matches!(report["outcome"]["code"].as_u64(), Some(0..=2)));
    assert!(report["outcome"]["reasons"].is_array());
    report
}

#[test]
fn documented_subcommands_are_real_cli_entry_points() {
    let binary = Path::new(env!("CARGO_BIN_EXE_car-go-clean"));
    let fixtures: &[&[&str]] = &[
        &["health", "--help"],
        &["config", "--help"],
        &["config", "migrate", "--help"],
        &["status", "--help"],
        &["projects", "--help"],
        &["scan", "--help"],
        &["run", "--help"],
        &["daemon", "--help"],
        &["stats", "--help"],
        &["logs", "--help"],
        &["service", "install", "--help"],
        &["service", "status", "--help"],
        &["service", "start", "--help"],
        &["service", "stop", "--help"],
        &["service", "restart", "--help"],
        &["service", "uninstall", "--help"],
    ];

    for fixture in fixtures {
        let output = Command::new(binary).args(*fixture).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "fixture {fixture:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Usage:"),
            "fixture {fixture:?} did not print command help"
        );
    }
}

#[cfg(unix)]
#[test]
fn documented_operator_flow_preserves_then_cleans_exact_review() {
    let work = tempdir().unwrap();
    let home = work.path().join("home");
    let root = work.path().join("projects");
    let project = root.join("sample");
    let target = project.join("target");
    let config = work.path().join("config.toml");
    let state = work.path().join("state");
    let bin = work.path().join("bin");
    let cargo_calls = work.path().join("cargo-calls");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(target.join("artifact"), vec![0; 4_096]).unwrap();
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ns\"\n",
            root.display()
        ),
    )
    .unwrap();
    write_executable(
        &bin.join("cargo"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) printf 'cargo 1.95.0\\n' ;;\n  clean) printf '%s\\n' \"$*\" >> '{}'; rm -rf \"$3\" ;;\n  *) exit 64 ;;\nesac\n",
            cargo_calls.display()
        ),
    );
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let binary = Path::new(env!("CARGO_BIN_EXE_car-go-clean"));

    let version = Command::new(binary).arg("version").output().unwrap();
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        env!("CARGO_PKG_VERSION")
    );

    let service = Command::new(binary)
        .args(["service", "status"])
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(service.status.code(), Some(0));
    let service_stdout = String::from_utf8_lossy(&service.stdout);
    assert!(service_stdout.contains("Installed: no"));
    assert!(service_stdout.contains("Enabled: no"));
    assert!(service_stdout.contains("Running: no"));

    let preview = Command::new(binary)
        .args(["run", "--dry-run", "--all", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(
        preview.status.code(),
        Some(0),
        "preview failed: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(target.is_dir(), "dry run removed the target");
    assert!(!cargo_calls.exists(), "dry run invoked Cargo");
    let preview_stdout = String::from_utf8(preview.stdout).unwrap();
    let review_id = preview_stdout
        .lines()
        .find_map(|line| line.strip_prefix("Review ID: "))
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| panic!("preview did not print a usable review ID: {preview_stdout}"));

    let execution = Command::new(binary)
        .args([
            "run",
            "--review",
            &review_id.to_string(),
            "--json",
            "--config",
        ])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(
        execution.status.code(),
        Some(0),
        "review execution failed: {}",
        String::from_utf8_lossy(&execution.stderr)
    );
    assert!(!target.exists(), "reviewed execution did not clean target");
    assert_eq!(
        fs::read_to_string(&cargo_calls).unwrap().lines().count(),
        1,
        "reviewed execution did not invoke Cargo exactly once"
    );
    let lines = String::from_utf8_lossy(&execution.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines[0]["event"], "target");
    let report = lines.last().unwrap();
    assert_eq!(report["format_version"], 1);
    assert_eq!(report["command"], "run");
    assert_eq!(report["outcome"]["code"], 0);
    assert_eq!(report["review_id"], review_id);

    for (command, args) in [
        (
            "health",
            vec![
                "health",
                "--json",
                "--config",
                config.to_str().unwrap(),
                "--state-dir",
                state.to_str().unwrap(),
            ],
        ),
        (
            "status",
            vec![
                "status",
                "--json",
                "--config",
                config.to_str().unwrap(),
                "--state-dir",
                state.to_str().unwrap(),
            ],
        ),
        (
            "stats",
            vec!["stats", "--json", "--state-dir", state.to_str().unwrap()],
        ),
        (
            "logs",
            vec![
                "logs",
                "--errors-only",
                "--tail",
                "5",
                "--json",
                "--state-dir",
                state.to_str().unwrap(),
            ],
        ),
    ] {
        let output = Command::new(binary)
            .args(args)
            .env("HOME", &home)
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(
            matches!(output.status.code(), Some(0 | 2)),
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = terminal_json(&output.stdout, command);
        assert_eq!(
            report["outcome"]["code"].as_i64(),
            output.status.code().map(i64::from)
        );
    }

    let config_output = Command::new(binary)
        .args(["config", "--config"])
        .arg(&config)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert_eq!(config_output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&config_output.stdout).contains("scan_dirs"));

    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("dynamic-artifact"), vec![0; 4_096]).unwrap();
    let dynamic = Command::new(binary)
        .args(["run", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(
        dynamic.status.code(),
        Some(0),
        "dynamic run failed: {}",
        String::from_utf8_lossy(&dynamic.stderr)
    );
    assert!(!target.exists(), "dynamic run did not clean fresh target");
    assert_eq!(
        fs::read_to_string(&cargo_calls).unwrap().lines().count(),
        2,
        "dynamic run did not invoke Cargo exactly once"
    );

    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("inspection-artifact"), vec![0; 4_096]).unwrap();
    for (command, args) in [
        (
            "scan",
            vec![
                "scan",
                "--json",
                "--config",
                config.to_str().unwrap(),
                "--state-dir",
                state.to_str().unwrap(),
            ],
        ),
        (
            "projects",
            vec![
                "projects",
                "--all",
                "--json",
                "--config",
                config.to_str().unwrap(),
                "--state-dir",
                state.to_str().unwrap(),
            ],
        ),
        (
            "projects",
            vec![
                "projects",
                "--risky",
                "--active",
                "--json",
                "--config",
                config.to_str().unwrap(),
                "--state-dir",
                state.to_str().unwrap(),
            ],
        ),
        (
            "status",
            vec![
                "status",
                "--refresh",
                "--json",
                "--config",
                config.to_str().unwrap(),
                "--state-dir",
                state.to_str().unwrap(),
            ],
        ),
        (
            "stats",
            vec![
                "stats",
                "--since",
                "1d",
                "--top",
                "5",
                "--json",
                "--state-dir",
                state.to_str().unwrap(),
            ],
        ),
    ] {
        let output = Command::new(binary)
            .args(args)
            .env("HOME", &home)
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(
            matches!(output.status.code(), Some(0 | 2)),
            "{command} fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        terminal_json(&output.stdout, command);
    }

    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("new-artifact"), vec![0; 4_096]).unwrap();
    let cached = Command::new(binary)
        .args([
            "run",
            "--dry-run",
            "--no-scan",
            "--all",
            "--include-managed-cache",
            "--include-active",
            "--force",
            "--json",
            "--config",
        ])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(matches!(cached.status.code(), Some(0 | 2)));
    terminal_json(&cached.stdout, "run");
    assert!(target.is_dir(), "cached dry run removed the target");

    let invalid_all = Command::new(binary)
        .args(["run", "--all"])
        .output()
        .unwrap();
    assert_eq!(invalid_all.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&invalid_all.stderr).contains("--dry-run"));
}

#[test]
fn documented_config_migration_changes_only_the_legacy_key() {
    let work = tempdir().unwrap();
    let home = work.path().join("home");
    let config = work.path().join("config.toml");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        &config,
        "scan_dirs = [\"/tmp\"]\nexcludes = [\"legacy\"]\n# keep me\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_car-go-clean"))
        .args(["config", "migrate", "--config"])
        .arg(&config)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "migration failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let migrated = fs::read_to_string(&config).unwrap();
    assert!(migrated.contains("override_excludes = [\"legacy\"]"));
    assert!(!migrated.lines().any(|line| line.starts_with("excludes =")));
    assert!(migrated.contains("# keep me"));
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
    let upgrade = step_running(steps, "make test-upgrade");
    let release_notes = step_running(steps, "make test-release-notes");

    assert!(installer.0 < upgrade.0);
    assert!(upgrade.0 < release_notes.0);

    let release_setup =
        YamlLoader::load_from_str(&repo_file(".github/release-build-setup.yml")).unwrap();
    let release_steps = release_setup[0].as_vec().unwrap();
    step_running(release_steps, "make test-upgrade");
}

#[cfg(unix)]
#[test]
fn shell_release_assets_are_staged_hashed_attested_and_uploaded_as_one_inventory() {
    use std::os::unix::fs::PermissionsExt;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let publish = workflow(".github/workflows/publish-shell-installer.yml");
    let steps = workflow_steps(&publish, "publish-shell-installer");
    let stage = named_step(steps, "Stage shell release assets");
    let attest = named_step(steps, "Attest shell release assets");
    let upload = named_step(steps, "Upload shell release assets");
    let work = tempdir().unwrap();
    let release_dir = work.path().join("packaging/release");
    fs::create_dir_all(&release_dir).unwrap();
    for asset in ["car-go-clean-installer.sh", "car-go-clean-upgrade.sh"] {
        fs::copy(
            root.join("packaging/release").join(asset),
            release_dir.join(asset),
        )
        .unwrap();
    }

    let stage_output = Command::new("sh")
        .args(["-eu", "-c", run_command(stage).unwrap()])
        .current_dir(work.path())
        .output()
        .unwrap();
    assert!(
        stage_output.status.success(),
        "asset staging failed: {}",
        String::from_utf8_lossy(&stage_output.stderr)
    );

    let manifest_path = work.path().join("car-go-clean-shell-assets.sha256");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let entries = manifest
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let digest = fields.next().unwrap();
            let name = fields.next().unwrap();
            assert!(fields.next().is_none(), "unexpected checksum fields");
            assert_eq!(digest.len(), 64);
            assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
            name.to_string()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        entries,
        BTreeSet::from([
            "car-go-clean-installer.sh".to_string(),
            "car-go-clean-upgrade.sh".to_string(),
        ])
    );

    let attested = attest["with"]["subject-path"]
        .as_str()
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        attested,
        BTreeSet::from([
            "car-go-clean-installer.sh".to_string(),
            "car-go-clean-upgrade.sh".to_string(),
            "car-go-clean-shell-assets.sha256".to_string(),
        ])
    );

    let fake_bin = work.path().join("bin");
    let gh_log = work.path().join("gh.log");
    fs::create_dir(&fake_bin).unwrap();
    let gh = fake_bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" > \"$GH_LOG\"\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = vec![fake_bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let upload_output = Command::new("sh")
        .args(["-eu", "-c", run_command(upload).unwrap()])
        .current_dir(work.path())
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("TAG", "v0.4.0")
        .env("GH_LOG", &gh_log)
        .output()
        .unwrap();
    assert!(
        upload_output.status.success(),
        "asset upload failed: {}",
        String::from_utf8_lossy(&upload_output.stderr)
    );
    assert_eq!(
        fs::read_to_string(gh_log).unwrap(),
        "release upload v0.4.0 car-go-clean-installer.sh car-go-clean-upgrade.sh car-go-clean-shell-assets.sha256 --clobber\n"
    );
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
