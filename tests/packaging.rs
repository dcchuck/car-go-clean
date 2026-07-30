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

fn uses_action<'a>(steps: &'a [Yaml], action: &str) -> Vec<&'a Yaml> {
    steps
        .iter()
        .filter(|step| step["uses"].as_str() == Some(action))
        .collect()
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
fn rehearse_release_dispatch_is_exact_sha_bound_and_uses_only_pinned_actions() {
    let rehearsal = workflow(".github/workflows/rehearse-release.yml");
    let dispatch = &rehearsal["on"]["workflow_dispatch"];
    for input in ["commit_sha", "version"] {
        assert_eq!(dispatch["inputs"][input]["required"].as_bool(), Some(true));
        assert_eq!(dispatch["inputs"][input]["type"].as_str(), Some("string"));
    }
    assert!(
        rehearsal["permissions"]
            .as_hash()
            .is_some_and(|permissions| permissions.is_empty()),
        "workflow-level permissions must be empty"
    );

    let expected_permissions = [
        ("validate", [("contents", "read")].as_slice()),
        (
            "build",
            [
                ("attestations", "write"),
                ("contents", "read"),
                ("id-token", "write"),
            ]
            .as_slice(),
        ),
        ("smoke", [("contents", "read")].as_slice()),
        ("runner-resolution", [("actions", "read")].as_slice()),
        ("tap-capability", [("contents", "read")].as_slice()),
        ("aggregate-evidence", [("actions", "read")].as_slice()),
    ];
    for (job, expected) in expected_permissions {
        let permissions = rehearsal["jobs"][job]["permissions"]
            .as_hash()
            .unwrap_or_else(|| panic!("{job} must declare job-scoped permissions"));
        let actual = permissions
            .iter()
            .map(|(key, value)| (key.as_str().unwrap(), value.as_str().unwrap()))
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected.iter().copied().collect());
    }

    let exact_actions = [
        (
            "actions/checkout",
            "actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803",
        ),
        (
            "actions/upload-artifact",
            "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
        ),
        (
            "actions/download-artifact",
            "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
        ),
        (
            "actions/attest",
            "actions/attest@508db95dd578ae2727ebd6217d5ba78e4fbda05d",
        ),
        (
            "dtolnay/rust-toolchain",
            "dtolnay/rust-toolchain@2c7215f132e9ebf062739d9130488b56d53c060c",
        ),
    ];
    let mut seen = BTreeSet::new();
    for (_, job) in rehearsal["jobs"].as_hash().unwrap() {
        for step in job["steps"].as_vec().unwrap() {
            let Some(action) = step["uses"].as_str() else {
                continue;
            };
            let (owner, expected) = exact_actions
                .iter()
                .find(|(owner, _)| action.starts_with(&format!("{owner}@")))
                .unwrap_or_else(|| panic!("unexpected unpinned action `{action}`"));
            assert_eq!(action, *expected, "{owner} is not immutably pinned");
            let revision = action.rsplit_once('@').unwrap().1;
            assert_eq!(revision.len(), 40, "{owner} is not pinned to a commit");
            assert!(
                revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{owner} pin is not a hexadecimal commit"
            );
            seen.insert(*owner);
        }
    }
    assert_eq!(
        seen,
        exact_actions
            .iter()
            .map(|(owner, _)| *owner)
            .collect::<BTreeSet<_>>()
    );

    for job in ["validate", "build", "smoke", "tap-capability"] {
        let checkout = uses_action(
            workflow_steps(&rehearsal, job),
            "actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803",
        );
        assert_eq!(checkout.len(), 1, "{job} must check out exactly once");
        assert_eq!(
            checkout[0]["with"]["ref"].as_str(),
            Some("${{ inputs.commit_sha }}")
        );
        assert_eq!(
            checkout[0]["with"]["persist-credentials"].as_bool(),
            Some(false)
        );
    }
}

#[test]
fn rehearse_release_builds_and_smokes_the_four_native_install_paths() {
    let rehearsal = workflow(".github/workflows/rehearse-release.yml");
    let expected = BTreeSet::from([
        ("aarch64-apple-darwin", "macos-14", "arm64"),
        ("x86_64-apple-darwin", "macos-15-intel", "x86_64"),
        ("aarch64-unknown-linux-musl", "ubuntu-24.04-arm", "aarch64"),
        ("x86_64-unknown-linux-musl", "ubuntu-24.04", "x86_64"),
    ]);

    for job in ["build", "smoke"] {
        assert_eq!(
            rehearsal["jobs"][job]["runs-on"].as_str(),
            Some("${{ matrix.runner }}")
        );
        let include = rehearsal["jobs"][job]["strategy"]["matrix"]["include"]
            .as_vec()
            .unwrap();
        let actual = include
            .iter()
            .map(|entry| {
                (
                    entry["target"].as_str().unwrap(),
                    entry["runner"].as_str().unwrap(),
                    entry["expected_uname"].as_str().unwrap(),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected, "{job} matrix drifted");
    }

    let build_steps = workflow_steps(&rehearsal, "build");
    let build = named_step(build_steps, "Build and verify target archive");
    let build_run = run_command(build).unwrap();
    for fragment in [
        "uname -m",
        "matrix.expected_uname",
        "dist build",
        "--artifacts=local",
        "--target=${{ matrix.target }}",
        "archive=\"car-go-clean-${{ matrix.target }}.tar.xz\"",
        "checksum=\"$archive.sha256\"",
    ] {
        assert!(
            build_run.contains(fragment),
            "build step is missing `{fragment}`"
        );
    }
    assert_eq!(
        uses_action(
            build_steps,
            "actions/attest@508db95dd578ae2727ebd6217d5ba78e4fbda05d",
        )
        .len(),
        1
    );

    let smoke_steps = workflow_steps(&rehearsal, "smoke");
    let smoke = named_step(smoke_steps, "Smoke actual installer and formula");
    let smoke_run = run_command(smoke).unwrap();
    for fragment in [
        "scripts/verify-release-assets.sh",
        "version_output=$(\"$binary\" version)",
        "health --skip-cargo",
        "python3 -m http.server",
        "packaging/release/car-go-clean-installer.sh",
        "--download-base-url",
        "Library/LaunchAgents/com.dcchuck.car-go-clean.plist",
        ".config/systemd/user/car-go-clean.service",
        "test ! -e \"$HOME/Library/LaunchAgents/com.dcchuck.car-go-clean.plist\"",
        "test ! -e \"$HOME/.config/systemd/user/car-go-clean.service\"",
        "scripts/render-homebrew-formula.sh",
        "brew install --formula",
        "brew test car-go-clean",
        "brew_binary=\"$(brew --prefix car-go-clean)/bin/car-go-clean\"",
        "\"$brew_binary\" version",
    ] {
        assert!(
            smoke_run.contains(fragment),
            "smoke step is missing `{fragment}`"
        );
    }

    for job in ["build", "smoke"] {
        let steps = workflow_steps(&rehearsal, job);
        let upload = named_step(steps, "Upload target evidence");
        assert_eq!(upload["if"].as_str(), Some("${{ always() }}"));
        assert_eq!(
            upload["uses"].as_str(),
            Some("actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a")
        );
        assert!(upload["with"]["name"]
            .as_str()
            .unwrap()
            .contains("${{ needs.validate.outputs.evidence_key }}-${{ matrix.target }}"));
    }
}

#[test]
fn rehearse_release_fails_closed_on_intel_or_tap_gaps_and_aggregates_evidence() {
    let rehearsal = workflow(".github/workflows/rehearse-release.yml");

    let resolution = workflow_steps(&rehearsal, "runner-resolution");
    let verify = named_step(resolution, "Verify resolved runner labels");
    let verify_run = run_command(verify).unwrap();
    for fragment in [
        "/actions/runs/$GITHUB_RUN_ID/jobs",
        "macos-15-intel",
        "runner_name",
        "x86_64 macOS",
        "archive/checksum coverage only",
    ] {
        assert!(
            verify_run.contains(fragment),
            "runner resolution is missing `{fragment}`"
        );
    }

    let tap_job = &rehearsal["jobs"]["tap-capability"];
    let tap_needs = tap_job["needs"]
        .as_vec()
        .expect("tap capability must list every trusted prerequisite")
        .iter()
        .map(|need| need.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        tap_needs,
        BTreeSet::from(["validate", "build", "smoke", "runner-resolution"])
    );
    let tap_gate = tap_job["if"]
        .as_str()
        .expect("tap capability must have an explicit result gate");
    assert_eq!(
        tap_gate,
        "${{ needs.validate.result == 'success' && needs.build.result == 'success' && needs.smoke.result == 'success' && needs.runner-resolution.result == 'success' }}",
        "the secret-bearing job must run only after every direct prerequisite succeeds"
    );
    let tap_steps = workflow_steps(&rehearsal, "tap-capability");
    let checkout = uses_action(
        tap_steps,
        "actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803",
    )[0];
    assert!(
        checkout["if"].is_badvalue(),
        "trusted checkout must use the successful job gate"
    );
    let capability = named_step(tap_steps, "Rehearse tap capability");
    assert!(
        capability["if"].is_badvalue(),
        "secret-bearing execution must use the successful job gate"
    );
    assert_eq!(
        capability["env"]["GH_TOKEN"].as_str(),
        Some("${{ secrets.HOMEBREW_TAP_TOKEN }}")
    );
    assert!(
        run_command(capability)
            .unwrap()
            .contains("scripts/rehearse-tap-capability.sh"),
        "Task 3 tap script hook is absent"
    );
    let cleanup = named_step(tap_steps, "Cleanup tap rehearsal");
    assert_eq!(cleanup["if"].as_str(), Some("${{ always() }}"));
    assert_eq!(
        cleanup["env"]["GH_TOKEN"].as_str(),
        Some("${{ secrets.HOMEBREW_TAP_TOKEN }}")
    );
    let tap_evidence = named_step(tap_steps, "Write tap-capability evidence");
    assert_eq!(tap_evidence["if"].as_str(), Some("${{ always() }}"));
    let tap_upload = named_step(tap_steps, "Upload tap-capability evidence");
    assert_eq!(tap_upload["if"].as_str(), Some("${{ always() }}"));

    let aggregate_job = &rehearsal["jobs"]["aggregate-evidence"];
    assert!(aggregate_job["if"].as_str().unwrap().contains("always()"));
    let aggregate_steps = workflow_steps(&rehearsal, "aggregate-evidence");
    for name in ["Download per-job evidence", "Download exact release plan"] {
        let download = named_step(aggregate_steps, name);
        assert_eq!(download["if"].as_str(), Some("${{ always() }}"));
        assert_eq!(download["continue-on-error"].as_bool(), Some(true));
    }
    let inventory = named_step(aggregate_steps, "Index partial evidence");
    assert_eq!(inventory["if"].as_str(), Some("${{ always() }}"));
    let upload = named_step(aggregate_steps, "Upload release rehearsal evidence");
    assert_eq!(upload["if"].as_str(), Some("${{ always() }}"));
    assert_eq!(
        upload["with"]["name"].as_str(),
        Some("release-rehearsal-${{ needs.validate.outputs.evidence_key }}")
    );
    let enforce = named_step(aggregate_steps, "Enforce complete sanitized evidence");
    assert_eq!(enforce["if"].as_str(), Some("${{ always() }}"));
    let upload_index = aggregate_steps
        .iter()
        .position(|step| std::ptr::eq(step, upload))
        .unwrap();
    let enforce_index = aggregate_steps
        .iter()
        .position(|step| std::ptr::eq(step, enforce))
        .unwrap();
    assert!(
        upload_index < enforce_index,
        "aggregate evidence must be uploaded before incompleteness fails the job"
    );
}

#[test]
fn rehearse_release_evidence_records_gate_outcomes_and_rejects_unsafe_artifact_names() {
    let rehearsal = workflow(".github/workflows/rehearse-release.yml");

    let validate_steps = workflow_steps(&rehearsal, "validate");
    let normalize = named_step(validate_steps, "Normalize the evidence key");
    for (candidate, expected) in [
        (
            "0123456789abcdef0123456789abcdef01234567",
            "value=0123456789abcdef0123456789abcdef01234567\nsafe_exact_sha=0123456789abcdef0123456789abcdef01234567\n",
        ),
        (
            "../../bad candidate HOMEBREW_TAP_TOKEN",
            "value=run-123-4\nsafe_exact_sha=invalid\n",
        ),
    ] {
        let output_file = tempdir().unwrap();
        let github_output = output_file.path().join("github-output");
        let output = Command::new("sh")
            .args(["-eu", "-c", run_command(normalize).unwrap()])
            .env("CANDIDATE_SHA", candidate)
            .env("GITHUB_RUN_ID", "123")
            .env("GITHUB_RUN_ATTEMPT", "4")
            .env("GITHUB_OUTPUT", &github_output)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "evidence-key normalization failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(github_output).unwrap(), expected);
    }

    for (name, id) in [
        ("Validate exact SHA and version", "validate_inputs"),
        ("Install verified cargo-dist", "install_dist"),
        ("Plan exact release", "dist_plan"),
    ] {
        assert_eq!(
            named_step(validate_steps, name)["id"].as_str(),
            Some(id),
            "{name} needs a stable evidence ID"
        );
    }
    let validation_evidence = named_step(validate_steps, "Write validation evidence");
    for (name, value) in [
        ("VALIDATE_OUTCOME", "${{ steps.validate_inputs.outcome }}"),
        ("INSTALL_DIST_OUTCOME", "${{ steps.install_dist.outcome }}"),
        ("DIST_PLAN_OUTCOME", "${{ steps.dist_plan.outcome }}"),
    ] {
        assert_eq!(validation_evidence["env"][name].as_str(), Some(value));
    }

    let build_steps = workflow_steps(&rehearsal, "build");
    assert_eq!(
        named_step(build_steps, "Attest target archive")["id"].as_str(),
        Some("attest_archive")
    );
    assert_eq!(
        named_step(build_steps, "Upload target archive and manifest")["id"].as_str(),
        Some("upload_archive")
    );
    let build_evidence = named_step(build_steps, "Write target build evidence");
    assert_eq!(
        build_evidence["env"]["ATTEST_OUTCOME"].as_str(),
        Some("${{ steps.attest_archive.outcome }}")
    );
    assert_eq!(
        build_evidence["env"]["ARCHIVE_UPLOAD_OUTCOME"].as_str(),
        Some("${{ steps.upload_archive.outcome }}")
    );

    for (_, job) in rehearsal["jobs"].as_hash().unwrap() {
        for step in job["steps"].as_vec().unwrap() {
            for field in ["name", "pattern"] {
                if let Some(value) = step["with"][field].as_str() {
                    assert!(
                        !value.contains("${{ inputs.commit_sha }}"),
                        "untrusted raw dispatch input is used in artifact {field} `{value}`"
                    );
                }
            }
        }
    }
}

#[test]
fn rehearse_release_aggregate_preserves_a_sanitized_partial_inventory() {
    let rehearsal = workflow(".github/workflows/rehearse-release.yml");
    let aggregate_steps = workflow_steps(&rehearsal, "aggregate-evidence");
    let inventory = named_step(aggregate_steps, "Index partial evidence");
    let work = tempdir().unwrap();
    let raw_evidence = work.path().join("raw-evidence");
    let evidence = work.path().join("evidence");
    fs::create_dir(&raw_evidence).unwrap();
    fs::write(
        raw_evidence.join("validate.json"),
        r#"{"format_version":1,"phase":"validate","exact_sha":"0123456789abcdef0123456789abcdef01234567","outcomes":{"validation":"failure"}}"#,
    )
    .unwrap();

    let output = Command::new("sh")
        .args(["-eu", "-c", run_command(inventory).unwrap()])
        .current_dir(work.path())
        .env("EXACT_SHA", "0123456789abcdef0123456789abcdef01234567")
        .env("VERSION", "0.4.0")
        .env("EVIDENCE_DOWNLOAD_OUTCOME", "failure")
        .env("PLAN_DOWNLOAD_OUTCOME", "failure")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "partial inventory generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let missing = fs::read_to_string(evidence.join("missing-evidence-files.txt")).unwrap();
    let actual_missing = missing.lines().collect::<BTreeSet<_>>();
    let expected_missing = BTreeSet::from([
        "build-aarch64-apple-darwin.json",
        "build-aarch64-unknown-linux-musl.json",
        "build-x86_64-apple-darwin.json",
        "build-x86_64-unknown-linux-musl.json",
        "runner-resolution.json",
        "smoke-aarch64-apple-darwin.json",
        "smoke-aarch64-unknown-linux-musl.json",
        "smoke-x86_64-apple-darwin.json",
        "smoke-x86_64-unknown-linux-musl.json",
        "tap-capability.json",
    ]);
    assert_eq!(actual_missing, expected_missing);
    assert_eq!(
        fs::read_to_string(evidence.join("missing-supporting-files.txt")).unwrap(),
        "plan/dist-plan.json\n"
    );

    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(evidence.join("aggregate-inventory.json")).unwrap())
            .unwrap();
    assert_eq!(report["format_version"], 1);
    assert_eq!(
        report["exact_sha"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(report["version"], "0.4.0");
    assert_eq!(report["complete"], false);
    assert_eq!(report["sanitized"], true);
    assert_eq!(
        report["available_evidence_files"],
        serde_json::json!(["validate.json"])
    );
    assert_eq!(
        fs::read_to_string(evidence.join("aggregate-status.txt")).unwrap(),
        "incomplete\n"
    );
}

#[test]
fn rehearse_release_aggregate_omits_unsanitized_fragments_without_echoing_secrets() {
    let rehearsal = workflow(".github/workflows/rehearse-release.yml");
    let aggregate_steps = workflow_steps(&rehearsal, "aggregate-evidence");
    let inventory = named_step(aggregate_steps, "Index partial evidence");
    let work = tempdir().unwrap();
    let raw_evidence = work.path().join("raw-evidence");
    let evidence = work.path().join("evidence");
    fs::create_dir(&raw_evidence).unwrap();
    fs::write(
        raw_evidence.join("validate.json"),
        r#"{"format_version":1,"phase":"validate","diagnostic":"HOMEBREW_TAP_TOKEN"}"#,
    )
    .unwrap();

    let output = Command::new("sh")
        .args(["-eu", "-c", run_command(inventory).unwrap()])
        .current_dir(work.path())
        .env("EXACT_SHA", "0123456789abcdef0123456789abcdef01234567")
        .env("VERSION", "0.4.0")
        .env("EVIDENCE_DOWNLOAD_OUTCOME", "success")
        .env("PLAN_DOWNLOAD_OUTCOME", "failure")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "unsanitized partial inventory generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!evidence.join("jobs/validate.json").exists());
    assert_eq!(
        fs::read_to_string(evidence.join("sanitization-findings.txt")).unwrap(),
        "validate.json\n"
    );
    for entry in fs::read_dir(&evidence).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            assert!(
                !fs::read_to_string(entry.path())
                    .unwrap()
                    .contains("HOMEBREW_TAP_TOKEN"),
                "sanitized aggregate echoed the forbidden marker in {}",
                entry.path().display()
            );
        }
    }

    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(evidence.join("aggregate-inventory.json")).unwrap())
            .unwrap();
    assert_eq!(report["sanitized"], false);
    assert_eq!(
        report["sanitization_findings"],
        serde_json::json!(["validate.json"])
    );
    assert_eq!(
        fs::read_to_string(evidence.join("aggregate-status.txt")).unwrap(),
        "incomplete-unsanitized\n"
    );
}

#[test]
fn rehearse_release_ci_uses_the_verified_release_toolchain() {
    let ci = workflow(".github/workflows/ci.yml");
    let steps = workflow_steps(&ci, "verify");
    assert_eq!(
        uses_action(
            steps,
            "actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803"
        )
        .len(),
        1
    );
    assert_eq!(
        uses_action(
            steps,
            "dtolnay/rust-toolchain@2c7215f132e9ebf062739d9130488b56d53c060c"
        )
        .len(),
        1
    );
    assert_eq!(
        run_command(named_step(steps, "Install verified dist")),
        Some("scripts/install-cargo-dist.sh")
    );
    step_running(steps, "make test-release-scripts");
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
