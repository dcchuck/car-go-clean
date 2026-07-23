use assert_cmd::Command;
use car_go_clean::store::Store;
use predicates::prelude::*;
use predicates::str::contains;
use std::fs;
use std::time::{Duration, SystemTime};

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
        .args(["run", "--dry-run", "--all"])
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
        .args(["run", "--dry-run", "--force", "--all"])
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
        .stdout(contains("skipped=1"));
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
            child_alias.display()
        )))
        .stdout(predicate::str::contains(canonical_unrelated.display().to_string()).not());

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
