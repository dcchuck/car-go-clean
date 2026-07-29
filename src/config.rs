use crate::storage::{current_home_dir, protected_roots_for, HostPlatform};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub scan_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub project_dirs: Vec<PathBuf>,
    #[serde(default = "default_excludes")]
    pub excludes: Vec<String>,
    #[serde(default = "default_clean_interval", with = "humantime_serde")]
    pub clean_interval: Duration,
    #[serde(default = "default_scan_interval", with = "humantime_serde")]
    pub scan_interval: Duration,
    #[serde(default = "default_target_quiet_period", with = "humantime_serde")]
    pub target_quiet_period: Duration,
    #[serde(default = "default_log_level")]
    pub log_level: String,
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
            excludes: default_excludes(),
            clean_interval: default_clean_interval(),
            scan_interval: default_scan_interval(),
            target_quiet_period: default_target_quiet_period(),
            log_level: default_log_level(),
        }
    }
}

impl Config {
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
            "debug" | "info" | "warn" | "error" => Ok(()),
            other => Err(anyhow!(
                "log_level {other:?}: must be one of debug, info, warn, error"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSet {
    pub state_dir: PathBuf,
    pub db_path: PathBuf,
    pub log_path: PathBuf,
    pub lock_path: PathBuf,
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
        return Ok(Config::default());
    }
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut cfg: Config =
        toml::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    cfg.scan_dirs = expand_all(cfg.scan_dirs);
    cfg.project_dirs = expand_all(cfg.project_dirs);
    Ok(cfg)
}

fn expand_all(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().map(expand_path).collect()
}

fn expand_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    let expanded_env = expand_env_vars(&raw);
    if expanded_env == "~" {
        return current_home_dir();
    }
    if let Some(rest) = expanded_env.strip_prefix("~/") {
        return current_home_dir().join(rest);
    }
    PathBuf::from(expanded_env)
}

fn expand_env_vars(input: &str) -> String {
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
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                name.push(c);
            }
            out.push_str(&env::var(name).unwrap_or_default());
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
            out.push_str(&env::var(name).unwrap_or_default());
        }
    }
    out
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
    default_excludes_for(&current_home_dir(), HostPlatform::current())
}

fn default_excludes_for(home: &Path, platform: HostPlatform) -> Vec<String> {
    let mut excludes = vec![".git".to_string(), "node_modules".to_string()];
    excludes.extend(
        protected_roots_for(home, platform)
            .into_iter()
            .map(|root| root.path.to_string_lossy().into_owned()),
    );

    excludes
}

#[cfg(test)]
mod default_exclude_tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn macos_defaults_anchor_managed_and_platform_paths_to_home() {
        let excludes = default_excludes_for(Path::new("/Users/tester"), HostPlatform::MacOs);

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
        let excludes = default_excludes_for(Path::new("/home/tester"), HostPlatform::Linux);

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
            default_excludes_for(Path::new(""), HostPlatform::MacOs),
            strings(&[".git", "node_modules"])
        );
        assert_eq!(
            default_excludes_for(Path::new("relative-home"), HostPlatform::Linux),
            strings(&[".git", "node_modules"])
        );
    }
}
