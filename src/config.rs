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
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for Config {
    fn default() -> Self {
        let scan_dirs = env::var_os("HOME").map(PathBuf::from).into_iter().collect();
        Self {
            scan_dirs,
            project_dirs: Vec::new(),
            excludes: default_excludes(),
            clean_interval: default_clean_interval(),
            scan_interval: default_scan_interval(),
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
    home_dir().join(".config/car-go-clean/config.toml")
}

pub fn paths() -> PathSet {
    let state_dir = if let Some(xdg) = env::var_os("XDG_STATE_HOME") {
        PathBuf::from(xdg).join("car-go-clean")
    } else {
        home_dir().join(".local/state/car-go-clean")
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
        return home_dir();
    }
    if let Some(rest) = expanded_env.strip_prefix("~/") {
        return home_dir().join(rest);
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

fn home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn default_clean_interval() -> Duration {
    Duration::from_secs(24 * 60 * 60)
}

fn default_scan_interval() -> Duration {
    Duration::from_secs(7 * 24 * 60 * 60)
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_excludes() -> Vec<String> {
    [
        ".git",
        "node_modules",
        ".cargo",
        ".rustup",
        "target",
        "Library/Caches",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}
