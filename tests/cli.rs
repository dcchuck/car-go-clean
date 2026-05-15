use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use std::fs;
use std::time::Duration;

#[test]
fn version_prints_package_version() {
    let mut cmd = Command::cargo_bin("car-go-clean").unwrap();
    cmd.arg("version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn health_passes_with_defaults_when_cargo_check_is_skipped() {
    let state = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("car-go-clean").unwrap();
    cmd.args(["health", "--state-dir"])
        .arg(state.path())
        .arg("--skip-cargo")
        .assert()
        .success()
        .stdout(contains("OK"));
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
            cmd.arg("--force");
        }
        cmd.args(["--config"])
            .arg(&config)
            .args(["--state-dir"])
            .arg(&state)
            .env("PATH", &path)
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
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Last review:"))
        .stdout(contains("Source: dry-run"))
        .stdout(contains("Cleanable projects: 1"))
        .stdout(contains("Cleanable bytes:"));
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
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Cached projects: 1"))
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
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Cached projects: 1"));
}
