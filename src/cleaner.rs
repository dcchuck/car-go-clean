use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const MAX_STDERR_EXCERPT: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanOutcome {
    pub exit_code: i32,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanResult {
    pub path: PathBuf,
    pub bytes_before: i64,
    pub bytes_after: i64,
    pub duration: Duration,
    pub exit_code: i32,
    pub stderr_excerpt: String,
    pub skipped: bool,
}

pub trait CommandRunner: Clone {
    fn run(&self, dir: &Path, cmd: &mut Command) -> Result<CleanOutcome>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, _dir: &Path, cmd: &mut Command) -> Result<CleanOutcome> {
        let output = cmd.output().context("run cargo clean")?;
        Ok(CleanOutcome {
            exit_code: output.status.code().unwrap_or(1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Cleaner<R: CommandRunner = RealRunner> {
    cargo_bin: PathBuf,
    runner: R,
    _timeout: Duration,
}

impl<R: CommandRunner> Cleaner<R> {
    pub fn new(cargo_bin: impl Into<PathBuf>, runner: R, timeout: Duration) -> Self {
        Self {
            cargo_bin: cargo_bin.into(),
            runner,
            _timeout: timeout,
        }
    }

    pub fn clean(&self, project_dir: impl AsRef<Path>) -> Result<CleanResult> {
        let project_dir = project_dir.as_ref();
        let target_dir = project_dir.join("target");
        let mut result = CleanResult {
            path: project_dir.to_path_buf(),
            bytes_before: 0,
            bytes_after: 0,
            duration: Duration::ZERO,
            exit_code: 0,
            stderr_excerpt: String::new(),
            skipped: false,
        };

        if !is_direct_directory(&target_dir) {
            result.skipped = true;
            return Ok(result);
        }

        result.bytes_before = dir_size(&target_dir)?;
        let start = Instant::now();
        let mut cmd = Command::new(&self.cargo_bin);
        cmd.arg("clean").current_dir(project_dir);
        let outcome = self.runner.run(project_dir, &mut cmd)?;
        result.duration = start.elapsed();
        result.exit_code = outcome.exit_code;
        result.stderr_excerpt = stderr_excerpt(&outcome.stderr);
        result.bytes_after = if target_dir.exists() {
            dir_size(&target_dir)?
        } else {
            0
        };
        Ok(result)
    }
}

impl Default for Cleaner<RealRunner> {
    fn default() -> Self {
        Self::new("cargo", RealRunner, Duration::from_secs(10 * 60))
    }
}

pub fn resolve_cargo_bin(candidates: &[PathBuf]) -> Result<PathBuf> {
    for candidate in candidates {
        if is_executable(candidate) {
            return Ok(candidate.clone());
        }
    }
    if let Some(path) = find_on_path("cargo") {
        return Ok(path);
    }
    anyhow::bail!("cargo not found in candidates or PATH")
}

pub fn default_cargo_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join(".cargo/bin/cargo"));
    }
    out.push(PathBuf::from("/opt/homebrew/bin/cargo"));
    out.push(PathBuf::from("/usr/local/bin/cargo"));
    out
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn is_direct_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

fn dir_size(root: &Path) -> Result<i64> {
    if !root.exists() {
        return Ok(0);
    }
    let mut total = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            total += dir_size(&path)?;
        } else if meta.is_file() {
            total += meta.len() as i64;
        }
    }
    Ok(total)
}

fn stderr_excerpt(stderr: &str) -> String {
    if stderr.len() <= MAX_STDERR_EXCERPT {
        return stderr.to_string();
    }
    stderr[stderr.len() - MAX_STDERR_EXCERPT..].to_string()
}
