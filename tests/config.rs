use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use car_go_clean::config::{default_path, load, paths, Config};

#[test]
fn default_config_scans_home_and_has_intervals() {
    let home = std::env::var("HOME").expect("HOME must be set for defaults");
    let cfg = Config::default();

    assert_eq!(cfg.scan_dirs, vec![PathBuf::from(home)]);
    assert!(cfg.project_dirs.is_empty());
    assert_eq!(cfg.clean_interval, Duration::from_secs(24 * 60 * 60));
    assert_eq!(cfg.scan_interval, Duration::from_secs(7 * 24 * 60 * 60));
    assert_eq!(cfg.log_level, "info");
    assert!(cfg.excludes.contains(&"target".to_string()));
}

#[test]
fn load_missing_file_returns_defaults() {
    let cfg = load("/definitely/not/here/car-go-clean.toml").expect("missing config should load");
    assert_eq!(cfg.clean_interval, Duration::from_secs(24 * 60 * 60));
}

#[test]
fn load_file_overlays_defaults_and_expands_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::env::set_var("CGC_TEST_ROOT", dir.path());
    fs::write(
        &path,
        r#"
scan_dirs = ["$CGC_TEST_ROOT/a", "$CGC_TEST_ROOT/b"]
project_dirs = ["~/one-off"]
clean_interval = "1h"
scan_interval = "2h"
log_level = "debug"
excludes = ["foo"]
"#,
    )
    .unwrap();

    let cfg = load(&path).unwrap();

    assert_eq!(cfg.scan_dirs.len(), 2);
    assert_eq!(cfg.scan_dirs[0].file_name().unwrap(), "a");
    assert_eq!(cfg.clean_interval, Duration::from_secs(60 * 60));
    assert_eq!(cfg.scan_interval, Duration::from_secs(2 * 60 * 60));
    assert_eq!(cfg.log_level, "debug");
    assert_eq!(cfg.excludes, vec!["foo"]);
    assert!(cfg.project_dirs[0].starts_with(std::env::var("HOME").unwrap()));
}

#[test]
fn validate_rejects_bad_intervals_and_log_levels() {
    let cfg = Config {
        clean_interval: Duration::ZERO,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());

    let cfg = Config {
        scan_interval: Duration::ZERO,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());

    let cfg = Config {
        log_level: "verbose".to_string(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());

    assert!(Config::default().validate().is_ok());
}

#[test]
fn default_and_state_paths_follow_xdg() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
    std::env::set_var("XDG_STATE_HOME", dir.path().join("state"));

    assert_eq!(
        default_path(),
        dir.path().join("config/car-go-clean/config.toml")
    );

    let p = paths();
    assert_eq!(p.state_dir, dir.path().join("state/car-go-clean"));
    assert_eq!(p.db_path, p.state_dir.join("state.db"));
    assert_eq!(p.log_path, p.state_dir.join("car-go-clean.log"));
    assert_eq!(p.lock_path, p.state_dir.join("daemon.lock"));
}
