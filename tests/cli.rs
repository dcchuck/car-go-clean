use assert_cmd::Command;
use car_go_clean::cleaner::CleanAttemptOutcome;
use car_go_clean::config::load;
use car_go_clean::store::Store;
use predicates::prelude::*;
use predicates::str::contains;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

fn json_lines(output: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(output)
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("invalid JSON line {line:?}: {error}"))
        })
        .collect()
}

fn terminal_report(output: &[u8], command: &str) -> serde_json::Value {
    let lines = json_lines(output);
    let report = lines
        .last()
        .unwrap_or_else(|| panic!("missing terminal report for {command}"));
    assert_eq!(report["format_version"], 1);
    assert_eq!(report["command"], command);
    assert!(report["outcome"]["code"].is_u64());
    assert!(report["outcome"]["kind"].is_string());
    assert!(report["outcome"]["reasons"].is_array());
    assert!(report.get("policy_hash").is_some());
    assert!(report.get("generation").is_some());
    assert!(report.get("review_id").is_some());
    assert!(report["scan_errors"].is_array());
    assert!(report.get("data").is_some());
    report.clone()
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn review_fixture(
    work: &tempfile::TempDir,
    project_names: &[&str],
) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = work.path().join("root");
    for name in project_names {
        let project = root.join(name);
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();
    }
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ns\"\n",
            root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let home = work.path().join("missing-home");
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let mut path = bin.clone().into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    (config, state, home, PathBuf::from(path))
}

#[cfg(unix)]
fn create_review_plan(
    config: &std::path::Path,
    state: &std::path::Path,
    home: &std::path::Path,
    path: &std::path::Path,
) -> (i64, String) {
    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--config"])
        .arg(config)
        .args(["--state-dir"])
        .arg(state)
        .env("HOME", home)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "dry run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Review ID: "))
        .unwrap_or_else(|| panic!("missing review ID in {stdout}"))
        .parse()
        .unwrap();
    (id, stdout)
}

fn seed_incomplete_diagnostic_state(work: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let root = work.path().join("root");
    let project = root.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();
    let state = work.path().join("state");
    let home = work.path().join("home");
    fs::create_dir(&home).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .assert()
        .code(2);

    (config, state, home)
}

#[test]
fn unknown_argument_exits_one_instead_of_incomplete() {
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("--definitely-unknown")
        .assert()
        .code(1)
        .stderr(contains("unexpected argument"));
}

#[test]
fn missing_subcommand_exits_one_instead_of_incomplete() {
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .assert()
        .code(1)
        .stderr(contains("Usage:"));
}

#[test]
fn top_level_help_parse_request_exits_zero() {
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("--help")
        .assert()
        .code(0)
        .stdout(contains("Usage:"));
}

#[test]
fn top_level_version_parse_request_exits_zero() {
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("--version")
        .assert()
        .code(0)
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn exit_code_zero_for_complete_scan() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .assert()
        .code(0)
        .stdout(contains("Scan complete: errors=0"));
}

#[test]
fn scan_text_names_generation_and_policy_hash() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .assert()
        .code(0)
        .stdout(contains("generation="))
        .stdout(contains("policy_hash="));
}

#[test]
fn scan_json_reports_generation_policy_origins_and_projects() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .output()
        .unwrap();

    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["generation"].as_i64().unwrap() > 0);
    assert_eq!(value["policy_hash"].as_str().unwrap().len(), 64);
    assert_eq!(
        value["data"]["origins"][0]["path"],
        root.to_string_lossy().as_ref()
    );
    assert_eq!(value["data"]["origins"][0]["completed"], true);
    assert!(value["data"]["origins"][0]["error"].is_null());
    assert_eq!(
        value["data"]["projects"][0],
        project.canonicalize().unwrap().to_string_lossy().as_ref()
    );
}

#[test]
fn complete_scan_json_has_a_versioned_terminal_report() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let report = terminal_report(&output.stdout, "scan");
    assert_eq!(
        report["outcome"],
        serde_json::json!({"code": 0, "kind": "complete", "reasons": []})
    );
    assert_eq!(report["policy_hash"].as_str().unwrap().len(), 64);
    assert!(report["generation"].as_i64().unwrap() > 0);
    assert!(report["review_id"].is_null());
    assert_eq!(report["scan_errors"], serde_json::json!([]));
    assert_eq!(report["data"]["origins"][0]["completed"], true);
}

#[test]
fn incomplete_scan_json_has_stable_reasons_and_details() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(root.join("broken/.git")).unwrap();
    fs::write(root.join("broken/Cargo.toml"), "[workspace]\n").unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let report = terminal_report(&output.stdout, "scan");
    assert_eq!(report["outcome"]["code"], 2);
    assert_eq!(report["outcome"]["kind"], "incomplete");
    assert_eq!(
        report["outcome"]["reasons"],
        serde_json::json!(["origin_incomplete", "scan_incomplete"])
    );
    assert!(!report["scan_errors"].as_array().unwrap().is_empty());
    assert!(report["scan_errors"][0]["message"]
        .as_str()
        .is_some_and(|message| !message.is_empty()));
}

#[test]
fn exit_code_two_for_incomplete_scan() {
    let work = tempfile::tempdir().unwrap();
    let incomplete_root = work.path().join("incomplete-root");
    fs::create_dir_all(incomplete_root.join("project/.git")).unwrap();
    fs::write(incomplete_root.join("project/Cargo.toml"), "[workspace]\n").unwrap();
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", incomplete_root.display()),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(contains("Scan complete: errors=1"));

    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn exit_code_two_after_cleaning_with_incomplete_scan() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let project = root.join("project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(project.join("target/blob"), vec![0; 2048]).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let incomplete_root = work.path().join("incomplete-root");
    fs::create_dir_all(incomplete_root.join("broken/.git")).unwrap();
    fs::write(incomplete_root.join("broken/Cargo.toml"), "[workspace]\n").unwrap();
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\", \"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.display(),
            incomplete_root.display()
        ),
    )
    .unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    fs::write(&cargo, "#!/bin/sh\nrm -rf target\n").unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .env("HOME", work.path().join("missing-home"))
        .env("PATH", path)
        .assert()
        .code(2)
        .stdout(contains("Run complete: cleaned=1"));
}

#[cfg(unix)]
#[test]
fn exit_code_one_for_cargo_failure() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let project = root.join("project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(project.join("target/blob"), vec![0; 2048]).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.display()
        ),
    )
    .unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    fs::write(&cargo, "#!/bin/sh\nprintf failed >&2\nexit 7\n").unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .env("HOME", work.path().join("missing-home"))
        .env("PATH", path)
        .assert()
        .code(1)
        .stdout(contains("Run complete: cleaned=0"))
        .stdout(contains("errors=1"));
}

#[cfg(unix)]
#[test]
fn exit_code_one_outranks_incomplete_scan() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let project = root.join("project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(project.join("target/blob"), vec![0; 2048]).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let incomplete_root = work.path().join("incomplete-root");
    fs::create_dir_all(incomplete_root.join("broken/.git")).unwrap();
    fs::write(incomplete_root.join("broken/Cargo.toml"), "[workspace]\n").unwrap();
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\", \"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.display(),
            incomplete_root.display()
        ),
    )
    .unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    fs::write(&cargo, "#!/bin/sh\nexit 7\n").unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .env("HOME", work.path().join("missing-home"))
        .env("PATH", path)
        .assert()
        .code(1)
        .stdout(contains("Run complete: cleaned=0"));
}

#[test]
fn exit_code_two_for_no_scan_with_durable_discovery_block() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(project.join("target/blob"), vec![0; 2048]).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let canonical = project.canonicalize().unwrap();
    store.upsert_project(&canonical, SystemTime::now()).unwrap();
    store
        .mark_worktree_discovery_failed(&canonical, SystemTime::now(), "git failed")
        .unwrap();
    drop(store);

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--no-scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(contains("Dry run"));
}

#[test]
fn exit_code_zero_for_safety_only_skip() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let project = root.join("project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(project.join("target/blob"), vec![0; 2048]).unwrap();
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"24h\"\n",
            root.display()
        ),
    )
    .unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .assert()
        .code(0)
        .stdout(contains("recent_write=1"));
}

#[test]
fn exit_code_one_for_config_and_lock_failures() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = work.path().join("cargo-ran");
    let cargo = bin.join("cargo");
    fs::write(&cargo, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    #[cfg(unix)]
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let invalid = work.path().join("invalid.toml");
    fs::write(&invalid, "unknown_key = true\n").unwrap();
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&invalid)
        .env("HOME", work.path().join("missing-home"))
        .env("PATH", &path)
        .assert()
        .code(1);

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().display()),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("daemon.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", work.path().join("missing-home"))
        .env("PATH", &path)
        .assert()
        .code(1);
    assert!(!marker.exists());
}

#[test]
fn service_help_lists_only_explicit_lifecycle_actions() {
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["service", "--help"])
        .assert()
        .success()
        .stdout(contains("install"))
        .stdout(contains("status"))
        .stdout(contains("start"))
        .stdout(contains("stop"))
        .stdout(contains("refresh"))
        .stdout(contains("restart"))
        .stdout(contains("uninstall"));
}

#[test]
fn service_status_prints_definition_enablement_and_process_state_separately() {
    let work = tempfile::tempdir().unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["service", "status"])
        .env("HOME", work.path())
        .assert()
        .success()
        .stdout(contains("Installed: no"))
        .stdout(contains("Enabled: no"))
        .stdout(contains("Running: no"));
}

#[cfg(unix)]
#[test]
fn health_and_status_keep_cleanup_outcome_when_service_probe_fails() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let home = work.path().join("home");
    let bin = work.path().join("bin");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let config = work.path().join("config.toml");
    let state = work.path().join("state");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    let (manager_name, definition) = if cfg!(target_os = "macos") {
        (
            "launchctl",
            home.join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist"),
        )
    } else {
        (
            "systemctl",
            home.join(".config/systemd/user/car-go-clean.service"),
        )
    };
    fs::create_dir_all(definition.parent().unwrap()).unwrap();
    fs::write(&definition, "legacy definition").unwrap();
    write_executable(
        &bin.join(manager_name),
        "#!/bin/sh\nprintf 'service probe denied\\n' >&2\nexit 1\n",
    );
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    for subcommand in ["health", "status"] {
        let mut command = Command::cargo_bin("car-go-clean").unwrap();
        command
            .arg(subcommand)
            .args(["--json", "--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("HOME", &home)
            .env("PATH", &path);
        if subcommand == "health" {
            command.arg("--skip-cargo");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{subcommand}");
        let report = terminal_report(&output.stdout, subcommand);
        assert_eq!(
            report["data"]["service"]["installed"],
            serde_json::Value::Null
        );
        assert_eq!(
            report["data"]["service"]["enabled"],
            serde_json::Value::Null
        );
        assert_eq!(
            report["data"]["service"]["running"],
            serde_json::Value::Null
        );
        assert_eq!(
            report["data"]["service"]["protected_roots"],
            serde_json::Value::Null
        );
        assert_eq!(
            report["data"]["service"]["warning"]["kind"],
            "service_probe_failed"
        );
        assert!(report["data"]["service"]["warning"]["detail"]
            .as_str()
            .unwrap()
            .contains("service probe denied"));
        assert_eq!(
            report["outcome"]["reasons"],
            serde_json::json!(["generation_missing", "scan_incomplete"])
        );

        let mut text_command = Command::cargo_bin("car-go-clean").unwrap();
        text_command
            .arg(subcommand)
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("HOME", &home)
            .env("PATH", &path);
        if subcommand == "health" {
            text_command.arg("--skip-cargo");
        }
        text_command
            .assert()
            .code(2)
            .stdout(contains("Service installed: <unknown>"))
            .stdout(contains("Service warning: service_probe_failed"))
            .stdout(contains("service probe denied"));
    }

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success();
    for subcommand in ["health", "status"] {
        let mut command = Command::cargo_bin("car-go-clean").unwrap();
        command
            .arg(subcommand)
            .args(["--json", "--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("HOME", &home)
            .env("PATH", &path);
        if subcommand == "health" {
            command.arg("--skip-cargo");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{subcommand}");
        let report = terminal_report(&output.stdout, subcommand);
        assert_eq!(report["outcome"]["reasons"], serde_json::json!([]));
        assert_eq!(
            report["data"]["service"]["warning"]["kind"],
            "service_probe_failed"
        );
    }

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["service", "status"])
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .failure()
        .stderr(contains("service probe denied"));
}

#[cfg(unix)]
#[test]
fn top_level_diagnostics_keep_known_service_state_when_capture_is_malformed() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let home = work.path().join("home");
    let bin = work.path().join("bin");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let config = work.path().join("config.toml");
    let state = work.path().join("state");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    let (manager_name, manager_body, definition, malformed_definition) = if cfg!(
        target_os = "macos"
    ) {
        (
                "launchctl",
                "#!/bin/sh\ncase \"$1\" in\n  print-disabled) printf 'disabled services = {\\n}\\n' ;;\n  print) exit 0 ;;\n  *) exit 64 ;;\nesac\n",
                home.join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist"),
                "<!-- car-go-clean-service-environment-v1 -->\n",
            )
    } else {
        (
                "systemctl",
                "#!/bin/sh\ncase \"$2\" in\n  show-environment) exit 0 ;;\n  is-enabled) printf 'enabled\\n' ;;\n  is-active) printf 'active\\n' ;;\n  *) exit 64 ;;\nesac\n",
                home.join(".config/systemd/user/car-go-clean.service"),
                "# car-go-clean-service-environment-v1\nEnvironment=unquoted\n",
            )
    };
    fs::create_dir_all(definition.parent().unwrap()).unwrap();
    fs::write(&definition, malformed_definition).unwrap();
    write_executable(&bin.join(manager_name), manager_body);
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["status", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report = terminal_report(&output.stdout, "status");
    assert_eq!(report["data"]["service"]["installed"], true);
    assert_eq!(report["data"]["service"]["enabled"], true);
    assert_eq!(report["data"]["service"]["running"], true);
    assert_eq!(
        report["data"]["service"]["protected_roots"],
        serde_json::Value::Null
    );
    assert_eq!(
        report["data"]["service"]["warning"]["kind"],
        "service_definition_unreadable"
    );
    assert!(report["data"]["service"]["warning"]["detail"]
        .as_str()
        .unwrap()
        .contains("malformed captured"));
}

#[cfg(unix)]
#[test]
fn service_diagnostics_expose_installed_roots_with_service_definition_provenance() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let home = work.path().join("home");
    let bin = work.path().join("bin");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let config = work.path().join("config.toml");
    let state = work.path().join("state");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    let (manager_name, manager_body, definition, captured_definition) = if cfg!(target_os = "macos")
    {
        (
                "launchctl",
                "#!/bin/sh\ncase \"$1\" in\n  print-disabled) printf 'disabled services = {\\n}\\n' ;;\n  print) exit 0 ;;\n  *) exit 64 ;;\nesac\n",
                home.join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist"),
                "<!-- car-go-clean-service-environment-v1 -->\n<key>EnvironmentVariables</key>\n<dict>\n<key>HOME</key>\n<string>/service/home</string>\n<key>CARGO_HOME</key>\n<string>/service/cargo</string>\n</dict>\n",
            )
    } else {
        (
                "systemctl",
                "#!/bin/sh\ncase \"$2\" in\n  show-environment) exit 0 ;;\n  is-enabled) printf 'enabled\\n' ;;\n  is-active) printf 'active\\n' ;;\n  *) exit 64 ;;\nesac\n",
                home.join(".config/systemd/user/car-go-clean.service"),
                "# car-go-clean-service-environment-v1\nEnvironment=\"CARGO_HOME=/service/cargo\"\nEnvironment=\"HOME=/service/home\"\nExecStart=/bin/true\n",
            )
    };
    fs::create_dir_all(definition.parent().unwrap()).unwrap();
    fs::write(&definition, captured_definition).unwrap();
    write_executable(&bin.join(manager_name), manager_body);
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["status", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report = terminal_report(&output.stdout, "status");
    let roots = report["data"]["service"]["protected_roots"]
        .as_array()
        .unwrap();
    assert!(!roots.is_empty());
    assert!(roots.iter().all(|root| {
        root["path"].is_string()
            && root["kind"].is_string()
            && root["provenance"] == "service_definition"
    }));
    assert!(roots.iter().any(|root| {
        root["path"] == "/service/cargo"
            && root["kind"] == "cargo"
            && root["provenance"] == "service_definition"
    }));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["service", "status"])
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(contains("Installed service protected roots"))
        .stdout(contains("/service/cargo (cargo, service_definition)"));
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

#[test]
fn run_help_explains_default_scan_and_safety_flags() {
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(contains(
            "Scan for projects, then run one cleanup review/cycle now",
        ))
        .stdout(contains(
            "Show what would be cleaned without invoking Cargo",
        ))
        .stdout(contains(
            "Use cached discovery state instead of scanning first",
        ))
        .stdout(contains(
            "Include projects under managed cache or container storage",
        ))
        .stdout(contains("Include projects used by running processes"))
        .stdout(contains(
            "Bypass scan-error, activity, and quiet-period gates; managed storage still",
        ))
        .stdout(contains("requires --include-managed-cache"));
}

#[cfg(unix)]
#[test]
fn run_no_scan_prunes_physically_excluded_cached_alias_before_review() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let work = tempfile::tempdir().unwrap();
    let library = work.path().join("Library");
    let physical = library.join("copied-crate");
    let alias = work.path().join("legacy-crate");
    fs::create_dir_all(physical.join("target")).unwrap();
    fs::write(physical.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(physical.join("target/blob.bin"), vec![0; 4096]).unwrap();
    symlink(&physical, &alias).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\nexcludes = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().display(),
            library.display(),
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&alias, SystemTime::now()).unwrap();
    drop(store);

    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work.path().join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--no-scan", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", path)
        .assert()
        .code(2)
        .stdout(contains("Run complete: cleaned=0"));

    assert!(!marker.exists());
    assert!(physical.join("target/blob.bin").exists());
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert_eq!(store.all_projects().unwrap().len(), 1);
}

#[test]
fn config_command_keeps_warning_off_round_trippable_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.toml");
    let round_trip = dir.path().join("round-trip.toml");
    fs::write(
        &input,
        format!(
            "scan_dirs = [\"{}\"]\nexcludes = [\"vendor\"]\n",
            dir.path().display()
        ),
    )
    .unwrap();

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

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["config", "--config"])
        .arg(&round_trip)
        .assert()
        .success()
        .stdout(predicate::str::contains("scan_dirs"));
}

#[test]
fn config_command_allows_only_an_absent_implicit_default() {
    let work = tempfile::tempdir().unwrap();
    let home = work.path().join("home");
    let xdg_config = work.path().join("config-root");
    fs::create_dir_all(&home).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("config")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .assert()
        .success()
        .stdout(contains("scan_dirs"));

    let explicit = work.path().join("missing.toml");
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["config", "--config"])
        .arg(&explicit)
        .env("HOME", &home)
        .assert()
        .code(1)
        .stderr(contains(format!("read {}", explicit.display())));
}

#[cfg(unix)]
#[test]
fn config_command_rejects_a_dangling_explicit_path() {
    use std::os::unix::fs::symlink;

    let work = tempfile::tempdir().unwrap();
    let explicit = work.path().join("config.toml");
    symlink(work.path().join("missing-target.toml"), &explicit).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["config", "--config"])
        .arg(&explicit)
        .env("HOME", work.path().join("home"))
        .assert()
        .code(1)
        .stderr(contains(format!("read {}", explicit.display())));
}

#[cfg(unix)]
#[test]
fn config_command_rejects_a_dangling_implicit_default_ancestor() {
    use std::os::unix::fs::symlink;

    let work = tempfile::tempdir().unwrap();
    let xdg_config = work.path().join("config-root");
    fs::create_dir(&xdg_config).unwrap();
    let dangling_ancestor = xdg_config.join("car-go-clean");
    symlink(
        work.path().join("missing-config-directory"),
        &dangling_ancestor,
    )
    .unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("config")
        .env("HOME", work.path().join("home"))
        .env("XDG_CONFIG_HOME", &xdg_config)
        .assert()
        .code(1)
        .stderr(contains(format!(
            "resolve symlink {}",
            dangling_ancestor.display()
        )));
}

#[test]
fn commands_reject_empty_or_relative_xdg_roots() {
    let work = tempfile::tempdir().unwrap();
    let home = work.path().join("home");
    fs::create_dir_all(&home).unwrap();

    for value in ["", "relative/config"] {
        Command::cargo_bin("car-go-clean")
            .unwrap()
            .arg("config")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", value)
            .assert()
            .code(1)
            .stderr(contains("XDG_CONFIG_HOME"))
            .stderr(contains("nonempty absolute path"));
    }
    for value in ["", "relative/state"] {
        Command::cargo_bin("car-go-clean")
            .unwrap()
            .arg("stats")
            .env("HOME", &home)
            .env("XDG_STATE_HOME", value)
            .assert()
            .code(1)
            .stderr(contains("XDG_STATE_HOME"))
            .stderr(contains("nonempty absolute path"));
    }
}

#[cfg(unix)]
#[test]
fn config_migrate_renames_legacy_excludes_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\nexcludes = [\"vendor\"]\n",
            dir.path().display()
        ),
    )
    .unwrap();

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

    assert!(load(&config).unwrap().warnings().is_empty());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["config", "migrate", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(contains("No migration needed"));
}

#[test]
fn run_dry_run_scans_fresh_state_by_default() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='fresh-dry-run'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--all", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Authority: generation="))
        .stdout(contains("Dry run"))
        .stdout(contains("Total projects: 1"))
        .stdout(contains("Cleanable projects: 1"))
        .stdout(contains(project.join("target").display().to_string()));

    assert!(project.join("target/debug/blob.bin").exists());
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert_eq!(store.all_projects().unwrap().len(), 1);
}

#[test]
fn custom_config_can_discover_protected_storage_but_requires_managed_storage_opt_in() {
    let work = tempfile::tempdir().unwrap();
    let home = work.path().join("home");
    let project = home.join(".rustup/toolchains/stable/copied-crate");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='copied-crate'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\nexcludes = []\ntarget_quiet_period = \"1ms\"\n",
            home.display()
        ),
    )
    .unwrap();

    let skipped_state = work.path().join("skipped-state");
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--force", "--all", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&skipped_state)
        .env("HOME", &home)
        .assert()
        .success()
        .stdout(contains("Total projects: 1"))
        .stdout(contains("Cleanable projects: 0"))
        .stdout(contains("Skipped projects: 1"))
        .stdout(contains("managed_cache=1"));
    let store = Store::open(skipped_state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert_eq!(store.all_projects().unwrap().len(), 1);

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args([
            "run",
            "--dry-run",
            "--force",
            "--include-managed-cache",
            "--all",
            "--config",
        ])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("included-state"))
        .env("HOME", &home)
        .assert()
        .success()
        .stdout(contains("Total projects: 1"))
        .stdout(contains("Cleanable projects: 1"))
        .stdout(contains(project.join("target").display().to_string()));
}

#[test]
fn run_no_scan_uses_only_cached_state() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--no-scan", "--all", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(predicate::str::contains("Scan complete").not())
        .stdout(contains("Total projects: 0"))
        .stdout(contains("Cleanable projects: 0"));

    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert!(store.all_projects().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn persisted_incomplete_origin_keeps_all_review_paths_incomplete_after_diagnostics_age_out() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let (config, state, home) = seed_incomplete_diagnostic_state(&work);
    let database = state.join("state.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "
            DELETE FROM worktree_discovery_failures;
            UPDATE errors SET ts = 1;
            ",
        )
        .unwrap();

    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cargo_marker = work.path().join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", cargo_marker.display()),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--no-scan", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .code(2);
    assert!(!cargo_marker.exists());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .assert()
        .code(2);

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["status", "--refresh", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .assert()
        .code(2);
}

#[test]
fn dry_run_no_scan_rejects_target_replaced_after_matching_generation() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("tree");
    let project = root.join("proj");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/original.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\noverride_excludes = []\ntarget_quiet_period = \"1ms\"\n",
            root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    fs::rename(project.join("target"), project.join("target-observed")).unwrap();
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("target/replacement.bin"), vec![0; 4096]).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args([
            "run",
            "--dry-run",
            "--no-scan",
            "--force",
            "--all",
            "--config",
        ])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(0)
        .stdout(contains("Total projects: 1"))
        .stdout(contains("Cleanable projects: 0"));
}

#[cfg(unix)]
#[test]
fn run_scans_fresh_state_before_real_cleanup() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();

    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='fresh-real-run'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", work.path().join("missing-home"))
        .env("PATH", path)
        .assert()
        .success()
        .stdout(contains("Authority: generation="))
        .stdout(contains("Run complete: cleaned=1"));

    assert!(!project.join("target").exists());
}

#[cfg(unix)]
#[test]
fn run_aborts_before_cargo_when_scan_persistence_fails() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let db_path = state.join("state.db");
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(project.canonicalize().unwrap(), SystemTime::now())
        .unwrap();
    drop(store);
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER reject_discovery_failure
            BEFORE INSERT ON worktree_discovery_failures
            BEGIN
                SELECT RAISE(FAIL, 'injected discovery persistence failure');
            END;
            ",
        )
        .unwrap();

    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work.path().join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            "#!/bin/sh\ntouch '{}'\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", path)
        .assert()
        .failure()
        .stderr(contains("injected discovery persistence failure"));

    assert!(!marker.exists());
    assert!(project.join("target/blob.bin").exists());
}

#[cfg(unix)]
#[test]
fn run_rolls_back_atomic_scan_when_project_upsert_fails() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let db_path = state.join("state.db");
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(project.canonicalize().unwrap(), SystemTime::now())
        .unwrap();
    let projects_before = store.all_projects().unwrap();
    let diagnostics_before = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    drop(store);
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch(
            "
            CREATE TRIGGER reject_project_upsert
            BEFORE INSERT ON projects
            BEGIN
                SELECT RAISE(FAIL, 'injected project persistence failure');
            END;
            ",
        )
        .unwrap();

    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work.path().join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            "#!/bin/sh\ntouch '{}'\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", path)
        .assert()
        .failure()
        .stderr(contains("injected project persistence failure"));

    assert!(!marker.exists());
    assert!(project.join("target/blob.bin").exists());
    let store = Store::open(&db_path).unwrap();
    assert_eq!(store.all_projects().unwrap(), projects_before);
    assert_eq!(
        store.errors_since(SystemTime::UNIX_EPOCH).unwrap(),
        diagnostics_before
    );
    let inspection = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(
        inspection
            .query_row("SELECT COUNT(*) FROM discovery_generations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        inspection
            .query_row("SELECT COUNT(*) FROM project_observations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn version_prints_package_version() {
    let mut cmd = Command::cargo_bin("car-go-clean").unwrap();
    cmd.arg("version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn health_and_status_without_generation_are_incomplete_in_text_and_json() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();
    let state = work.path().join("state");
    let home = work.path().join("home");
    fs::create_dir(&home).unwrap();

    for subcommand in ["health", "status"] {
        let mut json_command = Command::cargo_bin("car-go-clean").unwrap();
        json_command
            .arg(subcommand)
            .args(["--json", "--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("HOME", &home);
        if subcommand == "health" {
            json_command.arg("--skip-cargo");
        }
        let output = json_command.output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{subcommand}");
        let report = terminal_report(&output.stdout, subcommand);
        assert_eq!(
            report["outcome"]["reasons"],
            serde_json::json!(["generation_missing", "scan_incomplete"])
        );

        let mut text_command = Command::cargo_bin("car-go-clean").unwrap();
        text_command
            .arg(subcommand)
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("HOME", &home);
        if subcommand == "health" {
            text_command.arg("--skip-cargo");
        }
        text_command
            .assert()
            .code(2)
            .stdout(contains("Outcome: incomplete (code=2)"))
            .stdout(contains("Reasons: generation_missing, scan_incomplete"))
            .stdout(contains("Service installed: no"))
            .stdout(contains("Service enabled: no"))
            .stdout(contains("Service running: no"));
    }
}

#[test]
fn health_and_status_json_share_authority_diagnostics() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home) = seed_incomplete_diagnostic_state(&work);
    let canonical_root = work.path().join("root").canonicalize().unwrap();
    let mut reports = Vec::new();

    for subcommand in ["health", "status"] {
        let mut command = Command::cargo_bin("car-go-clean").unwrap();
        command
            .arg(subcommand)
            .args(["--json", "--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("HOME", &home);
        if subcommand == "health" {
            command.arg("--skip-cargo");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{subcommand}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["outcome"],
            serde_json::json!({
                "code": 2,
                "kind": "incomplete",
                "reasons": ["origin_incomplete", "scan_incomplete"]
            })
        );

        assert_eq!(
            report["data"]["config_source"],
            config.to_string_lossy().as_ref()
        );
        assert_eq!(
            report["data"]["canonical_scope_roots"]["scan_dirs"][0],
            canonical_root.to_string_lossy().as_ref()
        );
        assert_eq!(
            report["data"]["canonical_scope_roots"]["project_dirs"],
            serde_json::json!([])
        );
        assert_eq!(report["policy_hash"].as_str().unwrap().len(), 64);
        assert!(report["data"]["current_generation"]["id"].as_i64().unwrap() > 0);
        assert_eq!(
            report["data"]["current_generation"]["policy_hash"],
            report["policy_hash"]
        );
        assert!(report["data"]["protected_roots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|root| root.get("path").is_some()
                && root.get("kind").is_some()
                && root.get("provenance").is_some()));
        assert_eq!(
            report["data"]["incomplete_origins"][0]["configured_path"],
            work.path().join("root").to_string_lossy().as_ref()
        );
        assert!(report["data"]["incomplete_origins"][0]["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()));
        assert!(report["data"]
            .get("service_environment_divergence")
            .is_some());
        assert_eq!(
            report["data"]["service"],
            serde_json::json!({
                "installed": false,
                "enabled": false,
                "running": false,
                "protected_roots": null,
                "warning": null
            })
        );
        reports.push(report);
    }

    assert_eq!(reports[0]["data"], reports[1]["data"]);
    assert_eq!(reports[0]["policy_hash"], reports[1]["policy_hash"]);
    assert_eq!(reports[0]["generation"], reports[1]["generation"]);
}

#[test]
fn health_and_status_report_recent_pathless_scan_errors_as_incomplete() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .record_error(&car_go_clean::store::ErrorRecord {
            id: 0,
            ts: SystemTime::now(),
            category: "scan".to_string(),
            path: None,
            message: "scan failed before resolving a path".to_string(),
        })
        .unwrap();
    drop(store);

    for subcommand in ["health", "status"] {
        let mut command = Command::cargo_bin("car-go-clean").unwrap();
        command
            .arg(subcommand)
            .args(["--json", "--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state);
        if subcommand == "health" {
            command.arg("--skip-cargo");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{subcommand}");
        let report = terminal_report(&output.stdout, subcommand);
        assert_eq!(
            report["outcome"]["reasons"],
            serde_json::json!(["scan_incomplete"])
        );
        assert!(report["generation"].as_i64().unwrap() > 0);
        assert_eq!(report["scan_errors"][0]["path"], serde_json::Value::Null);
    }
}

#[test]
fn health_and_status_text_share_authority_diagnostics() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home) = seed_incomplete_diagnostic_state(&work);

    for subcommand in ["health", "status"] {
        let mut command = Command::cargo_bin("car-go-clean").unwrap();
        command
            .arg(subcommand)
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("HOME", &home);
        if subcommand == "health" {
            command.arg("--skip-cargo");
        }
        command
            .assert()
            .code(2)
            .stdout(contains("Cleanup authority"))
            .stdout(contains("Config source:"))
            .stdout(contains("Canonical scan roots:"))
            .stdout(contains("Policy hash:"))
            .stdout(contains("Current generation: id="))
            .stdout(contains("boot_session="))
            .stdout(contains("Protected roots:"))
            .stdout(contains("Incomplete origins:"))
            .stdout(contains("canonical="))
            .stdout(contains("Outcome: incomplete (code=2)"))
            .stdout(contains("Reasons: origin_incomplete, scan_incomplete"));
    }
}

#[test]
fn every_operator_json_command_ends_with_the_same_public_envelope() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();
    let state = work.path().join("state");
    let home = work.path().join("home");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .assert()
        .success();

    let invocations = [
        ("health", vec!["health", "--json", "--skip-cargo"]),
        ("status", vec!["status", "--json"]),
        ("projects", vec!["projects", "--json"]),
        ("scan", vec!["scan", "--json"]),
        ("run", vec!["run", "--dry-run", "--no-scan", "--json"]),
        ("stats", vec!["stats", "--json"]),
        ("logs", vec!["logs", "--errors-only", "--json"]),
    ];
    for (expected_command, args) in invocations {
        let mut command = Command::cargo_bin("car-go-clean").unwrap();
        command.args(args);
        if !matches!(expected_command, "stats" | "logs") {
            command.args(["--config"]).arg(&config);
        }
        let output = command
            .args(["--state-dir"])
            .arg(&state)
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{expected_command} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        terminal_report(&output.stdout, expected_command);
    }
}

#[test]
fn status_text_and_json_present_the_same_authority_facts() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    let json_output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["status", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    let report = terminal_report(&json_output.stdout, "status");
    let policy_hash = report["policy_hash"].as_str().unwrap();
    let generation = report["generation"].as_i64().unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["status", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains(policy_hash))
        .stdout(contains(format!("id={generation}")));
}

#[test]
fn json_failure_after_parsing_keeps_stdout_parseable() {
    let work = tempfile::tempdir().unwrap();
    let invalid_config = work.path().join("invalid.toml");
    fs::write(&invalid_config, "scan_dirs = [").unwrap();
    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--json", "--config"])
        .arg(&invalid_config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let report = terminal_report(&output.stdout, "scan");
    assert_eq!(
        report["outcome"],
        serde_json::json!({
            "code": 1,
            "kind": "failed",
            "reasons": ["command_failed"]
        })
    );
    assert!(!output.stderr.is_empty());
}

#[test]
fn scan_run_stats_work_with_fake_cargo() {
    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    for subcommand in ["scan", "run"] {
        let mut cmd = Command::cargo_bin("car-go-clean").unwrap();
        cmd.arg(subcommand);
        if subcommand == "run" {
            cmd.args(["--force", "--no-scan"]);
        }
        cmd.args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("PATH", &path)
            .env("HOME", work.path().join("missing-home"))
            .assert()
            .success();
    }

    let mut cmd = Command::cargo_bin("car-go-clean").unwrap();
    cmd.arg("stats")
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Bytes recovered"))
        .stdout(contains(project.display().to_string()));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("status")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", work.path().join("missing-home"))
        .assert()
        .success()
        .stdout(contains("Source: run (pre-clean snapshot)"));
}

#[cfg(unix)]
#[test]
fn cargo_failure_is_audited_without_recovery_accounting() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nrm -f target/removed.bin\nprintf 'cargo failed: λ\\n' >&2\nexit 7\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();

    let root = work.path().join("tree");
    let project = root.join("proj");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(project.join("target/removed.bin"), vec![0; 2048]).unwrap();
    fs::write(project.join("target/retained.bin"), vec![0; 1024]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", work.path().join("missing-home"))
        .env("PATH", &path)
        .assert()
        .failure()
        .stdout(contains("Run complete: cleaned=0"))
        .stdout(contains("errors=1"));

    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 0);
    assert_eq!(run.bytes_recovered, 0);
    assert_eq!(run.errors_count, 1);
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        0
    );
    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].exit_code, Some(7));
    assert!(events[0].bytes_before > events[0].bytes_after);
    assert_eq!(events[0].stderr_excerpt, "cargo failed: λ\n");
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert!(errors.iter().any(|error| {
        error.category == "clean"
            && error.message.contains("cargo clean exited 7")
            && error.message.contains("cargo failed: λ")
    }));
    assert!(store.all_projects().unwrap()[0].last_cleaned_at.is_none());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["stats", "--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Bytes recovered: 0"))
        .stdout(contains("Failed clean attempts: 1"));

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["stats", "--json", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stats: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stats["data"]["total_bytes"], 0);
    assert_eq!(stats["data"]["failed_clean_attempts"], 1);
}

#[cfg(unix)]
#[test]
fn post_cargo_measurement_failure_exits_one_without_losing_the_audit_event() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nrm -rf target\nprintf 'replacement' > target\nprintf 'cargo warning\\n' >&2\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();

    let root = work.path().join("tree");
    let project = root.join("proj");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 2048]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", work.path().join("missing-home"))
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = terminal_report(&output.stdout, "run");
    assert_eq!(report["outcome"]["kind"], "failed");
    assert_eq!(
        report["outcome"]["reasons"],
        serde_json::json!(["measurement_failed"])
    );
    assert_eq!(report["data"]["cleaned"], 0);
    assert_eq!(report["data"]["bytes_recovered"], 0);
    assert_eq!(report["data"]["errors"], 1);

    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let run = store.last_run().unwrap();
    assert_eq!(run.projects_cleaned, 0);
    assert_eq!(run.bytes_recovered, 0);
    assert_eq!(run.errors_count, 1);
    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].exit_code, Some(0));
    assert_eq!(events[0].stderr_excerpt, "cargo warning\n");
    assert_eq!(events[0].bytes_after, events[0].bytes_before);
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].category, "clean");
    assert!(errors[0]
        .message
        .contains("measure target after cargo clean"));
    assert!(store.all_projects().unwrap()[0].last_cleaned_at.is_none());
}

#[cfg(unix)]
#[test]
fn combined_cargo_and_measurement_failures_retain_both_reasons() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("tree");
    let project = root.join("both-fail");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 2048]).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_executable(
        &bin.join("cargo"),
        "#!/bin/sh\nrm -rf target\nprintf replacement > target\nprintf cargo-failed >&2\nexit 7\n",
    );
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", work.path().join("home"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let report = terminal_report(&output.stdout, "run");
    assert_eq!(
        report["outcome"]["reasons"],
        serde_json::json!(["cargo_failed", "measurement_failed"])
    );
    assert_eq!(report["data"]["errors"], 2);

    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let events = store.clean_events_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].exit_code, Some(7));
    assert_eq!(events[0].outcome, CleanAttemptOutcome::CargoNonzero);
    assert_eq!(
        store.failed_clean_attempts(SystemTime::UNIX_EPOCH).unwrap(),
        1
    );
    assert_eq!(
        store.total_bytes_recovered(SystemTime::UNIX_EPOCH).unwrap(),
        0
    );
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert_eq!(errors.len(), 2);
    assert_eq!(
        errors
            .iter()
            .filter(|error| error.message.contains("cargo clean exited 7"))
            .count(),
        1
    );
    assert_eq!(
        errors
            .iter()
            .filter(|error| error.message.contains("measure target after cargo clean"))
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn command_execution_failure_uses_cleanup_failed_not_cargo_failed() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("tree");
    for name in ["first", "second"] {
        let project = root.join(name);
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(project.join("target/blob.bin"), vec![0; 2048]).unwrap();
    }
    std::thread::sleep(Duration::from_millis(10));
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_executable(
        &bin.join("cargo"),
        "#!/bin/sh\nrm -f \"$0\"\nrm -rf target\nexit 0\n",
    );
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", work.path().join("home"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let report = terminal_report(&output.stdout, "run");
    assert_eq!(
        report["outcome"]["reasons"],
        serde_json::json!(["cleanup_failed"])
    );
    assert_eq!(report["data"]["cargo_failures"], 0);
    assert_eq!(report["data"]["measurement_failures"], 0);
    assert_eq!(report["data"]["cleanup_failures"], 1);
    assert_eq!(report["data"]["errors"], 1);
}

#[test]
fn run_dry_run_reports_without_invoking_cargo_clean() {
    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\necho cargo should not run >&2\nexit 2\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .arg("--dry-run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(contains("Dry run"))
        .stdout(contains("Cleanable projects: 1"))
        .stdout(contains("Cleanable target preview:"))
        .stdout(contains(project.join("target").display().to_string()));

    assert!(project.join("target/debug/blob.bin").exists());
}

#[test]
fn non_forced_cli_reviews_honor_durable_discovery_blocks_and_force_bypasses_them() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/router");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let canonical = project.canonicalize().unwrap();
    store.upsert_project(&canonical, SystemTime::now()).unwrap();
    store.replace_linked_worktrees(&canonical, &[]).unwrap();
    store
        .mark_worktree_discovery_failed(&canonical, SystemTime::now(), "git failed")
        .unwrap();

    for args in [
        vec!["projects", "--all"],
        vec!["run", "--dry-run"],
        vec!["status", "--refresh"],
    ] {
        let mut cmd = Command::cargo_bin("car-go-clean").unwrap();
        cmd.args(args)
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .assert()
            .code(2);
    }

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--force"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(contains("Cleanable projects: 1"));
}

#[test]
fn pathless_scan_error_keeps_cached_review_commands_incomplete() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("tree");
    let project = root.join("project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 2048]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    store.upsert_project(&project, SystemTime::now()).unwrap();
    store
        .record_error(&car_go_clean::store::ErrorRecord {
            id: 0,
            ts: SystemTime::now(),
            category: "scan".to_string(),
            path: None,
            message: "scan failed before resolving a path".to_string(),
        })
        .unwrap();
    drop(store);

    for args in [
        vec!["projects", "--all"],
        vec!["status", "--refresh"],
        vec!["run", "--dry-run", "--no-scan", "--force"],
    ] {
        Command::cargo_bin("car-go-clean")
            .unwrap()
            .args(args)
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .assert()
            .code(2);
    }
}

#[cfg(unix)]
#[test]
fn cli_reviews_normalize_alias_only_linked_provenance_without_a_prior_scan() {
    use std::os::unix::fs::symlink;

    let work = tempfile::tempdir().unwrap();
    let primary = work.path().join("tree/router");
    let child = work.path().join("tree/linked");
    let child_alias = work.path().join("tree/linked-alias");
    fs::create_dir_all(primary.join(".git")).unwrap();
    fs::create_dir_all(child.join("target/debug")).unwrap();
    fs::write(primary.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(child.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(child.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();
    symlink(&child, &child_alias).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();
    store
        .upsert_project(&canonical_child, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&child_alias))
        .unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "git failed")
        .unwrap();
    drop(store);

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2);

    let store = Store::open(state.join("state.db")).unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&child_alias))
        .unwrap();
    store
        .upsert_project(&canonical_child, SystemTime::now())
        .unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "git failed")
        .unwrap();
    drop(store);

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--no-scan", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(contains("Cleanable projects: 0"));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--no-scan", "--force", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(contains("Cleanable projects: 0"));
}

#[cfg(unix)]
#[test]
fn cli_run_blocks_canonical_child_for_broken_alias_in_active_provenance() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work.path().join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            "#!/bin/sh\ntouch '{}'\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();

    let primary = work.path().join("tree/router");
    let child = work.path().join("tree/linked");
    let child_alias = work.path().join("tree/linked-alias");
    fs::create_dir_all(primary.join(".git")).unwrap();
    fs::create_dir_all(child.join("target/debug")).unwrap();
    fs::write(primary.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(child.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(child.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();
    symlink(&child, &child_alias).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();
    store
        .upsert_project(&canonical_child, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&child_alias))
        .unwrap();
    store
        .mark_worktree_discovery_failed(
            &canonical_primary,
            SystemTime::now(),
            "active legacy failure",
        )
        .unwrap();
    fs::remove_file(&child_alias).unwrap();
    drop(store);

    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", &path)
        .assert()
        .code(2)
        .stdout(contains("cleaned=0"))
        .stdout(contains("skipped=0"));
    assert!(child.join("target/debug/blob.bin").exists());
    assert!(!marker.exists());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--force", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(contains("Cleanable projects: 0"));
}

#[cfg(unix)]
#[test]
fn cli_run_blocks_canonical_child_for_retargeted_alias_in_active_provenance() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let work = tempfile::tempdir().unwrap();
    let unrelated_root = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work.path().join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            "#!/bin/sh\ntouch '{}'\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();

    let primary = work.path().join("tree/router");
    let child = work.path().join("tree/linked");
    let unrelated = unrelated_root.path().join("unrelated");
    let child_alias = work.path().join("tree/linked-alias");
    fs::create_dir_all(primary.join(".git")).unwrap();
    fs::create_dir_all(child.join("target/debug")).unwrap();
    fs::write(primary.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(child.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(child.join("target/debug/blob.bin"), vec![0; 4096]).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    fs::write(unrelated.join("Cargo.toml"), "[workspace]\n").unwrap();
    symlink(&child, &child_alias).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();
    let _canonical_unrelated = unrelated.canonicalize().unwrap();
    store
        .upsert_project(&canonical_child, SystemTime::now())
        .unwrap();
    store
        .upsert_project(&child_alias, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&child_alias))
        .unwrap();
    store
        .mark_worktree_discovery_failed(
            &canonical_primary,
            SystemTime::now(),
            "active legacy failure",
        )
        .unwrap();
    fs::remove_file(&child_alias).unwrap();
    symlink(&unrelated, &child_alias).unwrap();
    drop(store);

    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2);

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", &path)
        .assert()
        .code(2)
        .stdout(contains("cleaned=0"))
        .stdout(contains("skipped=0"));
    assert!(child.join("target/debug/blob.bin").exists());
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn cli_blocks_v4_primary_alias_associations_after_fresh_canonical_failure() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    for retarget in [false, true] {
        let work = tempfile::tempdir().unwrap();
        let primary = work.path().join("primary");
        let replacement = work.path().join("replacement");
        let alias = work.path().join("legacy-primary-alias");
        let child = work.path().join("child");
        for checkout in [&primary, &replacement] {
            fs::create_dir_all(checkout.join(".git")).unwrap();
            fs::write(checkout.join("Cargo.toml"), "[workspace]\n").unwrap();
        }
        fs::create_dir_all(child.join("target")).unwrap();
        fs::write(child.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(child.join("target/blob.bin"), vec![0; 2048]).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        symlink(&primary, &alias).unwrap();
        let canonical_primary = primary.canonicalize().unwrap();
        let canonical_child = child.canonicalize().unwrap();

        let state = work.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let db_path = state.join("state.db");
        {
            let store = Store::open(&db_path).unwrap();
            store.migrate().unwrap();
            store
                .upsert_project(&canonical_child, SystemTime::now())
                .unwrap();
        }
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                DROP TABLE linked_worktrees;
                CREATE TABLE linked_worktrees (
                    primary_path TEXT NOT NULL,
                    linked_path TEXT NOT NULL,
                    PRIMARY KEY (primary_path, linked_path)
                );
                CREATE INDEX idx_linked_worktrees_linked
                    ON linked_worktrees(linked_path);
                DROP TABLE worktree_discovery_failures;
                CREATE TABLE worktree_discovery_failures (
                    primary_path TEXT PRIMARY KEY,
                    failed_at INTEGER NOT NULL,
                    message TEXT NOT NULL
                );
                DELETE FROM schema_version WHERE version >= 5;
                ",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
                rusqlite::params![alias.to_str().unwrap(), canonical_child.to_str().unwrap()],
            )
            .unwrap();
        }
        fs::remove_file(&alias).unwrap();
        if retarget {
            symlink(&replacement, &alias).unwrap();
        }
        {
            let store = Store::open(&db_path).unwrap();
            store.migrate().unwrap();
            store
                .mark_worktree_discovery_failed(
                    &canonical_primary,
                    SystemTime::now(),
                    "fresh canonical failure",
                )
                .unwrap();
        }

        let scan_root = work.path().join("scope");
        fs::create_dir_all(&scan_root).unwrap();
        let config = work.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
                scan_root.display()
            ),
        )
        .unwrap();
        let bin_dir = work.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let marker = work.path().join("cargo-ran");
        let fake_cargo = bin_dir.join("cargo");
        fs::write(
            &fake_cargo,
            format!(
                "#!/bin/sh\ntouch '{}'\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
        let mut path = bin_dir.into_os_string();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());

        Command::cargo_bin("car-go-clean")
            .unwrap()
            .args(["projects", "--all"])
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .assert()
            .code(2);

        Command::cargo_bin("car-go-clean")
            .unwrap()
            .arg("run")
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("PATH", &path)
            .assert()
            .code(2)
            .stdout(contains("cleaned=0"));
        assert!(!marker.exists());
        assert!(child.join("target/blob.bin").exists());
    }
}

#[cfg(unix)]
#[test]
fn run_dry_run_records_unreadable_targets_in_error_logs() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!("scan_dirs = [\"{}\"]\n", work.path().join("tree").display()),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    let target = project.join("target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .arg("--dry-run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Skipped projects: 1"));

    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("logs")
        .arg("--errors-only")
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("[review]"))
        .stdout(contains(target.display().to_string()))
        .stdout(contains("target read error"));
}

#[test]
fn projects_lists_cleanability_and_supports_json() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("projects")
        .arg("--all")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("cleanable"))
        .stdout(contains(project.display().to_string()));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("projects")
        .arg("--json")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("\"decision\""))
        .stdout(contains("\"cleanable\""));
}

#[test]
fn projects_default_is_compact_and_all_shows_full_list() {
    let work = tempfile::tempdir().unwrap();
    let tree = work.path().join("tree");
    let first = tree.join("proj-00");
    let last = tree.join("proj-24");
    for idx in 0..25 {
        let project = tree.join(format!("proj-{idx:02}"));
        fs::create_dir_all(project.join("target/debug")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(project.join("target/debug/blob.bin"), vec![0; 1024]).unwrap();
    }
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            tree.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("projects")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Projects"))
        .stdout(contains("Cleanable projects: 25"))
        .stdout(contains("Cleanable target preview:"))
        .stdout(contains(first.join("target").display().to_string()))
        .stdout(predicate::str::contains(last.join("target").display().to_string()).not())
        .stdout(contains("Use `projects --all` to show all 25 rows."));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("projects")
        .arg("--all")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains(last.display().to_string()));
}

#[test]
fn status_prints_safe_cleaning_summary() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .arg("--dry-run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("status")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Last review:"))
        .stdout(contains("Source: dry-run"))
        .stdout(contains("Cache"))
        .stdout(contains("Review"))
        .stdout(contains("Recovery"))
        .stdout(contains("Schedule"))
        .stdout(contains("Cleanable projects: 1"))
        .stdout(contains("Cleanable bytes: 16.0 KiB"))
        .stdout(predicate::str::contains("16,384 B").not())
        .stdout(contains("Total bytes recovered (all time): 0 B"));
}

#[test]
fn status_reports_no_review_before_explicit_review() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("tree/proj");
    fs::create_dir_all(project.join("target/debug")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("status")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Cached projects: 1"))
        .stdout(contains("Last review: <none>"))
        .stdout(predicate::str::contains("Cleanable projects:").not());
}

#[test]
fn status_prints_scheduler_timing() {
    let work = tempfile::tempdir().unwrap();
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        "clean_interval = \"1h\"\nscan_interval = \"2h\"\ntarget_quiet_period = \"1ms\"\n",
    )
    .unwrap();
    let state = work.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    let now = SystemTime::now();
    store
        .record_scheduler_status(
            now,
            now.checked_sub(Duration::from_secs(60)).unwrap(),
            now + Duration::from_secs(3600),
        )
        .unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("status")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(contains("Clean interval: 1 hour"))
        .stdout(contains("Scheduler state: recorded"))
        .stdout(contains("Next scheduled clean: overdue by"))
        .stdout(contains("Scan interval: 2 hours"))
        .stdout(contains("Next scheduled scan: in"))
        .stdout(contains("Reasons: generation_missing, scan_incomplete"));
}

#[test]
fn dry_run_syncs_stale_cached_projects_before_status_snapshot() {
    let work = tempfile::tempdir().unwrap();
    let live_project = work.path().join("tree/live");
    let stale_project = work.path().join("tree/stale");
    for project in [&live_project, &stale_project] {
        fs::create_dir_all(project.join("target/debug")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();
    }
    std::thread::sleep(Duration::from_millis(10));

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    fs::remove_dir_all(&stale_project).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .arg("--dry-run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Total projects: 1"))
        .stdout(contains("Cleanable projects: 1"));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("status")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Cached projects: 2"))
        .stdout(contains("Cleanable projects: 1"));
}

#[test]
fn run_dry_run_syncs_stale_cached_projects_before_review() {
    let work = tempfile::tempdir().unwrap();
    let live_project = work.path().join("tree/live");
    let stale_project = work.path().join("tree/stale");
    for project in [&live_project, &stale_project] {
        fs::create_dir_all(project.join("target/debug")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(project.join("target/debug/blob.bin"), vec![0; 16 * 1024]).unwrap();
    }

    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("tree").display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("scan")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();

    fs::remove_dir_all(&stale_project).unwrap();
    std::thread::sleep(Duration::from_millis(10));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .arg("--dry-run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Total projects: 1"))
        .stdout(contains("Cleanable projects: 1"));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("status")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Cached projects: 2"));
}

#[cfg(unix)]
#[test]
fn dry_run_without_all_persists_and_prints_review_metadata() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);

    let (review_id, stdout) = create_review_plan(&config, &state, &home, &path);

    assert!(review_id > 0);
    for label in [
        "Policy hash: ",
        "Discovery generation: ",
        "Created: ",
        "Expires: ",
        "Candidate bytes: ",
    ] {
        assert!(stdout.contains(label), "missing {label:?} in {stdout}");
    }
    let connection = rusqlite::Connection::open(state.join("state.db")).unwrap();
    let plan_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM review_plans", [], |row| row.get(0))
        .unwrap();
    let target_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM review_plan_targets WHERE plan_id = ?1",
            [review_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(plan_count, 1);
    assert_eq!(target_count, 1);
}

#[cfg(unix)]
#[test]
fn dry_run_json_is_newline_delimited_and_structurally_usable() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let events = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2, "{stdout}");
    assert_eq!(events[0]["format_version"], 1);
    assert_eq!(events[0]["event"], "scan");
    assert!(events[0]["data"]["generation"].as_i64().unwrap() > 0);
    assert!(events[1]["data"]["review"]["id"].as_i64().unwrap() > 0);
    assert_eq!(events[1]["data"]["reviews"].as_array().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn run_json_stream_versions_target_events_and_ends_with_failed_report() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    write_executable(
        &work.path().join("bin/cargo"),
        "#!/bin/sh\nprintf cargo-failed >&2\nexit 7\n",
    );

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
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

    assert_eq!(output.status.code(), Some(1));
    let events = json_lines(&output.stdout);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["format_version"], 1);
    assert_eq!(events[0]["event"], "target");
    assert!(events[0].get("outcome").is_none());
    assert_eq!(
        events[0]["data"]["project"],
        work.path()
            .join("root/project")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert!(events[0]["data"]["target"]
        .as_str()
        .unwrap()
        .ends_with("/target"));

    let terminal = terminal_report(&output.stdout, "run");
    assert_eq!(terminal["outcome"]["code"], 1);
    assert_eq!(terminal["outcome"]["kind"], "failed");
    assert_eq!(
        terminal["outcome"]["reasons"],
        serde_json::json!(["cargo_failed"])
    );
    assert_eq!(terminal["review_id"], review_id);
    assert_eq!(terminal["data"]["errors"], 1);
}

#[cfg(unix)]
#[test]
fn cargo_failure_outranks_incomplete_scan_without_losing_either_reason() {
    let work = tempfile::tempdir().unwrap();
    let good_root = work.path().join("good");
    let project = good_root.join("project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();
    let incomplete_root = work.path().join("incomplete");
    fs::create_dir_all(incomplete_root.join("broken/.git")).unwrap();
    fs::write(incomplete_root.join("broken/Cargo.toml"), "[workspace]\n").unwrap();
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\", \"{}\"]\ntarget_quiet_period = \"1ns\"\n",
            good_root.display(),
            incomplete_root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let home = work.path().join("home");
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_executable(
        &bin.join("cargo"),
        "#!/bin/sh\nprintf cargo-failed >&2\nexit 9\n",
    );
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--force", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let report = terminal_report(&output.stdout, "run");
    assert_eq!(report["outcome"]["kind"], "failed");
    assert_eq!(
        report["outcome"]["reasons"],
        serde_json::json!(["cargo_failed", "origin_incomplete", "scan_incomplete"])
    );
    assert_eq!(report["data"]["errors"], 1);
    assert!(!report["scan_errors"].as_array().unwrap().is_empty());
}

#[test]
fn safety_skips_alone_do_not_make_json_outcome_incomplete() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let project = root.join("project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();
    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let report = terminal_report(&output.stdout, "projects");
    assert_eq!(
        report["outcome"],
        serde_json::json!({"code": 0, "kind": "complete", "reasons": []})
    );
    assert!(report["data"]["reviews"][0]["decision"]
        .get("skipped")
        .is_some());
}

#[test]
fn projects_json_without_current_generation_is_incomplete_for_a_stable_reason() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(work.path().join("state"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        terminal_report(&output.stdout, "projects")["outcome"]["reasons"],
        serde_json::json!(["generation_missing", "scan_incomplete"])
    );
}

#[test]
fn projects_json_with_only_stale_authority_reports_invalid_generation() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    let first_config = work.path().join("first.toml");
    fs::write(
        &first_config,
        format!("scan_dirs = [\"{}\"]\n", root.display()),
    )
    .unwrap();
    let changed_config = work.path().join("changed.toml");
    fs::write(
        &changed_config,
        format!(
            "scan_dirs = [\"{}\"]\noverride_excludes = [\"new\"]\n",
            root.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&first_config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success();
    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--json", "--config"])
        .arg(&changed_config)
        .args(["--state-dir"])
        .arg(&state)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        terminal_report(&output.stdout, "projects")["outcome"]["reasons"],
        serde_json::json!(["generation_invalid", "scan_incomplete"])
    );
}

#[test]
fn dry_run_no_scan_without_authority_creates_no_review_plan() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let config = work.path().join("config.toml");
    fs::write(&config, format!("scan_dirs = [\"{}\"]\n", root.display())).unwrap();
    let state = work.path().join("state");

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--no-scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2)
        .stdout(contains("No review ID was created"));

    let connection = rusqlite::Connection::open(state.join("state.db")).unwrap();
    let plan_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM review_plans", [], |row| row.get(0))
        .unwrap();
    assert_eq!(plan_count, 0);
}

#[test]
fn run_all_without_dry_run_is_a_cli_error() {
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--all"])
        .assert()
        .code(1)
        .stderr(contains("--all"))
        .stderr(contains("--dry-run"));
}

#[test]
fn review_conflicts_with_mutating_run_options_but_allows_json() {
    for option in [
        "--dry-run",
        "--no-scan",
        "--include-managed-cache",
        "--include-active",
        "--force",
        "--all",
    ] {
        Command::cargo_bin("car-go-clean")
            .unwrap()
            .args(["run", "--review", "1", option])
            .assert()
            .code(1)
            .stderr(contains("cannot be used with"));
    }

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", "1", "--json"])
        .assert()
        .code(1)
        .stderr(
            predicate::str::is_match("unexpected argument.*--json")
                .unwrap()
                .not(),
        );
}

#[cfg(unix)]
#[test]
fn reviewed_run_executes_only_persisted_cleanable_targets() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["approved"]);
    let pending = work.path().join("root/pending");
    fs::create_dir_all(&pending).unwrap();
    fs::write(pending.join("Cargo.toml"), "[workspace]\n").unwrap();

    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    fs::create_dir_all(pending.join("target")).unwrap();
    fs::write(pending.join("target/new.bin"), vec![0; 4096]).unwrap();

    let marker = work.path().join("cargo-calls");
    write_executable(
        &work.path().join("bin/cargo"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" >> '{}'\nrm -rf \"$3\"\n",
            marker.display()
        ),
    );

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "reviewed run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let approved = work.path().join("root/approved").canonicalize().unwrap();
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim(),
        approved.display().to_string()
    );
    assert!(!approved.join("target").exists());
    assert!(pending.join("target/new.bin").exists());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let cleaning = format!(
        "Cleaning {} (project {})",
        approved.join("target").display(),
        approved.display()
    );
    assert!(stdout.contains(&cleaning), "{stdout}");
    assert!(stdout.find(&cleaning) < stdout.find("Run complete:").or(Some(usize::MAX)));
}

#[cfg(unix)]
#[test]
fn reviewed_run_preserves_persisted_managed_storage_opt_in() {
    let work = tempfile::tempdir().unwrap();
    let home = work.path().join("home");
    let project = home.join(".cargo/registry/src/project");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(project.join("target/blob.bin"), vec![0; 4096]).unwrap();
    let config = work.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\noverride_excludes = []\ntarget_quiet_period = \"1ns\"\n",
            home.display()
        ),
    )
    .unwrap();
    let state = work.path().join("state");
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = work.path().join("cargo-ran");
    write_executable(
        &bin.join("cargo"),
        &format!("#!/bin/sh\ntouch '{}'\nrm -rf \"$3\"\n", marker.display()),
    );
    let mut path = bin.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let path = PathBuf::from(path);

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--include-managed-cache", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let review_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Review ID: "))
        .unwrap()
        .parse::<i64>()
        .unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(contains("cleaned=1"));

    assert!(marker.exists());
    assert!(!project.join("target").exists());
}

#[cfg(unix)]
#[test]
fn reviewed_run_removes_target_that_became_unsafe_without_invoking_cargo() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    let project = work.path().join("root/project");
    fs::rename(project.join("target"), project.join("target-reviewed")).unwrap();
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("target/replacement.bin"), vec![0; 4096]).unwrap();
    let marker = work.path().join("cargo-ran");
    write_executable(
        &work.path().join("bin/cargo"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success()
        .stdout(contains("cleaned=0"))
        .stdout(contains("skipped=1"));

    assert!(!marker.exists());
    assert!(project.join("target/replacement.bin").exists());
}

#[cfg(unix)]
#[test]
fn superseded_generation_rejects_entire_review_without_cargo() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    let marker = work.path().join("cargo-ran");
    write_executable(
        &work.path().join("bin/cargo"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .code(1)
        .stderr(contains("review plan"));

    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn policy_change_rejects_old_no_scan_and_review_authority() {
    let work = tempfile::tempdir().unwrap();
    let (policy_a, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&policy_a, &state, &home, &path);
    let policy_b = work.path().join("policy-b.toml");
    fs::write(
        &policy_b,
        format!(
            "scan_dirs = [\"{}\"]\noverride_excludes = [\"policy-b-only\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("root").display()
        ),
    )
    .unwrap();
    let marker = work.path().join("cargo-ran");
    write_executable(
        &work.path().join("bin/cargo"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&policy_b)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&policy_a)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .code(1)
        .stderr(contains("review plan"));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--no-scan", "--config"])
        .arg(&policy_a)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .code(2)
        .stdout(contains("matching discovery generation"));

    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn policy_a_b_a_prunes_an_untouched_a1_plan_without_cargo() {
    let work = tempfile::tempdir().unwrap();
    let (policy_a, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&policy_a, &state, &home, &path);
    let policy_b = work.path().join("policy-b.toml");
    fs::write(
        &policy_b,
        format!(
            "scan_dirs = [\"{}\"]\noverride_excludes = [\"policy-b-only\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("root").display()
        ),
    )
    .unwrap();
    let marker = work.path().join("cargo-ran");
    write_executable(
        &work.path().join("bin/cargo"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&policy_b)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success();

    let inspection = rusqlite::Connection::open(state.join("state.db")).unwrap();
    let plan_generation_valid = inspection
        .query_row(
            "
            SELECT generation.authority_valid
            FROM review_plans AS plan
            JOIN discovery_generations AS generation
              ON generation.id = plan.generation_id
            WHERE plan.id = ?1
            ",
            [review_id],
            |row| row.get::<_, bool>(0),
        )
        .unwrap();
    assert!(!plan_generation_valid);
    drop(inspection);

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&policy_a)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success();
    let replacement_generation = rusqlite::Connection::open(state.join("state.db"))
        .unwrap()
        .query_row(
            "
            SELECT id
            FROM discovery_generations
            WHERE authority_valid = 1
            ORDER BY id DESC
            LIMIT 1
            ",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args([
            "run",
            "--review",
            &review_id.to_string(),
            "--json",
            "--config",
        ])
        .arg(&policy_a)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        terminal_report(&output.stdout, "run")["outcome"]["reasons"],
        serde_json::json!(["review_generation_mismatch"])
    );
    assert_eq!(
        terminal_report(&output.stdout, "run")["data"]["review_plan_rejection"],
        serde_json::json!({
            "kind": "generation_mismatch",
            "replacing_generation": replacement_generation,
        })
    );
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn policy_mismatched_review_exits_one_without_cargo() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    let changed_config = work.path().join("changed-config.toml");
    fs::write(
        &changed_config,
        format!(
            "scan_dirs = [\"{}\"]\noverride_excludes = [\"new-exclusion\"]\ntarget_quiet_period = \"1ms\"\n",
            work.path().join("root").display()
        ),
    )
    .unwrap();
    let marker = work.path().join("cargo-ran");
    write_executable(
        &work.path().join("bin/cargo"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&changed_config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .code(1)
        .stderr(contains("policy"));

    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn reviewed_run_fails_cleanly_while_daemon_holds_lock() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    let marker = work.path().join("cargo-ran");
    write_executable(
        &work.path().join("bin/cargo"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("daemon.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .code(1)
        .stderr(contains("another car-go-clean process is running"));

    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn json_review_failures_use_stable_machine_reason_identifiers() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);

    let missing = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", "999999", "--json", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(
        terminal_report(&missing.stdout, "run")["outcome"]["reasons"],
        serde_json::json!(["review_plan_missing"])
    );
    assert_eq!(
        terminal_report(&missing.stdout, "run")["data"]["review_plan_rejection"],
        serde_json::json!({"kind": "missing"})
    );

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["scan", "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .success();
    let replacement_generation = rusqlite::Connection::open(state.join("state.db"))
        .unwrap()
        .query_row(
            "
            SELECT id
            FROM discovery_generations
            WHERE authority_valid = 1
            ORDER BY id DESC
            LIMIT 1
            ",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let superseded = Command::cargo_bin("car-go-clean")
        .unwrap()
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
    assert_eq!(superseded.status.code(), Some(1));
    assert_eq!(
        terminal_report(&superseded.stdout, "run")["outcome"]["reasons"],
        serde_json::json!(["review_generation_mismatch"])
    );
    assert_eq!(
        terminal_report(&superseded.stdout, "run")["data"]["review_plan_rejection"],
        serde_json::json!({
            "kind": "generation_mismatch",
            "replacing_generation": replacement_generation,
        })
    );
}

#[cfg(unix)]
#[test]
fn pruned_expired_review_keeps_text_and_json_expiry_diagnostics() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    let connection = rusqlite::Connection::open(state.join("state.db")).unwrap();
    connection
        .execute(
            "UPDATE review_plans SET expires_at = 0 WHERE id = ?1",
            [review_id],
        )
        .unwrap();
    drop(connection);

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
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

    assert_eq!(output.status.code(), Some(1));
    let report = terminal_report(&output.stdout, "run");
    assert_eq!(
        report["outcome"]["reasons"],
        serde_json::json!(["review_plan_expired"])
    );
    assert_eq!(
        report["data"]["review_plan_rejection"],
        serde_json::json!({"kind": "expired"})
    );

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .assert()
        .code(1)
        .stderr(contains("review plan has expired"));
}

#[cfg(unix)]
#[test]
fn json_policy_mismatch_has_its_own_reason() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    let changed_config = work.path().join("changed.toml");
    fs::write(
        &changed_config,
        format!(
            "scan_dirs = [\"{}\"]\noverride_excludes = [\"new\"]\ntarget_quiet_period = \"1ns\"\n",
            work.path().join("root").display()
        ),
    )
    .unwrap();

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args([
            "run",
            "--review",
            &review_id.to_string(),
            "--json",
            "--config",
        ])
        .arg(&changed_config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        terminal_report(&output.stdout, "run")["outcome"]["reasons"],
        serde_json::json!(["review_policy_mismatch"])
    );
    assert_eq!(
        terminal_report(&output.stdout, "run")["data"]["review_plan_rejection"],
        serde_json::json!({"kind": "policy_mismatch"})
    );
}

#[cfg(unix)]
#[test]
fn json_lock_failure_is_a_failed_terminal_report() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["project"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("daemon.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
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

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        terminal_report(&output.stdout, "run")["outcome"]["reasons"],
        serde_json::json!(["lock_unavailable"])
    );
}

#[cfg(unix)]
#[test]
fn reviewed_run_continues_after_cargo_failure_and_prints_each_attempt() {
    let work = tempfile::tempdir().unwrap();
    let (config, state, home, path) = review_fixture(&work, &["fails", "succeeds"]);
    let (review_id, _) = create_review_plan(&config, &state, &home, &path);
    let marker = work.path().join("cargo-calls");
    write_executable(
        &work.path().join("bin/cargo"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" >> '{}'\ncase \"$PWD\" in\n  */fails) printf failed >&2; exit 7 ;;\n  *) rm -rf \"$3\"; exit 0 ;;\nesac\n",
            marker.display()
        ),
    );

    let output = Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--review", &review_id.to_string(), "--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &home)
        .env("PATH", &path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let calls = fs::read_to_string(&marker).unwrap();
    assert_eq!(calls.lines().count(), 2, "{calls}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.matches("Cleaning ").count(), 2, "{stdout}");
    assert!(stdout.contains("cleaned=1"), "{stdout}");
    assert!(stdout.contains("errors=1"), "{stdout}");
    assert!(!work.path().join("root/succeeds/target").exists());
    assert!(work.path().join("root/fails/target").exists());
}

#[cfg(unix)]
#[test]
fn cli_physically_classifies_frozen_trusted_and_untrusted_primary_rows() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    for (trusted, class_path, _decision) in [
        (true, "Library/Caches/replacement", "skipped:managed_cache"),
        (
            true,
            "OrbStack/docker/replacement",
            "skipped:container_storage",
        ),
        (false, "Library/Caches/replacement", "skipped:managed_cache"),
        (
            false,
            "OrbStack/docker/replacement",
            "skipped:container_storage",
        ),
    ] {
        let work = tempfile::tempdir().unwrap();
        let work_path = work.path().canonicalize().unwrap();
        let original = work_path.join("original");
        let frozen_primary = work_path.join("frozen-primary");
        let replacement = work_path.join(class_path);
        let child = work_path.join("historical-child");
        for path in [&original, &replacement, &child] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(replacement.join("Cargo.toml"), "[package]\n").unwrap();
        fs::create_dir_all(replacement.join("target")).unwrap();
        fs::write(replacement.join("target/blob.bin"), vec![0; 4096]).unwrap();
        let canonical_replacement = replacement.canonicalize().unwrap();
        let canonical_child = child.canonicalize().unwrap();
        let state = work_path.join("state");
        fs::create_dir_all(&state).unwrap();
        let db_path = state.join("state.db");

        if trusted {
            fs::create_dir_all(&frozen_primary).unwrap();
            fs::write(frozen_primary.join("Cargo.toml"), "[package]\n").unwrap();
            let store = Store::open(&db_path).unwrap();
            store.migrate().unwrap();
            store
                .upsert_project(&frozen_primary, SystemTime::now())
                .unwrap();
            store
                .replace_linked_worktrees(&frozen_primary, std::slice::from_ref(&canonical_child))
                .unwrap();
            drop(store);
            fs::remove_dir_all(&frozen_primary).unwrap();
            symlink(&canonical_replacement, &frozen_primary).unwrap();
        } else {
            symlink(&original, &frozen_primary).unwrap();
            {
                let store = Store::open(&db_path).unwrap();
                store.migrate().unwrap();
                store
                    .upsert_project(&frozen_primary, SystemTime::now())
                    .unwrap();
            }
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                DROP TABLE linked_worktrees;
                CREATE TABLE linked_worktrees (
                    primary_path TEXT NOT NULL,
                    linked_path TEXT NOT NULL,
                    PRIMARY KEY (primary_path, linked_path)
                );
                DELETE FROM schema_version WHERE version >= 5;
                ",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
                rusqlite::params![
                    frozen_primary.to_str().unwrap(),
                    canonical_child.to_str().unwrap()
                ],
            )
            .unwrap();
            drop(conn);
            let store = Store::open(&db_path).unwrap();
            store.migrate().unwrap();
            drop(store);
            fs::remove_file(&frozen_primary).unwrap();
            symlink(&canonical_replacement, &frozen_primary).unwrap();
        }

        let bin_dir = work_path.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let marker = work_path.join("cargo-ran");
        let fake_cargo = bin_dir.join("cargo");
        fs::write(
            &fake_cargo,
            format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
        let scan_root = work_path.join("scope");
        fs::create_dir_all(&scan_root).unwrap();
        let config = work_path.join("config.toml");
        fs::write(
            &config,
            format!(
                "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
                scan_root.display()
            ),
        )
        .unwrap();
        let mut path = bin_dir.into_os_string();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());

        Command::cargo_bin("car-go-clean")
            .unwrap()
            .args(["projects", "--all"])
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .assert()
            .code(2);
        Command::cargo_bin("car-go-clean")
            .unwrap()
            .arg("run")
            .args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("PATH", &path)
            .assert()
            .success()
            .stdout(contains("cleaned=0"));

        assert!(!marker.exists(), "trusted={trusted}, path={class_path}");
        assert!(
            replacement.join("target/blob.bin").exists(),
            "trusted={trusted}, path={class_path}"
        );
    }
}

#[cfg(unix)]
#[test]
fn cli_reused_v4_untrusted_primary_does_not_release_historical_child() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let work = tempfile::tempdir().unwrap();
    let work_path = work.path().canonicalize().unwrap();
    let original = work_path.join("original");
    let reused = work_path.join("reused-primary");
    let child = work_path.join("historical-child");
    fs::create_dir_all(&original).unwrap();
    fs::create_dir_all(child.join("target")).unwrap();
    fs::write(child.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(child.join("target/blob.bin"), vec![0; 4096]).unwrap();
    symlink(&original, &reused).unwrap();
    let canonical_original = original.canonicalize().unwrap();
    let canonical_child = child.canonicalize().unwrap();
    let state = work_path.join("state");
    fs::create_dir_all(&state).unwrap();
    let db_path = state.join("state.db");
    {
        let store = Store::open(&db_path).unwrap();
        store.migrate().unwrap();
        store
            .upsert_project(&canonical_child, SystemTime::now())
            .unwrap();
    }
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "
        DROP TABLE linked_worktrees;
        CREATE TABLE linked_worktrees (
            primary_path TEXT NOT NULL,
            linked_path TEXT NOT NULL,
            PRIMARY KEY (primary_path, linked_path)
        );
        DELETE FROM schema_version WHERE version >= 5;
        ",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO linked_worktrees (primary_path, linked_path) VALUES (?1, ?2)",
        rusqlite::params![reused.to_str().unwrap(), canonical_child.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&db_path).unwrap();
    store.migrate().unwrap();
    fs::remove_file(&reused).unwrap();
    fs::create_dir(&reused).unwrap();
    store.replace_linked_worktrees(&reused, &[]).unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_original, SystemTime::now(), "original failed")
        .unwrap();
    drop(store);

    let bin_dir = work_path.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work_path.join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let scan_root = work_path.join("scope");
    fs::create_dir_all(&scan_root).unwrap();
    let config = work_path.join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            scan_root.display()
        ),
    )
    .unwrap();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2);
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("PATH", &path)
        .assert()
        .code(2)
        .stdout(contains("cleaned=0"));

    assert!(!marker.exists());
    assert!(child.join("target/blob.bin").exists());
}

#[cfg(unix)]
#[test]
fn cli_successful_discovery_resolves_only_its_effective_scan_error() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let work_path = work.path().canonicalize().unwrap();
    let primary = work_path.join("primary");
    let linked = work_path.join("linked");
    let unrelated = work_path.join("unrelated");
    fs::create_dir_all(&primary).unwrap();
    fs::create_dir_all(linked.join("target")).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    fs::write(primary.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(linked.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(linked.join("target/blob.bin"), vec![0; 4096]).unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();
    let canonical_unrelated = unrelated.canonicalize().unwrap();
    let state = work_path.join("state");
    fs::create_dir_all(&state).unwrap();
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_project(&canonical_linked, SystemTime::now())
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&canonical_linked))
        .unwrap();
    store
        .record_error(&car_go_clean::store::ErrorRecord {
            id: 0,
            ts: SystemTime::now(),
            category: "worktree_discovery".to_string(),
            path: Some(canonical_primary.to_string_lossy().into_owned()),
            message: "git failed".to_string(),
        })
        .unwrap();
    store
        .record_error(&car_go_clean::store::ErrorRecord {
            id: 0,
            ts: SystemTime::now(),
            category: "scan".to_string(),
            path: Some(canonical_unrelated.to_string_lossy().into_owned()),
            message: "permission denied".to_string(),
        })
        .unwrap();
    store
        .mark_worktree_discovery_failed(&canonical_primary, SystemTime::now(), "git failed")
        .unwrap();
    store
        .replace_linked_worktrees(&canonical_primary, std::slice::from_ref(&canonical_linked))
        .unwrap();
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 2);
    drop(store);
    std::thread::sleep(Duration::from_millis(10));

    let bin_dir = work_path.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = work_path.join("cargo-ran");
    let fake_cargo = bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            "#!/bin/sh\ntouch '{}'\nif [ \"$1\" = clean ]; then rm -rf target; fi\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let scan_root = work_path.join("scope");
    fs::create_dir_all(&scan_root).unwrap();
    let config = work_path.join("config.toml");
    fs::write(
        &config,
        format!(
            "scan_dirs = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            scan_root.display()
        ),
    )
    .unwrap();
    let mut path = bin_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["projects", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .code(2);
    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("run")
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .env("HOME", &work_path)
        .env("PATH", &path)
        .assert()
        .code(2)
        .stdout(contains("cleaned=0"));

    assert!(!marker.exists());
    assert!(linked.join("target").exists());
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 2);
}
