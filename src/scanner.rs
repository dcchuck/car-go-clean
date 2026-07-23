use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct ScannerOptions {
    pub roots: Vec<PathBuf>,
    pub project_dirs: Vec<PathBuf>,
    pub excludes: Vec<String>,
}

#[derive(Clone)]
pub struct Scanner {
    opts: ScannerOptions,
    worktree_resolver: Arc<dyn GitWorktreeResolver>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub projects: Vec<PathBuf>,
    pub errors: Vec<ScanError>,
    pub worktree_discoveries: Vec<WorktreeDiscovery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeDiscovery {
    Success {
        primary: PathBuf,
        linked: Vec<PathBuf>,
    },
    Failure {
        primary: PathBuf,
        message: String,
    },
}

pub trait GitWorktreeResolver {
    fn linked_worktrees(&self, primary: &Path) -> Result<Vec<PathBuf>, GitWorktreeError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGitWorktreeResolver;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeError {
    message: String,
}

impl GitWorktreeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for GitWorktreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitWorktreeError {}

impl GitWorktreeResolver for SystemGitWorktreeResolver {
    fn linked_worktrees(&self, primary: &Path) -> Result<Vec<PathBuf>, GitWorktreeError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(primary)
            .args(["worktree", "list", "--porcelain", "-z"])
            .output()
            .map_err(|err| GitWorktreeError::new(format!("failed to run git: {err}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            let message = if stderr.is_empty() {
                format!("git worktree list failed with status {}", output.status)
            } else {
                format!("git worktree list failed: {stderr}")
            };
            return Err(GitWorktreeError::new(message));
        }

        output
            .stdout
            .split(|byte| *byte == 0)
            .filter_map(|record| record.strip_prefix(b"worktree "))
            .map(git_path_from_bytes)
            .collect()
    }
}

impl fmt::Debug for Scanner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Scanner")
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl Scanner {
    pub fn new(opts: ScannerOptions) -> Self {
        Self::with_worktree_resolver(opts, Arc::new(SystemGitWorktreeResolver))
    }

    pub fn with_worktree_resolver(
        opts: ScannerOptions,
        resolver: Arc<dyn GitWorktreeResolver>,
    ) -> Self {
        Self {
            opts,
            worktree_resolver: resolver,
        }
    }

    pub fn scan(&self) -> Result<Vec<PathBuf>> {
        Ok(self.scan_with_errors()?.projects)
    }

    pub fn scan_with_errors(&self) -> Result<ScanReport> {
        let mut found = BTreeSet::new();
        let mut errors = Vec::new();
        let mut worktree_discoveries = Vec::new();
        let canonical_roots: Vec<_> = self
            .opts
            .roots
            .iter()
            .filter_map(|root| fs::canonicalize(root).ok())
            .collect();
        for root in &self.opts.roots {
            self.walk(
                root,
                &[],
                &canonical_roots,
                &mut found,
                &mut worktree_discoveries,
                &mut errors,
            )?;
        }
        for project in &self.opts.project_dirs {
            if has_cargo_toml(project) {
                found.insert(project.clone());
            }
        }
        Ok(ScanReport {
            projects: found.into_iter().collect(),
            errors,
            worktree_discoveries,
        })
    }

    fn walk(
        &self,
        dir: &Path,
        parent_ignores: &[Arc<Gitignore>],
        canonical_roots: &[PathBuf],
        found: &mut BTreeSet<PathBuf>,
        outcomes: &mut Vec<WorktreeDiscovery>,
        errors: &mut Vec<ScanError>,
    ) -> Result<()> {
        let meta = match fs::metadata(dir) {
            Ok(meta) => meta,
            Err(err) => {
                errors.push(scan_error(dir, err));
                return Ok(());
            }
        };
        if !meta.is_dir() || self.should_skip(dir) || is_ignored(parent_ignores, dir, true) {
            return Ok(());
        }
        if has_cargo_toml(dir) {
            if dir.join(".git").is_dir() {
                let primary = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
                found.insert(primary.clone());
                self.discover_linked_worktrees(&primary, canonical_roots, found, outcomes, errors);
            } else {
                found.insert(dir.to_path_buf());
            }
            return Ok(());
        }
        let ignores = ignores_for(dir, parent_ignores);
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                errors.push(scan_error(dir, err));
                return Ok(());
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    errors.push(scan_error(dir, err));
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    errors.push(scan_error(entry.path(), err));
                    continue;
                }
            };
            if file_type.is_dir() && !file_type.is_symlink() {
                self.walk(
                    &entry.path(),
                    &ignores,
                    canonical_roots,
                    found,
                    outcomes,
                    errors,
                )?;
            }
        }
        Ok(())
    }

    fn discover_linked_worktrees(
        &self,
        primary: &Path,
        canonical_roots: &[PathBuf],
        found: &mut BTreeSet<PathBuf>,
        outcomes: &mut Vec<WorktreeDiscovery>,
        errors: &mut Vec<ScanError>,
    ) {
        if !primary.join(".git").is_dir() {
            return;
        }

        match self.worktree_resolver.linked_worktrees(primary) {
            Ok(candidates) => {
                let mut linked = BTreeSet::new();
                for candidate in candidates {
                    let Ok(candidate) = fs::canonicalize(candidate) else {
                        continue;
                    };
                    if candidate == primary
                        || !canonical_roots
                            .iter()
                            .any(|root| candidate.starts_with(root))
                        || self.should_skip(&candidate)
                        || !has_cargo_toml(&candidate)
                    {
                        continue;
                    }
                    found.insert(candidate.clone());
                    linked.insert(candidate);
                }
                outcomes.push(WorktreeDiscovery::Success {
                    primary: primary.to_path_buf(),
                    linked: linked.into_iter().collect(),
                });
            }
            Err(err) => {
                let message = err.to_string();
                outcomes.push(WorktreeDiscovery::Failure {
                    primary: primary.to_path_buf(),
                    message: message.clone(),
                });
                errors.push(ScanError {
                    path: primary.to_path_buf(),
                    message,
                });
            }
        }
    }

    fn should_skip(&self, path: &Path) -> bool {
        let base = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if base == "target" {
            return true;
        }
        self.opts.excludes.iter().any(|exclude| {
            !exclude.is_empty()
                && (base == exclude
                    || path_ends_with(path, exclude)
                    || path
                        .components()
                        .any(|component| component.as_os_str() == exclude.as_str()))
        })
    }
}

fn git_path_from_bytes(record: &[u8]) -> Result<PathBuf, GitWorktreeError> {
    if record.is_empty() {
        return Err(GitWorktreeError::new(
            "git worktree list returned an empty worktree path",
        ));
    }

    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        Ok(PathBuf::from(OsString::from_vec(record.to_vec())))
    }

    #[cfg(not(unix))]
    {
        let path = String::from_utf8(record.to_vec()).map_err(|_| {
            GitWorktreeError::new("git worktree list returned a non-UTF-8 worktree path")
        })?;
        Ok(PathBuf::from(path))
    }
}

fn path_ends_with(path: &Path, exclude: &str) -> bool {
    let exclude = Path::new(exclude);
    let exclude_parts: Vec<_> = exclude.components().collect();
    if exclude_parts.is_empty() {
        return false;
    }
    let path_parts: Vec<_> = path.components().collect();
    path_parts.ends_with(&exclude_parts)
}

fn has_cargo_toml(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file()
}

fn scan_error(path: impl AsRef<Path>, err: std::io::Error) -> ScanError {
    ScanError {
        path: path.as_ref().to_path_buf(),
        message: err.to_string(),
    }
}

fn ignores_for(dir: &Path, parent_ignores: &[Arc<Gitignore>]) -> Vec<Arc<Gitignore>> {
    let mut ignores = parent_ignores.to_vec();
    let gitignore = dir.join(".gitignore");
    if gitignore.is_file() {
        let mut builder = GitignoreBuilder::new(dir);
        let _ = builder.add(&gitignore);
        if let Ok(matcher) = builder.build() {
            ignores.push(Arc::new(matcher));
        }
    }
    ignores
}

fn is_ignored(ignores: &[Arc<Gitignore>], path: &Path, is_dir: bool) -> bool {
    ignores
        .iter()
        .any(|ignore| ignore.matched_path_or_any_parents(path, is_dir).is_ignore())
}
