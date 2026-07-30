use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use car_go_clean::config::{
    default_path, load, load_default, paths, prepare_migration, Config, ConfigWarning,
};

struct EnvironmentGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvironmentGuard {
    fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

fn environment_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[test]
fn default_config_scans_home_and_has_intervals() {
    let home = std::env::var("HOME").expect("HOME must be set for defaults");
    let cfg = Config::default();

    assert_eq!(cfg.scan_dirs, vec![PathBuf::from(&home)]);
    assert!(cfg.project_dirs.is_empty());
    assert_eq!(cfg.clean_interval, Duration::from_secs(24 * 60 * 60));
    assert_eq!(cfg.scan_interval, Duration::from_secs(24 * 60 * 60));
    assert_eq!(cfg.log_level, "info");
    let excludes = cfg.effective_excludes();
    assert!(excludes.contains(&".git".to_string()));
    assert!(excludes.contains(&"node_modules".to_string()));
    assert!(excludes.contains(
        &PathBuf::from(&home)
            .join(".cargo")
            .to_string_lossy()
            .into_owned()
    ));
    assert!(excludes.contains(
        &PathBuf::from(&home)
            .join(".rustup")
            .to_string_lossy()
            .into_owned()
    ));
    for relative in [
        ".cache",
        ".bun/install/cache",
        "go/pkg/mod",
        ".colima",
        ".lima",
        ".local/share/containers",
    ] {
        assert!(excludes.contains(
            &PathBuf::from(&home)
                .join(relative)
                .to_string_lossy()
                .into_owned(),
        ));
    }
    assert!(!excludes.contains(&"target".to_string()));

    match std::env::consts::OS {
        "macos" => {
            assert!(excludes.contains(
                &PathBuf::from(&home)
                    .join("OrbStack")
                    .to_string_lossy()
                    .into_owned()
            ));
        }
        "linux" => {
            assert!(excludes.contains(
                &PathBuf::from(&home)
                    .join(".local/share/rancher-desktop")
                    .to_string_lossy()
                    .into_owned()
            ));
        }
        _ => {}
    }
}

#[test]
fn load_missing_implicit_default_returns_defaults() {
    let cfg =
        load_default("/definitely/not/here/car-go-clean.toml").expect("missing default config");
    assert_eq!(cfg.clean_interval, Duration::from_secs(24 * 60 * 60));
}

#[test]
fn load_missing_explicit_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.toml");

    let error = format!("{:#}", load(&path).unwrap_err());

    assert!(
        error.contains(&format!("read {}", path.display())),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn load_dangling_explicit_symlink_is_an_error() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    symlink(dir.path().join("missing-target.toml"), &path).unwrap();

    let error = format!("{:#}", load(&path).unwrap_err());

    assert!(
        error.contains(&format!("read {}", path.display())),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn load_dangling_implicit_default_is_not_treated_as_absent() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    symlink(dir.path().join("missing-target.toml"), &path).unwrap();

    let error = format!("{:#}", load_default(&path).unwrap_err());

    assert!(
        error.contains(&format!("read {}", path.display())),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn load_dangling_implicit_default_ancestor_is_not_treated_as_absent() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let dangling_ancestor = dir.path().join("config-root");
    symlink(dir.path().join("missing-config-root"), &dangling_ancestor).unwrap();
    let path = dangling_ancestor.join("car-go-clean/config.toml");

    let error = format!("{:#}", load_default(&path).unwrap_err());

    assert!(
        error.contains(&format!("resolve symlink {}", dangling_ancestor.display())),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn load_inaccessible_explicit_path_is_an_error_when_permissions_are_enforced() {
    use std::os::unix::fs::PermissionsExt;

    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("private");
    let path = parent.join("config.toml");
    fs::create_dir(&parent).unwrap();
    fs::write(&path, "scan_dirs = [\"/tmp\"]\n").unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();

    let result = load(&path);

    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains(&format!("read {}", path.display())),
        "{error}"
    );
}

#[test]
fn default_target_quiet_period_is_two_hours() {
    let cfg = Config::default();

    assert_eq!(cfg.target_quiet_period, Duration::from_secs(2 * 60 * 60));
}

#[test]
fn load_file_overlays_target_quiet_period() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
target_quiet_period = "30m"
"#,
    )
    .unwrap();

    let cfg = load(&path).unwrap();

    assert_eq!(cfg.target_quiet_period, Duration::from_secs(30 * 60));
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
    assert_eq!(cfg.effective_excludes(), vec!["foo"]);
    assert!(cfg.project_dirs[0].starts_with(std::env::var("HOME").unwrap()));
}

#[test]
fn validate_rejects_bad_intervals_and_log_levels() {
    let mut cfg = Config::default();
    cfg.clean_interval = Duration::ZERO;
    assert!(cfg.validate().is_err());

    let mut cfg = Config::default();
    cfg.scan_interval = Duration::ZERO;
    assert!(cfg.validate().is_err());

    let mut cfg = Config::default();
    cfg.target_quiet_period = Duration::ZERO;
    assert!(cfg.validate().is_err());

    let mut cfg = Config::default();
    cfg.log_level = "verbose".to_string();
    assert!(cfg.validate().is_err());

    assert!(Config::default().validate().is_ok());
}

#[test]
fn partial_file_overlays_defaults_instead_of_emptying_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "target_quiet_period = \"30m\"\n").unwrap();

    let cfg = load(&path).unwrap();

    assert_eq!(cfg.scan_dirs, Config::default().scan_dirs);
    assert_eq!(cfg.project_dirs, Config::default().project_dirs);
    assert_eq!(cfg.target_quiet_period, Duration::from_secs(30 * 60));
    assert_eq!(
        cfg.effective_excludes(),
        Config::default().effective_excludes()
    );
}

#[test]
fn strict_config_rejects_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "scan_dris = [\"/tmp\"]\n").unwrap();

    let error = format!("{:#}", load(&path).unwrap_err());

    assert!(error.contains("scan_dris"), "{error}");
    assert!(error.contains("unknown field"), "{error}");
}

#[test]
fn strict_config_rejects_empty_effective_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "scan_dirs = []\nproject_dirs = []\n").unwrap();

    let error = format!("{:#}", load(&path).unwrap_err());

    assert!(error.contains("scan_dirs and project_dirs cannot both be empty"));
}

#[test]
fn strict_config_rejects_relative_scan_and_project_paths() {
    for body in [
        "scan_dirs = [\"relative/root\"]\n",
        "scan_dirs = []\nproject_dirs = [\"relative/project\"]\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, body).unwrap();

        let error = format!("{:#}", load(&path).unwrap_err());

        assert!(error.contains("must be absolute"), "{error}");
    }
}

#[test]
fn strict_config_expands_bare_and_braced_variables_in_every_path_field() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::env::set_var("CGC_SCOPE_ROOT", dir.path());
    fs::write(
        &path,
        r#"
scan_dirs = ["$CGC_SCOPE_ROOT/scan"]
project_dirs = ["${CGC_SCOPE_ROOT}/project"]
extra_excludes = ["$CGC_SCOPE_ROOT/extra"]
override_excludes = ["relative-pattern", "${CGC_SCOPE_ROOT}/absolute"]
"#,
    )
    .unwrap();

    let cfg = load(&path).unwrap();

    assert_eq!(cfg.scan_dirs, vec![dir.path().join("scan")]);
    assert_eq!(cfg.project_dirs, vec![dir.path().join("project")]);
    assert_eq!(
        cfg.effective_excludes(),
        vec![
            "relative-pattern".to_string(),
            dir.path().join("absolute").to_string_lossy().into_owned(),
            dir.path().join("extra").to_string_lossy().into_owned(),
        ]
    );
}

#[test]
fn strict_config_rejects_unset_or_unterminated_variables_in_every_path_field() {
    std::env::remove_var("CGC_DEFINITELY_UNSET");
    for body in [
        "scan_dirs = [\"$CGC_DEFINITELY_UNSET/root\"]\n",
        "project_dirs = [\"${CGC_DEFINITELY_UNSET}/project\"]\n",
        "extra_excludes = [\"$CGC_DEFINITELY_UNSET/cache\"]\n",
        "override_excludes = [\"${CGC_DEFINITELY_UNSET}/cache\"]\n",
        "excludes = [\"$CGC_DEFINITELY_UNSET/cache\"]\n",
        "scan_dirs = [\"${CGC_DEFINITELY_UNSET/root\"]\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, body).unwrap();

        assert!(load(&path).is_err(), "{body}");
    }
}

#[test]
fn legacy_excludes_loads_as_a_warned_override_but_conflicts_with_new_override() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy.toml");
    fs::write(&legacy, "scan_dirs = [\"/tmp\"]\nexcludes = [\"vendor\"]\n").unwrap();

    let cfg = load(&legacy).unwrap();

    assert_eq!(cfg.effective_excludes(), vec!["vendor".to_string()]);
    assert_eq!(cfg.warnings(), &[ConfigWarning::LegacyExcludes]);

    let conflict = dir.path().join("conflict.toml");
    fs::write(
        &conflict,
        "scan_dirs = [\"/tmp\"]\nexcludes = []\noverride_excludes = []\n",
    )
    .unwrap();
    let error = format!("{:#}", load(&conflict).unwrap_err());
    assert!(error.contains("excludes and override_excludes cannot both be set"));
}

#[test]
fn migration_renames_only_the_legacy_key_and_preserves_comments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"# operator scope
scan_dirs = ["/tmp/work"]

# intentionally broad legacy override
excludes = [
  "vendor", # generated source
]
"#,
    )
    .unwrap();

    let migration = prepare_migration(&path).unwrap().unwrap();
    let diff = migration.unified_diff();

    assert!(diff.contains("--- "));
    assert!(diff.contains("+++ "));
    assert!(diff.contains("-excludes = ["));
    assert!(diff.contains("+override_excludes = ["));
    assert!(fs::read_to_string(&path).unwrap().contains("excludes = ["));

    migration.apply().unwrap();
    let migrated = fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("# operator scope"));
    assert!(migrated.contains("# intentionally broad legacy override"));
    assert!(migrated.contains("# generated source"));
    assert!(migrated.contains("override_excludes = ["));
    assert!(!migrated.lines().any(|line| line.starts_with("excludes =")));
    assert!(load(&path).unwrap().warnings().is_empty());
}

#[cfg(unix)]
#[test]
fn migration_rejects_a_symlink_without_changing_link_or_target() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("operator.toml");
    let link = dir.path().join("config.toml");
    let original = "scan_dirs = [\"/tmp/work\"]\nexcludes = [\"vendor\"]\n";
    fs::write(&target, original).unwrap();
    symlink(&target, &link).unwrap();

    let error = format!("{:#}", prepare_migration(&link).unwrap_err());

    assert!(error.contains("symlink"), "{error}");
    assert_eq!(fs::read_link(&link).unwrap(), target);
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
}

#[test]
fn migration_is_a_noop_without_a_legacy_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "scan_dirs = [\"/tmp\"]\nextra_excludes = [\"vendor\"]\n",
    )
    .unwrap();

    assert!(prepare_migration(&path).unwrap().is_none());
}

#[test]
fn migration_refuses_conflicting_or_unknown_configuration() {
    let dir = tempfile::tempdir().unwrap();
    for body in [
        "scan_dirs = [\"/tmp\"]\nexcludes = []\noverride_excludes = []\n",
        "scan_dirs = [\"/tmp\"]\nexclude = []\n",
    ] {
        let path = dir.path().join(format!("{}.toml", body.len()));
        fs::write(&path, body).unwrap();
        assert!(prepare_migration(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), body);
    }
}

#[test]
fn config_output_round_trips_through_strict_loading() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.toml");
    let output = dir.path().join("output.toml");
    fs::write(
        &input,
        r#"
scan_dirs = ["/tmp/work"]
project_dirs = ["/opt/explicit"]
extra_excludes = ["generated"]
target_quiet_period = "45m"
"#,
    )
    .unwrap();
    let first = load(&input).unwrap();

    fs::write(&output, first.to_toml().unwrap()).unwrap();
    let second = load(&output).unwrap();

    assert_eq!(second, first);
    assert!(second.warnings().is_empty());
}

#[test]
fn default_and_state_paths_follow_xdg() {
    let _lock = environment_lock();
    let dir = tempfile::tempdir().unwrap();
    let _config = EnvironmentGuard::set("XDG_CONFIG_HOME", dir.path().join("config"));
    let _state = EnvironmentGuard::set("XDG_STATE_HOME", dir.path().join("state"));

    assert_eq!(
        default_path().unwrap(),
        dir.path().join("config/car-go-clean/config.toml")
    );

    let p = paths().unwrap();
    assert_eq!(p.state_dir, dir.path().join("state/car-go-clean"));
    assert_eq!(p.db_path, p.state_dir.join("state.db"));
    assert_eq!(p.log_path, p.state_dir.join("car-go-clean.log"));
    assert_eq!(p.lock_path, p.state_dir.join("daemon.lock"));
}

#[test]
fn xdg_config_home_rejects_empty_and_relative_roots() {
    let _lock = environment_lock();
    for value in ["", "relative/config"] {
        let _config = EnvironmentGuard::set("XDG_CONFIG_HOME", value);

        let error = format!("{:#}", default_path().unwrap_err());

        assert!(error.contains("XDG_CONFIG_HOME"), "{error}");
        assert!(error.contains("nonempty absolute path"), "{error}");
    }
}

#[test]
fn xdg_state_home_rejects_empty_and_relative_roots() {
    let _lock = environment_lock();
    for value in ["", "relative/state"] {
        let _state = EnvironmentGuard::set("XDG_STATE_HOME", value);

        let error = format!("{:#}", paths().unwrap_err());

        assert!(error.contains("XDG_STATE_HOME"), "{error}");
        assert!(error.contains("nonempty absolute path"), "{error}");
    }
}
