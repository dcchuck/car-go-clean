use crate::policy::{Environment, ProcessEnvironment};
use crate::storage::{current_home_dir, protected_roots_for, HostPlatform};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigWarning {
    LegacyExcludes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub scan_dirs: Vec<PathBuf>,
    pub project_dirs: Vec<PathBuf>,
    pub clean_interval: Duration,
    pub scan_interval: Duration,
    pub target_quiet_period: Duration,
    pub log_level: String,
    editable_default_excludes: Vec<String>,
    extra_excludes: Vec<String>,
    override_excludes: Option<Vec<String>>,
    warnings: Vec<ConfigWarning>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    scan_dirs: Option<Vec<PathBuf>>,
    project_dirs: Option<Vec<PathBuf>>,
    extra_excludes: Option<Vec<String>>,
    override_excludes: Option<Vec<String>>,
    excludes: Option<Vec<String>>,
    clean_interval: Option<String>,
    scan_interval: Option<String>,
    target_quiet_period: Option<String>,
    log_level: Option<String>,
}

#[derive(Serialize)]
struct ConfigOutput<'a> {
    scan_dirs: &'a [PathBuf],
    project_dirs: &'a [PathBuf],
    extra_excludes: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    override_excludes: Option<&'a [String]>,
    clean_interval: String,
    scan_interval: String,
    target_quiet_period: String,
    log_level: &'a str,
}

impl Default for Config {
    fn default() -> Self {
        let home = current_home_dir();
        let scan_dirs = if home.as_os_str().is_empty() {
            Vec::new()
        } else {
            vec![home]
        };
        Self {
            scan_dirs,
            project_dirs: Vec::new(),
            clean_interval: default_clean_interval(),
            scan_interval: default_scan_interval(),
            target_quiet_period: default_target_quiet_period(),
            log_level: default_log_level(),
            editable_default_excludes: default_excludes(),
            extra_excludes: Vec::new(),
            override_excludes: None,
            warnings: Vec::new(),
        }
    }
}

impl Config {
    pub fn effective_excludes(&self) -> Vec<String> {
        let mut values = self
            .override_excludes
            .clone()
            .unwrap_or_else(|| self.editable_default_excludes.clone());
        values.extend(self.extra_excludes.iter().cloned());
        values
    }

    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(&ConfigOutput {
            scan_dirs: &self.scan_dirs,
            project_dirs: &self.project_dirs,
            extra_excludes: &self.extra_excludes,
            override_excludes: self.override_excludes.as_deref(),
            clean_interval: humantime::format_duration(self.clean_interval).to_string(),
            scan_interval: humantime::format_duration(self.scan_interval).to_string(),
            target_quiet_period: humantime::format_duration(self.target_quiet_period).to_string(),
            log_level: &self.log_level,
        })
        .context("serialize effective configuration")
    }

    pub fn validate(&self) -> Result<()> {
        if self.clean_interval.is_zero() {
            return Err(anyhow!("clean_interval must be positive"));
        }
        if self.scan_interval.is_zero() {
            return Err(anyhow!("scan_interval must be positive"));
        }
        if self.target_quiet_period.is_zero() {
            return Err(anyhow!("target_quiet_period must be positive"));
        }
        match self.log_level.as_str() {
            "debug" | "info" | "warn" | "error" => {}
            other => Err(anyhow!(
                "log_level {other:?}: must be one of debug, info, warn, error"
            ))?,
        }
        if self.scan_dirs.is_empty() && self.project_dirs.is_empty() {
            return Err(anyhow!("scan_dirs and project_dirs cannot both be empty"));
        }
        require_absolute(&self.scan_dirs, "scan_dirs")?;
        require_absolute(&self.project_dirs, "project_dirs")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSet {
    pub state_dir: PathBuf,
    pub db_path: PathBuf,
    pub log_path: PathBuf,
    pub lock_path: PathBuf,
}

#[derive(Debug)]
pub struct ConfigMigration {
    path: PathBuf,
    before: String,
    after: String,
}

impl ConfigMigration {
    pub fn unified_diff(&self) -> String {
        similar::TextDiff::from_lines(&self.before, &self.after)
            .unified_diff()
            .header(
                &format!("{} (legacy)", self.path.display()),
                &format!("{} (migrated)", self.path.display()),
            )
            .to_string()
    }

    pub fn apply(self) -> Result<()> {
        let current = fs::read_to_string(&self.path)
            .with_context(|| format!("re-read {}", self.path.display()))?;
        if current != self.before {
            return Err(anyhow!(
                "{} changed after migration preview; refusing to overwrite it",
                self.path.display()
            ));
        }
        write_atomic(&self.path, self.after.as_bytes())
    }
}

pub fn default_path() -> PathBuf {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("car-go-clean/config.toml");
    }
    current_home_dir().join(".config/car-go-clean/config.toml")
}

pub fn paths() -> PathSet {
    let state_dir = if let Some(xdg) = env::var_os("XDG_STATE_HOME") {
        PathBuf::from(xdg).join("car-go-clean")
    } else {
        current_home_dir().join(".local/state/car-go-clean")
    };
    PathSet {
        db_path: state_dir.join("state.db"),
        log_path: state_dir.join("car-go-clean.log"),
        lock_path: state_dir.join("daemon.lock"),
        state_dir,
    }
}

pub fn load(path: impl AsRef<Path>) -> Result<Config> {
    let path = path.as_ref();
    if !path.exists() {
        let cfg = Config::default();
        cfg.validate()?;
        return Ok(cfg);
    }
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let raw: RawConfig =
        toml::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    apply_overlay(raw).with_context(|| format!("validate {}", path.display()))
}

pub fn prepare_migration(path: impl AsRef<Path>) -> Result<Option<ConfigMigration>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let before = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let raw: RawConfig =
        toml::from_str(&before).with_context(|| format!("parse {}", path.display()))?;
    if raw.excludes.is_some() && raw.override_excludes.is_some() {
        return Err(anyhow!("excludes and override_excludes cannot both be set"));
    }
    if raw.excludes.is_none() {
        return Ok(None);
    }
    apply_overlay(raw)?;

    let mut document = before
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("edit {}", path.display()))?;
    let (old_key, item) = document
        .as_table_mut()
        .remove_entry("excludes")
        .ok_or_else(|| anyhow!("legacy excludes key disappeared during migration"))?;
    let new_key = toml_edit::Key::new("override_excludes")
        .with_leaf_decor(old_key.leaf_decor().clone())
        .with_dotted_decor(old_key.dotted_decor().clone());
    document.as_table_mut().insert_formatted(&new_key, item);
    let after = document.to_string();
    let migrated: RawConfig = toml::from_str(&after).context("validate migrated configuration")?;
    if migrated.override_excludes.is_none() || migrated.excludes.is_some() {
        return Err(anyhow!(
            "migrated configuration did not preserve exclusions"
        ));
    }
    apply_overlay(migrated)?;

    Ok(Some(ConfigMigration {
        path: path.to_path_buf(),
        before,
        after,
    }))
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let temp_path = path.with_extension(format!("car-go-clean-migrate-{}.tmp", std::process::id()));
    let permissions = fs::metadata(path)
        .with_context(|| format!("read permissions for {}", path.display()))?
        .permissions();
    let mut created = false;
    let result = (|| -> Result<()> {
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .with_context(|| format!("create {}", temp_path.display()))?;
        created = true;
        temp.set_permissions(permissions)
            .with_context(|| format!("set permissions on {}", temp_path.display()))?;
        temp.write_all(contents)
            .with_context(|| format!("write {}", temp_path.display()))?;
        temp.sync_all()
            .with_context(|| format!("sync {}", temp_path.display()))?;
        drop(temp);
        fs::rename(&temp_path, path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn apply_overlay(raw: RawConfig) -> Result<Config> {
    if raw.excludes.is_some() && raw.override_excludes.is_some() {
        return Err(anyhow!("excludes and override_excludes cannot both be set"));
    }

    let legacy = raw.excludes.is_some();
    let override_field = if legacy {
        "excludes"
    } else {
        "override_excludes"
    };
    let override_excludes = raw.override_excludes.or(raw.excludes);
    let mut cfg = Config::default();

    if let Some(paths) = raw.scan_dirs {
        cfg.scan_dirs = expand_paths(paths, "scan_dirs")?;
    }
    if let Some(paths) = raw.project_dirs {
        cfg.project_dirs = expand_paths(paths, "project_dirs")?;
    }
    if let Some(values) = raw.extra_excludes {
        cfg.extra_excludes = expand_excludes(values, "extra_excludes")?;
    }
    if let Some(values) = override_excludes {
        cfg.override_excludes = Some(expand_excludes(values, override_field)?);
    }
    if let Some(value) = raw.clean_interval {
        cfg.clean_interval = parse_duration(&value, "clean_interval")?;
    }
    if let Some(value) = raw.scan_interval {
        cfg.scan_interval = parse_duration(&value, "scan_interval")?;
    }
    if let Some(value) = raw.target_quiet_period {
        cfg.target_quiet_period = parse_duration(&value, "target_quiet_period")?;
    }
    if let Some(value) = raw.log_level {
        cfg.log_level = value;
    }
    if legacy {
        cfg.warnings.push(ConfigWarning::LegacyExcludes);
    }

    cfg.validate()?;
    Ok(cfg)
}

fn parse_duration(value: &str, field: &str) -> Result<Duration> {
    humantime::parse_duration(value).with_context(|| format!("{field}: invalid duration {value:?}"))
}

fn expand_paths(paths: Vec<PathBuf>, field: &str) -> Result<Vec<PathBuf>> {
    paths
        .into_iter()
        .map(|path| expand_path(path, field))
        .collect()
}

fn expand_excludes(values: Vec<String>, field: &str) -> Result<Vec<String>> {
    values
        .into_iter()
        .map(|value| {
            let started_absolute = Path::new(&value).is_absolute();
            let expanded = expand_path(PathBuf::from(value), field)?;
            if started_absolute && !expanded.is_absolute() {
                return Err(anyhow!(
                    "{field} absolute entry {} became relative",
                    expanded.display()
                ));
            }
            Ok(expanded.to_string_lossy().into_owned())
        })
        .collect()
}

fn require_absolute(paths: &[PathBuf], field: &str) -> Result<()> {
    for path in paths {
        if !path.is_absolute() {
            return Err(anyhow!("{field} entry {} must be absolute", path.display()));
        }
    }
    Ok(())
}

fn expand_path(path: PathBuf, field: &str) -> Result<PathBuf> {
    let raw = path.to_string_lossy();
    let expanded_env = expand_env_vars(&raw, field)?;
    if expanded_env == "~" {
        return Ok(current_home_dir());
    }
    if let Some(rest) = expanded_env.strip_prefix("~/") {
        return Ok(current_home_dir().join(rest));
    }
    Ok(PathBuf::from(expanded_env))
}

fn expand_env_vars(input: &str, field: &str) -> Result<String> {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            let mut name = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(c) => name.push(c),
                    None => return Err(anyhow!("{field}: unterminated environment variable")),
                }
            }
            if name.is_empty() {
                return Err(anyhow!(
                    "{field}: environment variable name cannot be empty"
                ));
            }
            let value = env::var(&name).with_context(|| {
                format!("{field}: environment variable {name} is not set or not Unicode")
            })?;
            out.push_str(&value);
            continue;
        }
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c == '_' || c.is_ascii_alphanumeric() {
                name.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if name.is_empty() {
            out.push('$');
        } else {
            let value = env::var(&name).with_context(|| {
                format!("{field}: environment variable {name} is not set or not Unicode")
            })?;
            out.push_str(&value);
        }
    }
    Ok(out)
}

fn default_clean_interval() -> Duration {
    Duration::from_secs(24 * 60 * 60)
}

fn default_scan_interval() -> Duration {
    Duration::from_secs(24 * 60 * 60)
}

fn default_target_quiet_period() -> Duration {
    Duration::from_secs(2 * 60 * 60)
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_excludes() -> Vec<String> {
    default_excludes_for(
        &current_home_dir(),
        HostPlatform::current(),
        &ProcessEnvironment,
    )
}

fn default_excludes_for(
    home: &Path,
    platform: HostPlatform,
    environment: &dyn Environment,
) -> Vec<String> {
    let mut excludes = vec![".git".to_string(), "node_modules".to_string()];
    excludes.extend(
        protected_roots_for(platform, home, environment)
            .into_iter()
            .map(|root| root.path.to_string_lossy().into_owned()),
    );

    excludes
}

#[cfg(test)]
mod default_exclude_tests {
    use super::*;
    use std::ffi::OsString;

    struct EmptyEnvironment;

    impl Environment for EmptyEnvironment {
        fn var_os(&self, _name: &str) -> Option<OsString> {
            None
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn macos_defaults_anchor_managed_and_platform_paths_to_home() {
        let excludes = default_excludes_for(
            Path::new("/Users/tester"),
            HostPlatform::MacOs,
            &EmptyEnvironment,
        );

        assert_eq!(
            excludes,
            strings(&[
                ".git",
                "node_modules",
                "/Users/tester/.cargo",
                "/Users/tester/.rustup",
                "/Users/tester/.cache",
                "/Users/tester/.bun/install/cache",
                "/Users/tester/go/pkg/mod",
                "/Users/tester/.colima",
                "/Users/tester/.lima",
                "/Users/tester/.local/share/containers",
                "/Users/tester/Library",
                "/Users/tester/.Trash",
                "/Users/tester/OrbStack",
            ])
        );
        assert!(!excludes.iter().any(|entry| entry == "target"));
    }

    #[test]
    fn linux_defaults_cover_rootless_container_and_desktop_vm_storage() {
        let excludes = default_excludes_for(
            Path::new("/home/tester"),
            HostPlatform::Linux,
            &EmptyEnvironment,
        );

        assert_eq!(
            excludes,
            strings(&[
                ".git",
                "node_modules",
                "/home/tester/.cargo",
                "/home/tester/.rustup",
                "/home/tester/.cache",
                "/home/tester/.bun/install/cache",
                "/home/tester/go/pkg/mod",
                "/home/tester/.colima",
                "/home/tester/.lima",
                "/home/tester/.local/share/containers",
                "/home/tester/.local/share/docker",
                "/home/tester/.docker/desktop",
                "/home/tester/.local/share/rancher-desktop",
                "/home/tester/.local/share/Trash",
            ])
        );
        assert!(!excludes.iter().any(|entry| entry == "target"));
    }

    #[test]
    fn missing_or_relative_home_never_creates_unanchored_manager_patterns() {
        assert_eq!(
            default_excludes_for(Path::new(""), HostPlatform::MacOs, &EmptyEnvironment),
            strings(&[".git", "node_modules"])
        );
        assert_eq!(
            default_excludes_for(
                Path::new("relative-home"),
                HostPlatform::Linux,
                &EmptyEnvironment,
            ),
            strings(&[".git", "node_modules"])
        );
    }
}
