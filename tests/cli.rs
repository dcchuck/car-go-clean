use assert_cmd::Command;
use car_go_clean::store::Store;
use predicates::prelude::*;
use predicates::str::contains;
use std::fs;
use std::time::{Duration, SystemTime};

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
            "scan_dirs = []\nexcludes = [\"{}\"]\ntarget_quiet_period = \"1ms\"\n",
            library.display()
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
        .success()
        .stdout(contains("Run complete: cleaned=0"));

    assert!(!marker.exists());
    assert!(physical.join("target/blob.bin").exists());
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert!(store.all_projects().unwrap().is_empty());
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
        .stdout(contains("Scan complete\nDry run"))
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
        .success()
        .stdout(predicate::str::contains("Scan complete").not())
        .stdout(contains("Total projects: 0"))
        .stdout(contains("Cleanable projects: 0"));

    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert!(store.all_projects().unwrap().is_empty());
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
        .stdout(contains("Scan complete\nRun complete: cleaned=1"));

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
fn run_aborts_before_cargo_when_project_upsert_fails() {
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
    let errors = store.errors_since(SystemTime::UNIX_EPOCH).unwrap();
    assert!(errors.iter().any(|error| {
        error.category == "cache"
            && error.path.as_deref() == project.canonicalize().unwrap().to_str()
            && error
                .message
                .contains("injected project persistence failure")
    }));
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
            cmd.args(["--force", "--no-scan"]);
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

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .arg("status")
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Source: run (pre-clean snapshot)"));
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
            .success()
            .stdout(contains("scan_error"));
    }

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--force"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Cleanable projects: 1"));
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
        .success()
        .stdout(contains("skipped:scan_error"))
        .stdout(contains(canonical_child.display().to_string()));

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
        .success()
        .stdout(contains("Cleanable projects: 0"))
        .stdout(contains("scan_error=1"));

    Command::cargo_bin("car-go-clean")
        .unwrap()
        .args(["run", "--dry-run", "--no-scan", "--force", "--all"])
        .args(["--config"])
        .arg(&config)
        .args(["--state-dir"])
        .arg(&state)
        .assert()
        .success()
        .stdout(contains("Cleanable projects: 1"))
        .stdout(contains(
            canonical_child.join("target").display().to_string(),
        ));
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
        .success()
        .stdout(contains("cleaned=0"))
        .stdout(contains("skipped=2"));
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
        .success()
        .stdout(contains("Cleanable projects: 1"));
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
    let canonical_unrelated = unrelated.canonicalize().unwrap();
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
        .success()
        .stdout(contains(format!(
            "skipped:scan_error\tworkspace\t4096\t{}",
            canonical_child.display()
        )))
        .stdout(contains(format!(
            "skipped:no_target\tworkspace\t0\t{}",
            canonical_unrelated.display()
        )));

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
        .stdout(contains("cleaned=0"))
        .stdout(contains("skipped=3"));
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

        let config = work.path().join("config.toml");
        fs::write(&config, "scan_dirs = []\ntarget_quiet_period = \"1ms\"\n").unwrap();
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
            .success()
            .stdout(contains("skipped:scan_error"))
            .stdout(contains(canonical_child.display().to_string()));

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
        .success()
        .stdout(contains("Clean interval: 1 hour"))
        .stdout(contains("Scheduler state: recorded"))
        .stdout(contains("Next scheduled clean: overdue by"))
        .stdout(contains("Scan interval: 2 hours"))
        .stdout(contains("Next scheduled scan: in"));
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

#[cfg(unix)]
#[test]
fn cli_physically_classifies_frozen_trusted_and_untrusted_primary_rows() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    for (trusted, class_path, decision) in [
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
        let config = work_path.join("config.toml");
        fs::write(&config, "scan_dirs = []\ntarget_quiet_period = \"1ms\"\n").unwrap();
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
            .success()
            .stdout(contains(decision))
            .stdout(contains(canonical_replacement.display().to_string()));
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
    let config = work_path.join("config.toml");
    fs::write(&config, "scan_dirs = []\ntarget_quiet_period = \"1ms\"\n").unwrap();
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
        .success()
        .stdout(contains("skipped:scan_error"))
        .stdout(contains(canonical_child.display().to_string()));
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
    let config = work_path.join("config.toml");
    fs::write(&config, "scan_dirs = []\ntarget_quiet_period = \"1ms\"\n").unwrap();
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
        .success()
        .stdout(contains(format!(
            "cleanable\tworkspace\t4096\t{}",
            canonical_linked.display()
        )));
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
        .success()
        .stdout(contains("cleaned=1"));

    assert!(marker.exists());
    assert!(!linked.join("target").exists());
    let store = Store::open(state.join("state.db")).unwrap();
    store.migrate().unwrap();
    assert_eq!(store.errors_since(SystemTime::UNIX_EPOCH).unwrap().len(), 2);
}
