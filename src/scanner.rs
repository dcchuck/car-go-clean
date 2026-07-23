use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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

        self.worktree_paths_from_output(primary, &output)
    }
}

impl SystemGitWorktreeResolver {
    fn worktree_paths_from_output(
        &self,
        primary: &Path,
        output: &Output,
    ) -> Result<Vec<PathBuf>, GitWorktreeError> {
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

        parse_git_worktree_porcelain(primary, &output.stdout)
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
        let mut canonical_roots: Vec<_> = self
            .opts
            .roots
            .iter()
            .filter_map(|root| fs::canonicalize(root).ok())
            .collect();
        canonical_roots.extend(
            self.opts
                .project_dirs
                .iter()
                .filter_map(|project| fs::canonicalize(project).ok()),
        );
        canonical_roots.sort();
        canonical_roots.dedup();
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
            self.add_configured_project(
                project,
                &canonical_roots,
                &mut found,
                &mut worktree_discoveries,
                &mut errors,
            );
        }
        Ok(ScanReport {
            projects: found.into_iter().collect(),
            errors,
            worktree_discoveries,
        })
    }

    fn add_configured_project(
        &self,
        project: &Path,
        canonical_roots: &[PathBuf],
        found: &mut BTreeSet<PathBuf>,
        outcomes: &mut Vec<WorktreeDiscovery>,
        errors: &mut Vec<ScanError>,
    ) {
        if !has_cargo_toml(project) {
            return;
        }
        let project = match fs::canonicalize(project) {
            Ok(project) => project,
            Err(err) => {
                errors.push(scan_error(project, err));
                return;
            }
        };
        if project.to_str().is_none() {
            errors.push(non_utf8_scan_error(&project));
            if project.join(".git").is_dir() {
                record_discovery_failure(&project, non_utf8_discovery_message(), outcomes, errors);
            }
            return;
        }
        found.insert(project.clone());
        if project.join(".git").is_dir() {
            self.discover_linked_worktrees(&project, canonical_roots, found, outcomes, errors);
        }
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
                let primary = match fs::canonicalize(dir) {
                    Ok(primary) => primary,
                    Err(err) => {
                        errors.push(scan_error(dir, err));
                        return Ok(());
                    }
                };
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
                if primary.to_str().is_none()
                    || candidates
                        .iter()
                        .any(|candidate| candidate.to_str().is_none())
                {
                    record_discovery_failure(
                        primary,
                        non_utf8_discovery_message(),
                        outcomes,
                        errors,
                    );
                    return;
                }
                let mut linked = BTreeSet::new();
                for candidate in candidates {
                    let Ok(candidate) = fs::canonicalize(candidate) else {
                        continue;
                    };
                    if candidate.to_str().is_none() {
                        record_discovery_failure(
                            primary,
                            non_utf8_discovery_message(),
                            outcomes,
                            errors,
                        );
                        return;
                    }
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
                record_discovery_failure(primary, &err.to_string(), outcomes, errors);
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

fn parse_git_worktree_porcelain(
    primary: &Path,
    output: &[u8],
) -> Result<Vec<PathBuf>, GitWorktreeError> {
    if output.is_empty() || !output.ends_with(&[0]) {
        return Err(GitWorktreeError::new(
            "git worktree list returned truncated porcelain output",
        ));
    }

    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();
    let mut in_record = false;
    let mut primary_records = 0;
    for field in output[..output.len() - 1].split(|byte| *byte == 0) {
        if field.is_empty() {
            if !in_record {
                return Err(GitWorktreeError::new(
                    "git worktree list returned an empty or malformed record",
                ));
            }
            in_record = false;
            continue;
        }

        if !in_record {
            let Some(path) = field.strip_prefix(b"worktree ") else {
                return Err(GitWorktreeError::new(
                    "git worktree list record does not start with a worktree path",
                ));
            };
            let path = git_path_from_bytes(path)?;
            if !seen.insert(path.clone()) {
                return Err(GitWorktreeError::new(
                    "git worktree list returned a duplicate worktree path",
                ));
            }
            if path == primary {
                primary_records += 1;
            } else {
                paths.push(path);
            }
            in_record = true;
        } else if field == b"worktree" || field.starts_with(b"worktree ") {
            return Err(GitWorktreeError::new(
                "git worktree list returned multiple worktree paths in one record",
            ));
        }
    }

    if in_record {
        return Err(GitWorktreeError::new(
            "git worktree list returned a record without a terminating separator",
        ));
    }
    if seen.is_empty() {
        return Err(GitWorktreeError::new(
            "git worktree list returned no worktree records",
        ));
    }
    if primary_records != 1 {
        return Err(GitWorktreeError::new(
            "git worktree list did not include exactly one queried primary checkout",
        ));
    }
    Ok(paths)
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

fn record_discovery_failure(
    primary: &Path,
    message: &str,
    outcomes: &mut Vec<WorktreeDiscovery>,
    errors: &mut Vec<ScanError>,
) {
    outcomes.push(WorktreeDiscovery::Failure {
        primary: primary.to_path_buf(),
        message: message.to_string(),
    });
    errors.push(ScanError {
        path: primary.to_path_buf(),
        message: message.to_string(),
    });
}

fn non_utf8_discovery_message() -> &'static str {
    "git worktree discovery returned a non-UTF-8 path that cannot be persisted safely"
}

fn non_utf8_scan_error(path: &Path) -> ScanError {
    ScanError {
        path: path.to_path_buf(),
        message: "project path is non-UTF-8 and cannot be persisted safely".to_string(),
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

#[cfg(test)]
mod worktree_porcelain_tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(unix)]
    use std::process::{ExitStatus, Output};

    #[test]
    fn parser_accepts_complete_records_with_whitespace_and_newlines() {
        let primary = Path::new("/workspace/main checkout");
        let output = b"worktree /workspace/main checkout\0HEAD abc\0branch refs/heads/main\0\0worktree /workspace/feature\ncheckout\0HEAD def\0detached\0\0";

        assert_eq!(
            parse_git_worktree_porcelain(primary, output).unwrap(),
            vec![PathBuf::from("/workspace/feature\ncheckout")]
        );
    }

    #[test]
    fn parser_rejects_truncated_or_structurally_invalid_output() {
        let primary = Path::new("/workspace/main");
        for output in [
            b"".as_slice(),
            b"worktree /workspace/main",
            b"worktree /workspace/main\0HEAD abc\0",
            b"HEAD abc\0\0",
            b"worktree \0\0",
            b"worktree /workspace/main\0worktree /workspace/linked\0\0",
            b"worktree /workspace/other\0HEAD abc\0\0",
            b"worktree /workspace/main\0HEAD abc\0\0worktree /workspace/main\0HEAD def\0\0",
        ] {
            assert!(
                parse_git_worktree_porcelain(primary, output).is_err(),
                "unexpectedly accepted {output:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn system_resolver_output_boundary_rejects_malformed_success_output() {
        let output = Output {
            status: ExitStatus::from_raw(0),
            stdout: b"worktree /workspace/main".to_vec(),
            stderr: Vec::new(),
        };

        assert!(SystemGitWorktreeResolver
            .worktree_paths_from_output(Path::new("/workspace/main"), &output)
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn system_resolver_output_boundary_returns_only_linked_paths_and_rejects_failure_status() {
        let resolver = SystemGitWorktreeResolver;
        let primary = Path::new("/workspace/main");
        let success = Output {
            status: ExitStatus::from_raw(0),
            stdout:
                b"worktree /workspace/main\0HEAD abc\0\0worktree /workspace/feature\0HEAD def\0\0"
                    .to_vec(),
            stderr: Vec::new(),
        };
        let failure = Output {
            status: ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: b"fatal: broken".to_vec(),
        };

        assert_eq!(
            resolver
                .worktree_paths_from_output(primary, &success)
                .unwrap(),
            vec![PathBuf::from("/workspace/feature")]
        );
        assert!(resolver
            .worktree_paths_from_output(primary, &failure)
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn parser_preserves_non_utf8_paths_at_the_git_boundary() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let primary = Path::new("/workspace/main");
        let output =
            b"worktree /workspace/main\0HEAD abc\0\0worktree /workspace/\xff\0HEAD def\0\0";
        let parsed = parse_git_worktree_porcelain(primary, output).unwrap();

        assert_eq!(
            parsed[0],
            PathBuf::from(OsString::from_vec(b"/workspace/\xff".to_vec()))
        );
    }
}
