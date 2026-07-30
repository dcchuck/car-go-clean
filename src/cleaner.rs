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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanAttemptOutcome {
    Success,
    CargoNonzero,
    RunnerFailure,
}

impl CleanAttemptOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::CargoNonzero => "cargo_nonzero",
            Self::RunnerFailure => "runner_failure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanResult {
    pub path: PathBuf,
    pub bytes_before: i64,
    pub bytes_after: i64,
    pub duration: Duration,
    pub exit_code: Option<i32>,
    pub stderr_excerpt: String,
    pub outcome: Option<CleanAttemptOutcome>,
    pub attempt_error: Option<String>,
    pub measurement_error: Option<String>,
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
        let exit_code = output
            .status
            .code()
            .context("cargo clean terminated without an exit code")?;
        Ok(CleanOutcome {
            exit_code,
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
        self.clean_with_attempt_reporter(project_dir, |_, _| {})
    }

    pub fn clean_with_attempt_reporter(
        &self,
        project_dir: impl AsRef<Path>,
        report_attempt: impl FnOnce(&Path, &Path),
    ) -> Result<CleanResult> {
        self.clean_with_attempt_reporter_and_pre_spawn_validator(
            project_dir,
            report_attempt,
            |_, _| Ok(true),
        )
    }

    pub fn clean_with_attempt_reporter_and_pre_spawn_validator(
        &self,
        project_dir: impl AsRef<Path>,
        report_attempt: impl FnOnce(&Path, &Path),
        validate_before_spawn: impl FnOnce(&Path, &Path) -> Result<bool>,
    ) -> Result<CleanResult> {
        let project_dir = project_dir.as_ref();
        let target_dir = project_dir.join("target");
        let mut result = CleanResult {
            path: project_dir.to_path_buf(),
            bytes_before: 0,
            bytes_after: 0,
            duration: Duration::ZERO,
            exit_code: None,
            stderr_excerpt: String::new(),
            outcome: None,
            attempt_error: None,
            measurement_error: None,
            skipped: false,
        };

        if !is_direct_directory(&target_dir) {
            result.skipped = true;
            return Ok(result);
        }

        result.bytes_before = dir_size(&target_dir)?;
        result.bytes_after = result.bytes_before;
        let start = Instant::now();
        let mut cmd = Command::new(&self.cargo_bin);
        cmd.arg("clean")
            .arg("--target-dir")
            .arg(&target_dir)
            .env_remove("CARGO_TARGET_DIR")
            .current_dir(project_dir);
        report_attempt(project_dir, &target_dir);
        if !validate_before_spawn(project_dir, &target_dir)? {
            result.duration = start.elapsed();
            result.skipped = true;
            return Ok(result);
        }
        let outcome = match self.runner.run(project_dir, &mut cmd) {
            Ok(outcome) => outcome,
            Err(error) => {
                result.duration = start.elapsed();
                result.outcome = Some(CleanAttemptOutcome::RunnerFailure);
                result.attempt_error = Some(error.to_string());
                match dir_size(&target_dir) {
                    Ok(bytes_after) => result.bytes_after = bytes_after,
                    Err(error) => {
                        result.measurement_error =
                            Some(format!("measure target after cargo clean: {error:#}"));
                    }
                }
                return Ok(result);
            }
        };
        result.duration = start.elapsed();
        result.exit_code = Some(outcome.exit_code);
        result.stderr_excerpt = stderr_excerpt(&outcome.stderr);
        result.outcome = Some(if outcome.exit_code == 0 {
            CleanAttemptOutcome::Success
        } else {
            CleanAttemptOutcome::CargoNonzero
        });
        match dir_size(&target_dir) {
            Ok(bytes_after) => result.bytes_after = bytes_after,
            Err(error) => {
                result.measurement_error =
                    Some(format!("measure target after cargo clean: {error:#}"));
            }
        }
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

    let mut start = stderr.len() - MAX_STDERR_EXCERPT;
    while !stderr.is_char_boundary(start) {
        start += 1;
    }
    stderr[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_excerpt_starts_on_a_utf8_boundary() {
        let stderr = format!("prefix:{}", "€".repeat(2_000));

        let excerpt = stderr_excerpt(&stderr);

        assert!(excerpt.len() <= MAX_STDERR_EXCERPT);
        assert!(stderr.ends_with(&excerpt));
        assert!(excerpt.chars().all(|character| character == '€'));
    }

    #[test]
    fn stderr_excerpt_preserves_short_input() {
        assert_eq!(stderr_excerpt("cargo failed: λ"), "cargo failed: λ");
    }

    #[cfg(unix)]
    #[test]
    fn signal_terminated_process_preserves_runner_failure_and_remeasures_bytes() {
        use std::os::unix::fs::PermissionsExt;

        let project = tempfile::tempdir().unwrap();
        fs::create_dir_all(project.path().join("target")).unwrap();
        fs::write(project.path().join("target/removed.bin"), [0; 2_048]).unwrap();
        fs::write(project.path().join("target/retained.bin"), [0; 1_024]).unwrap();
        let cargo = project.path().join("signal-cargo");
        fs::write(
            &cargo,
            "#!/bin/sh\nrm \"$PWD/target/removed.bin\"\nkill -KILL $$\n",
        )
        .unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();

        let result = Cleaner::new(cargo, RealRunner, Duration::from_secs(60))
            .clean(project.path())
            .unwrap();

        assert_eq!(result.outcome, Some(CleanAttemptOutcome::RunnerFailure));
        assert_eq!(result.exit_code, None);
        assert!(result
            .attempt_error
            .as_deref()
            .unwrap()
            .contains("cargo clean terminated without an exit code"));
        assert_eq!(result.bytes_before, 3_072);
        assert_eq!(result.bytes_after, 1_024);
        assert!(result.measurement_error.is_none());
    }
}
