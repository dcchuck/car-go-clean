use crate::identity::{
    BootSessionId, FilesystemIdentity, IdentityProvider, SystemIdentityProvider,
};
use crate::policy::ScopePolicy;
use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsString;
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
    exclusion_matchers: ExclusionMatchers,
    worktree_resolver: Arc<dyn GitWorktreeResolver>,
    policy: Option<ScopePolicy>,
    policy_hash: String,
    identity_provider: Arc<dyn IdentityProvider>,
}

#[derive(Debug, Clone, Default)]
struct ExclusionMatchers {
    absolute: Vec<AbsoluteExclusion>,
    relative: Vec<Vec<OsString>>,
}

#[derive(Debug, Clone)]
struct AbsoluteExclusion {
    lexical: PathBuf,
    canonical: Option<PathBuf>,
}

#[derive(Clone, Copy)]
struct CargoProjectOptions {
    honor_excludes: bool,
    report_canonicalization_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub projects: Vec<PathBuf>,
    pub errors: Vec<ScanError>,
    pub worktree_discoveries: Vec<WorktreeDiscovery>,
    pub origins: Vec<DiscoveryOriginResult>,
    pub policy_hash: String,
    pub boot_session_id: Option<BootSessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryOriginKind {
    ScanRoot,
    ExplicitProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveryOriginResult {
    pub kind: DiscoveryOriginKind,
    pub configured_path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub completed: bool,
    pub error: Option<String>,
    pub projects: Vec<ObservedProject>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservedProject {
    pub path: PathBuf,
    pub project_identity: FilesystemIdentity,
    pub target_identity: Option<FilesystemIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
    pub kind: ScanErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanErrorKind {
    Scan,
    WorktreeDiscovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeDiscovery {
    Success {
        primary: PathBuf,
        linked: Vec<PathBuf>,
        excluded: Vec<PathBuf>,
        out_of_scope: Vec<PathBuf>,
    },
    Failure {
        primary: PathBuf,
        message: String,
    },
}

struct OriginScan {
    result: DiscoveryOriginResult,
    projects: Vec<ObservedProject>,
    errors: Vec<ScanError>,
    worktree_discoveries: Vec<WorktreeDiscovery>,
}

impl OriginScan {
    fn completed_empty(kind: DiscoveryOriginKind, configured_path: &Path) -> Self {
        Self::completed_empty_with_optional_canonical(kind, configured_path, None)
    }

    fn completed_empty_with_canonical(
        kind: DiscoveryOriginKind,
        configured_path: &Path,
        canonical_path: PathBuf,
    ) -> Self {
        Self::completed_empty_with_optional_canonical(kind, configured_path, Some(canonical_path))
    }

    fn completed_empty_with_optional_canonical(
        kind: DiscoveryOriginKind,
        configured_path: &Path,
        canonical_path: Option<PathBuf>,
    ) -> Self {
        Self {
            result: DiscoveryOriginResult {
                kind,
                configured_path: configured_path.to_path_buf(),
                canonical_path,
                completed: true,
                error: None,
                projects: Vec::new(),
            },
            projects: Vec::new(),
            errors: Vec::new(),
            worktree_discoveries: Vec::new(),
        }
    }

    fn failed(
        kind: DiscoveryOriginKind,
        configured_path: &Path,
        canonical_path: Option<PathBuf>,
        error: ScanError,
    ) -> Self {
        Self {
            result: DiscoveryOriginResult {
                kind,
                configured_path: configured_path.to_path_buf(),
                canonical_path,
                completed: false,
                error: Some(error.message.clone()),
                projects: Vec::new(),
            },
            projects: Vec::new(),
            errors: vec![error],
            worktree_discoveries: Vec::new(),
        }
    }
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
    #[doc(hidden)]
    pub fn worktree_paths_from_output(
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
        let policy_hash = scanner_scope_hash(&opts);
        let exclusion_matchers = ExclusionMatchers::new(
            std::iter::once("target").chain(opts.excludes.iter().map(String::as_str)),
        );
        Self {
            opts,
            exclusion_matchers,
            worktree_resolver: resolver,
            policy: None,
            policy_hash,
            identity_provider: Arc::new(SystemIdentityProvider),
        }
    }

    pub fn with_authority(
        mut self,
        policy: ScopePolicy,
        identity_provider: Arc<dyn IdentityProvider>,
    ) -> Self {
        self.policy_hash = policy.hash().to_string();
        self.policy = Some(policy);
        self.identity_provider = identity_provider;
        self
    }

    pub(crate) fn policy_hash(&self) -> &str {
        &self.policy_hash
    }

    pub fn scan(&self) -> Result<Vec<PathBuf>> {
        Ok(self.scan_with_errors()?.projects)
    }

    pub fn scan_with_errors(&self) -> Result<ScanReport> {
        let boot_session_id = self.identity_provider.boot_session()?;
        let mut all_projects = BTreeSet::new();
        let mut errors = Vec::new();
        let mut worktree_discoveries = Vec::new();
        let canonical_roots = self.canonical_authority_roots();
        let mut origins = Vec::with_capacity(self.opts.roots.len() + self.opts.project_dirs.len());

        for root in &self.opts.roots {
            let origin = self.scan_origin(DiscoveryOriginKind::ScanRoot, root, &canonical_roots)?;
            all_projects.extend(origin.projects.iter().map(|project| project.path.clone()));
            errors.extend(origin_errors(&origin));
            worktree_discoveries.extend(origin.worktree_discoveries);
            origins.push(origin.result);
        }
        for project in &self.opts.project_dirs {
            let origin = self.scan_origin(
                DiscoveryOriginKind::ExplicitProject,
                project,
                &canonical_roots,
            )?;
            all_projects.extend(origin.projects.iter().map(|project| project.path.clone()));
            errors.extend(origin_errors(&origin));
            worktree_discoveries.extend(origin.worktree_discoveries);
            origins.push(origin.result);
        }

        Ok(ScanReport {
            projects: all_projects.into_iter().collect(),
            errors,
            worktree_discoveries,
            origins,
            policy_hash: self.policy_hash.clone(),
            boot_session_id,
        })
    }

    fn scan_origin(
        &self,
        kind: DiscoveryOriginKind,
        configured_path: &Path,
        canonical_roots: &[PathBuf],
    ) -> Result<OriginScan> {
        if self.should_skip(configured_path) {
            return Ok(OriginScan::completed_empty(kind, configured_path));
        }

        let canonical_path = match fs::canonicalize(configured_path) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(OriginScan::completed_empty(kind, configured_path));
            }
            Err(error) => {
                return Ok(OriginScan::failed(
                    kind,
                    configured_path,
                    None,
                    scan_error(configured_path, error),
                ));
            }
        };
        if self.should_skip(&canonical_path) {
            return Ok(OriginScan::completed_empty_with_canonical(
                kind,
                configured_path,
                canonical_path,
            ));
        }
        if !self.policy_allows_origin(kind, &canonical_path) {
            return Ok(OriginScan::failed(
                kind,
                configured_path,
                Some(canonical_path.clone()),
                ScanError {
                    path: canonical_path,
                    message: format!(
                        "configured {} changed after policy construction",
                        origin_kind_label(kind)
                    ),
                    kind: ScanErrorKind::Scan,
                },
            ));
        }

        let mut found = BTreeSet::new();
        let mut worktree_discoveries = Vec::new();
        let mut local_errors = Vec::new();
        match kind {
            DiscoveryOriginKind::ScanRoot => self.walk(
                &canonical_path,
                &[],
                canonical_roots,
                &mut found,
                &mut worktree_discoveries,
                &mut local_errors,
            )?,
            DiscoveryOriginKind::ExplicitProject => self.add_cargo_project(
                &canonical_path,
                canonical_roots,
                CargoProjectOptions {
                    honor_excludes: true,
                    report_canonicalization_error: true,
                },
                &mut found,
                &mut worktree_discoveries,
                &mut local_errors,
            ),
        }

        let mut projects = Vec::with_capacity(found.len());
        for path in found {
            match self.observe_project(&path) {
                Ok(project) => projects.push(project),
                Err(error) => local_errors.push(ScanError {
                    path,
                    message: error.to_string(),
                    kind: ScanErrorKind::Scan,
                }),
            }
        }
        let completed = local_errors.is_empty();
        let error = joined_error_messages(&local_errors);
        Ok(OriginScan {
            result: DiscoveryOriginResult {
                kind,
                configured_path: configured_path.to_path_buf(),
                canonical_path: Some(canonical_path),
                completed,
                error,
                projects: projects.clone(),
            },
            projects,
            errors: local_errors,
            worktree_discoveries,
        })
    }

    fn observe_project(&self, path: &Path) -> Result<ObservedProject> {
        let project_identity = self.identity_provider.identity(path)?;
        let target_path = path.join("target");
        let target_identity = match fs::symlink_metadata(&target_path) {
            Ok(_) => self.identity_provider.identity(&target_path).ok(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => None,
        };
        Ok(ObservedProject {
            path: path.to_path_buf(),
            project_identity,
            target_identity,
        })
    }

    fn canonical_authority_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = if let Some(policy) = &self.policy {
            let diagnostics = policy.diagnostics();
            diagnostics
                .canonical_scan_roots
                .iter()
                .chain(diagnostics.canonical_project_paths)
                .cloned()
                .collect()
        } else {
            self.opts
                .roots
                .iter()
                .chain(&self.opts.project_dirs)
                .filter(|path| !self.should_skip(path))
                .filter_map(|path| fs::canonicalize(path).ok())
                .filter(|path| !self.should_skip(path))
                .collect()
        };
        roots.sort();
        roots.dedup();
        roots
    }

    fn policy_allows_origin(&self, kind: DiscoveryOriginKind, canonical_path: &Path) -> bool {
        let Some(policy) = &self.policy else {
            return true;
        };
        let diagnostics = policy.diagnostics();
        match kind {
            DiscoveryOriginKind::ScanRoot => diagnostics
                .canonical_scan_roots
                .iter()
                .any(|path| path == canonical_path),
            DiscoveryOriginKind::ExplicitProject => diagnostics
                .canonical_project_paths
                .iter()
                .any(|path| path == canonical_path),
        }
    }

    fn add_cargo_project(
        &self,
        project: &Path,
        canonical_roots: &[PathBuf],
        options: CargoProjectOptions,
        found: &mut BTreeSet<PathBuf>,
        outcomes: &mut Vec<WorktreeDiscovery>,
        errors: &mut Vec<ScanError>,
    ) {
        if options.honor_excludes && self.should_skip(project) {
            return;
        }
        let project = match fs::canonicalize(project) {
            Ok(project) => project,
            Err(err) => {
                if options.report_canonicalization_error {
                    errors.push(scan_error(project, err));
                }
                return;
            }
        };
        if options.honor_excludes && self.should_skip(&project) {
            return;
        }
        if project.to_str().is_none() {
            if project.join(".git").is_dir() {
                record_discovery_failure(&project, non_utf8_discovery_message(), outcomes, errors);
            } else {
                errors.push(non_utf8_scan_error(&project));
            }
            return;
        }
        if !has_cargo_toml(&project) {
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
        if self.should_skip(dir) {
            return Ok(());
        }
        let meta = match fs::metadata(dir) {
            Ok(meta) => meta,
            Err(err) => {
                errors.push(scan_error(dir, err));
                return Ok(());
            }
        };
        if !meta.is_dir() || is_ignored(parent_ignores, dir, true) {
            return Ok(());
        }
        if has_cargo_toml(dir) {
            self.add_cargo_project(
                dir,
                canonical_roots,
                CargoProjectOptions {
                    honor_excludes: true,
                    report_canonicalization_error: true,
                },
                found,
                outcomes,
                errors,
            );
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
            let path = entry.path();
            if self.should_skip(&path) {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    errors.push(scan_error(path, err));
                    continue;
                }
            };
            if file_type.is_dir() && !file_type.is_symlink() {
                self.walk(&path, &ignores, canonical_roots, found, outcomes, errors)?;
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
                if primary.to_str().is_none() {
                    record_discovery_failure(
                        primary,
                        non_utf8_discovery_message(),
                        outcomes,
                        errors,
                    );
                    return;
                }
                let mut linked = BTreeSet::new();
                let mut excluded = BTreeSet::new();
                let mut out_of_scope = BTreeSet::new();
                for candidate in candidates {
                    if self.should_skip(&candidate) {
                        excluded.insert(candidate);
                        continue;
                    }
                    let candidate = match fs::canonicalize(&candidate) {
                        Ok(candidate) => candidate,
                        Err(_) if candidate.to_str().is_none() => {
                            record_discovery_failure(
                                primary,
                                non_utf8_discovery_message(),
                                outcomes,
                                errors,
                            );
                            return;
                        }
                        Err(_) => continue,
                    };
                    if self.should_skip(&candidate) {
                        excluded.insert(candidate);
                        continue;
                    }
                    if candidate.to_str().is_none() {
                        record_discovery_failure(
                            primary,
                            non_utf8_discovery_message(),
                            outcomes,
                            errors,
                        );
                        return;
                    }
                    if candidate == primary {
                        continue;
                    }
                    if !canonical_roots
                        .iter()
                        .any(|root| candidate.starts_with(root))
                    {
                        out_of_scope.insert(candidate);
                        continue;
                    }
                    if self.should_skip(&candidate) {
                        excluded.insert(candidate);
                        continue;
                    }
                    if !has_cargo_toml(&candidate) {
                        continue;
                    }
                    found.insert(candidate.clone());
                    linked.insert(candidate);
                }
                outcomes.push(WorktreeDiscovery::Success {
                    primary: primary.to_path_buf(),
                    linked: linked.into_iter().collect(),
                    excluded: excluded.into_iter().collect(),
                    out_of_scope: out_of_scope.into_iter().collect(),
                });
            }
            Err(err) => {
                record_discovery_failure(primary, &err.to_string(), outcomes, errors);
            }
        }
    }

    pub(crate) fn is_excluded(&self, path: &Path) -> bool {
        self.should_skip(path)
    }

    fn should_skip(&self, path: &Path) -> bool {
        self.exclusion_matchers.matches(path)
    }
}

impl ExclusionMatchers {
    fn new<'a>(excludes: impl IntoIterator<Item = &'a str>) -> Self {
        let mut matchers = Self::default();
        for exclude in excludes {
            let exclude = Path::new(exclude);
            if exclude.as_os_str().is_empty() {
                continue;
            }
            if exclude.is_absolute() {
                matchers.absolute.push(AbsoluteExclusion {
                    lexical: exclude.to_path_buf(),
                    canonical: canonicalize_with_missing_suffix(exclude),
                });
                continue;
            }

            let components = exclude
                .components()
                .filter(|component| !matches!(component, std::path::Component::CurDir))
                .map(|component| component.as_os_str().to_os_string())
                .collect::<Vec<_>>();
            if !components.is_empty() {
                matchers.relative.push(components);
            }
        }
        matchers
    }

    fn matches(&self, path: &Path) -> bool {
        if self.absolute.iter().any(|exclude| {
            path.starts_with(&exclude.lexical)
                || exclude
                    .canonical
                    .as_deref()
                    .is_some_and(|canonical| path.starts_with(canonical))
        }) {
            return true;
        }

        let path_components = path
            .components()
            .map(|component| component.as_os_str())
            .collect::<Vec<_>>();
        self.relative.iter().any(|exclude| {
            path_components.windows(exclude.len()).any(|window| {
                window
                    .iter()
                    .zip(exclude)
                    .all(|(actual, expected)| *actual == expected.as_os_str())
            })
        })
    }
}

fn scanner_scope_hash(options: &ScannerOptions) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"scanner-scope-v1\0");
    hasher.update(format!("{options:?}").as_bytes());
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hash, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hash
}

fn origin_errors(origin: &OriginScan) -> Vec<ScanError> {
    origin.errors.clone()
}

fn joined_error_messages(errors: &[ScanError]) -> Option<String> {
    let mut messages = errors
        .iter()
        .map(|error| error.message.clone())
        .collect::<Vec<_>>();
    messages.sort();
    messages.dedup();
    (!messages.is_empty()).then(|| messages.join("; "))
}

fn origin_kind_label(kind: DiscoveryOriginKind) -> &'static str {
    match kind {
        DiscoveryOriginKind::ScanRoot => "scan root",
        DiscoveryOriginKind::ExplicitProject => "explicit project",
    }
}

fn canonicalize_with_missing_suffix(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path;
    let mut unresolved = Vec::new();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(mut canonical) => {
                for component in unresolved.iter().rev() {
                    canonical.push(component);
                }
                return Some(canonical);
            }
            Err(_) => {
                unresolved.push(ancestor.file_name()?.to_os_string());
                ancestor = ancestor.parent()?;
            }
        }
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
    let mut record: Option<PorcelainWorktreeRecord> = None;
    let mut primary_records = 0;
    for field in output[..output.len() - 1].split(|byte| *byte == 0) {
        if field.is_empty() {
            let Some(record) = record.take() else {
                return Err(malformed_worktree_output(
                    "git worktree list returned an empty record",
                ));
            };
            record.validate()?;
            let path = record.path;
            if !seen.insert(path.clone()) {
                return Err(malformed_worktree_output(
                    "git worktree list returned a duplicate worktree path",
                ));
            }
            if path == primary {
                primary_records += 1;
            } else {
                paths.push(path);
            }
            continue;
        }

        if let Some(record) = record.as_mut() {
            record.add_field(field)?;
        } else {
            let Some(path) = field.strip_prefix(b"worktree ") else {
                return Err(malformed_worktree_output(
                    "git worktree list record does not start with a worktree path",
                ));
            };
            let path = git_path_from_bytes(path)?;
            if !path.is_absolute() {
                return Err(malformed_worktree_output(
                    "git worktree list returned a relative worktree path",
                ));
            }
            record = Some(PorcelainWorktreeRecord::new(path));
        }
    }

    if record.is_some() {
        return Err(malformed_worktree_output(
            "git worktree list returned a record without a terminating separator",
        ));
    }
    if seen.is_empty() {
        return Err(malformed_worktree_output(
            "git worktree list returned no worktree records",
        ));
    }
    if primary_records != 1 {
        return Err(malformed_worktree_output(
            "git worktree list did not include exactly one queried primary checkout",
        ));
    }
    Ok(paths)
}

#[derive(Debug)]
struct PorcelainWorktreeRecord {
    path: PathBuf,
    head: bool,
    branch: bool,
    detached: bool,
    bare: bool,
    locked: bool,
    prunable: bool,
}

impl PorcelainWorktreeRecord {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            head: false,
            branch: false,
            detached: false,
            bare: false,
            locked: false,
            prunable: false,
        }
    }

    fn add_field(&mut self, field: &[u8]) -> Result<(), GitWorktreeError> {
        if let Some(object_id) = field.strip_prefix(b"HEAD ") {
            if self.head
                || self.branch
                || self.detached
                || self.bare
                || self.locked
                || self.prunable
                || !valid_git_object_id(object_id)
            {
                return Err(malformed_worktree_output(
                    "git worktree list returned a duplicate or malformed HEAD field",
                ));
            }
            self.head = true;
        } else if let Some(branch) = field.strip_prefix(b"branch ") {
            if !self.head
                || self.branch
                || self.detached
                || self.bare
                || self.locked
                || self.prunable
                || branch.is_empty()
            {
                return Err(malformed_worktree_output(
                    "git worktree list returned a duplicate or empty branch field",
                ));
            }
            self.branch = true;
        } else if field == b"detached" {
            if !self.head
                || self.branch
                || self.detached
                || self.bare
                || self.locked
                || self.prunable
            {
                return Err(malformed_worktree_output(
                    "git worktree list returned a duplicate detached field",
                ));
            }
            self.detached = true;
        } else if field == b"bare" {
            if self.head
                || self.branch
                || self.detached
                || self.bare
                || self.locked
                || self.prunable
            {
                return Err(malformed_worktree_output(
                    "git worktree list returned a duplicate bare field",
                ));
            }
            self.bare = true;
        } else if valid_optional_marker(field, b"locked") {
            if !self.core_complete() || self.locked || self.prunable {
                return Err(malformed_worktree_output(
                    "git worktree list returned an out-of-order or duplicate locked field",
                ));
            }
            self.locked = true;
        } else if valid_optional_marker(field, b"prunable") {
            if !self.core_complete() || self.prunable {
                return Err(malformed_worktree_output(
                    "git worktree list returned an out-of-order or duplicate prunable field",
                ));
            }
            self.prunable = true;
        } else {
            return Err(malformed_worktree_output(
                "git worktree list returned an unknown or malformed field",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), GitWorktreeError> {
        if self.core_complete() {
            Ok(())
        } else {
            Err(malformed_worktree_output(
                "git worktree list returned an incomplete or contradictory record",
            ))
        }
    }

    fn core_complete(&self) -> bool {
        let valid_bare = self.bare && !self.head && !self.branch && !self.detached;
        let valid_checkout = !self.bare && self.head && (self.branch ^ self.detached);
        valid_bare || valid_checkout
    }
}

fn valid_git_object_id(value: &[u8]) -> bool {
    matches!(value.len(), 40 | 64) && value.iter().all(u8::is_ascii_hexdigit)
}

fn valid_optional_marker(field: &[u8], marker: &[u8]) -> bool {
    field == marker
        || field
            .strip_prefix(marker)
            .and_then(|suffix| suffix.strip_prefix(b" "))
            .is_some_and(|reason| !reason.is_empty())
}

fn malformed_worktree_output(message: &str) -> GitWorktreeError {
    GitWorktreeError::new(message)
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
        kind: ScanErrorKind::WorktreeDiscovery,
    });
}

fn non_utf8_discovery_message() -> &'static str {
    "git worktree discovery returned a non-UTF-8 path that cannot be persisted safely"
}

fn non_utf8_scan_error(path: &Path) -> ScanError {
    ScanError {
        path: path.to_path_buf(),
        message: "project path is non-UTF-8 and cannot be persisted safely".to_string(),
        kind: ScanErrorKind::Scan,
    }
}

fn has_cargo_toml(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file()
}

fn scan_error(path: impl AsRef<Path>, err: std::io::Error) -> ScanError {
    ScanError {
        path: path.as_ref().to_path_buf(),
        message: err.to_string(),
        kind: ScanErrorKind::Scan,
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
        let output = b"worktree /workspace/main checkout\0HEAD 0123456789012345678901234567890123456789\0branch refs/heads/main\0\0worktree /workspace/feature\ncheckout\0HEAD abcdefabcdefabcdefabcdefabcdefabcdefabcd\0detached\0locked acceptance reason\0prunable missing gitdir\0\0";

        assert_eq!(
            parse_git_worktree_porcelain(primary, output).unwrap(),
            vec![PathBuf::from("/workspace/feature\ncheckout")]
        );
    }

    #[test]
    fn parser_accepts_complete_bare_record() {
        let primary = Path::new("/workspace/repo.git");
        let output = b"worktree /workspace/repo.git\0bare\0locked\0\0";

        assert!(parse_git_worktree_porcelain(primary, output)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn parser_rejects_incomplete_or_contradictory_record_fields() {
        let primary = Path::new("/workspace/main");
        let oid = b"0123456789012345678901234567890123456789";
        let mut cases = vec![
            b"worktree /workspace/main\0\0".to_vec(),
            b"worktree relative\0bare\0\0".to_vec(),
            b"worktree /workspace/main\0HEAD abc\0branch refs/heads/main\0\0".to_vec(),
            b"worktree /workspace/main\0HEAD 0123456789012345678901234567890123456789\0\0"
                .to_vec(),
            b"worktree /workspace/main\0HEAD 0123456789012345678901234567890123456789\0branch \0\0"
                .to_vec(),
            b"worktree /workspace/main\0HEAD 0123456789012345678901234567890123456789\0branch refs/heads/main\0detached\0\0"
                .to_vec(),
            b"worktree /workspace/main\0bare\0HEAD 0123456789012345678901234567890123456789\0branch refs/heads/main\0\0"
                .to_vec(),
            b"worktree /workspace/main\0bare\0unknown value\0\0".to_vec(),
            b"worktree /workspace/main\0bare\0locked \0\0".to_vec(),
            b"worktree /workspace/main\0bare\0prunable \0\0".to_vec(),
            b"worktree /workspace/main\0bare\0bare\0\0".to_vec(),
            b"worktree /workspace/main\0bare\0locked\0locked reason\0\0".to_vec(),
            b"worktree /workspace/main\0locked\0bare\0\0".to_vec(),
            b"worktree /workspace/main\0bare\0prunable reason\0locked reason\0\0".to_vec(),
            b"worktree /workspace/main\0branch refs/heads/main\0HEAD 0123456789012345678901234567890123456789\0\0"
                .to_vec(),
        ];
        let mut duplicate_head = b"worktree /workspace/main\0HEAD ".to_vec();
        duplicate_head.extend_from_slice(oid);
        duplicate_head.extend_from_slice(b"\0HEAD ");
        duplicate_head.extend_from_slice(oid);
        duplicate_head.extend_from_slice(b"\0detached\0\0");
        cases.push(duplicate_head);

        for output in cases {
            assert!(
                parse_git_worktree_porcelain(primary, &output).is_err(),
                "unexpectedly accepted {output:?}"
            );
        }
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
            stdout: b"worktree /workspace/main\0HEAD 0123456789012345678901234567890123456789\0branch refs/heads/main\0\0worktree /workspace/feature\0HEAD abcdefabcdefabcdefabcdefabcdefabcdefabcd\0detached\0\0"
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
        let output = b"worktree /workspace/main\0HEAD 0123456789012345678901234567890123456789\0branch refs/heads/main\0\0worktree /workspace/\xff\0HEAD abcdefabcdefabcdefabcdefabcdefabcdefabcd\0detached\0\0";
        let parsed = parse_git_worktree_porcelain(primary, output).unwrap();

        assert_eq!(
            parsed[0],
            PathBuf::from(OsString::from_vec(b"/workspace/\xff".to_vec()))
        );
    }
}
