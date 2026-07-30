use crate::config::Config;
use crate::storage::{protected_roots_for, validate_absolute_physical_path, HostPlatform};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const POLICY_HASH_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum ProtectedRootKind {
    Cargo,
    Rustup,
    GoModule,
    Bun,
    ManagedCache,
    Container,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RootProvenance {
    Default,
    Environment(String),
    ServiceDefinition,
    Structural,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ProtectedRoot {
    pub path: PathBuf,
    pub kind: ProtectedRootKind,
    pub provenance: RootProvenance,
}

pub trait Environment {
    fn var_os(&self, name: &str) -> Option<OsString>;
}

pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn var_os(&self, name: &str) -> Option<OsString> {
        env::var_os(name)
    }
}

pub trait Canonicalizer {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;

    fn resolve_physical(&self, path: &Path) -> io::Result<PathBuf> {
        self.canonicalize(path)
    }
}

struct FileSystemCanonicalizer;

impl Canonicalizer for FileSystemCanonicalizer {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }

    fn resolve_physical(&self, path: &Path) -> io::Result<PathBuf> {
        crate::storage::resolve_physical_path(path)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopePolicy {
    canonical_scan_roots: Vec<PathBuf>,
    canonical_project_paths: Vec<PathBuf>,
    lexical_exclusions: Vec<String>,
    canonical_exclusions: Vec<PathBuf>,
    protected_roots: Vec<ProtectedRoot>,
    target_quiet_period: Duration,
    scan_interval: Duration,
    config_source: PathBuf,
    hash: String,
    #[serde(skip)]
    policy_hash_format_version: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScopePolicyDiagnostics<'a> {
    pub policy_hash_format_version: u32,
    pub hash: &'a str,
    pub canonical_scan_roots: &'a [PathBuf],
    pub canonical_project_paths: &'a [PathBuf],
    pub lexical_exclusions: &'a [String],
    pub canonical_exclusions: &'a [PathBuf],
    pub protected_roots: &'a [ProtectedRoot],
    pub target_quiet_period_ms: u128,
    pub scan_interval_ms: u128,
    pub config_source: &'a Path,
}

#[derive(Serialize)]
struct PolicyHashInput<'a> {
    format_version: u32,
    scan_roots: &'a [PathBuf],
    project_paths: &'a [PathBuf],
    lexical_exclusions: &'a [String],
    canonical_exclusions: &'a [PathBuf],
    protected_roots: &'a [ProtectedRoot],
    target_quiet_period_ms: u128,
    scan_interval_ms: u128,
    config_source: &'a Path,
}

impl ScopePolicy {
    pub fn build(
        config: &Config,
        config_source: &Path,
        environment: &dyn Environment,
    ) -> Result<Self> {
        Self::build_with_canonicalizer(config, config_source, environment, &FileSystemCanonicalizer)
    }

    #[doc(hidden)]
    pub fn build_with_canonicalizer(
        config: &Config,
        config_source: &Path,
        environment: &dyn Environment,
        canonicalizer: &dyn Canonicalizer,
    ) -> Result<Self> {
        let mut canonical_scan_roots =
            canonicalize_required(&config.scan_dirs, "scan root", canonicalizer)?;
        let mut canonical_project_paths =
            canonicalize_required(&config.project_dirs, "explicit project", canonicalizer)?;
        sort_and_deduplicate(&mut canonical_scan_roots);
        sort_and_deduplicate(&mut canonical_project_paths);

        let mut lexical_exclusions = config.effective_excludes();
        lexical_exclusions.retain(|value| !value.is_empty());
        sort_and_deduplicate(&mut lexical_exclusions);

        let mut canonical_exclusions = Vec::new();
        for exclusion in &lexical_exclusions {
            let path = Path::new(exclusion);
            if !path.is_absolute() {
                continue;
            }
            match canonicalizer.canonicalize(path) {
                Ok(path) => canonical_exclusions.push(path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    bail!("canonicalize exclusion {}: {error}", path.display());
                }
            }
        }
        sort_and_deduplicate(&mut canonical_exclusions);

        let home = environment
            .var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let home = if home.as_os_str().is_empty() {
            home
        } else {
            validate_absolute_physical_path("HOME", &home)?;
            canonicalizer.resolve_physical(&home).with_context(|| {
                format!(
                    "resolve HOME as an absolute physical path: {}",
                    home.display()
                )
            })?
        };
        let mut protected_roots = protected_roots_for(HostPlatform::current(), &home, environment);
        for root in &mut protected_roots {
            if let RootProvenance::Environment(variable) = &root.provenance {
                validate_absolute_physical_path(variable, &root.path)?;
                root.path = canonicalizer
                    .resolve_physical(&root.path)
                    .with_context(|| {
                        format!(
                            "resolve {variable} as an absolute physical path: {}",
                            root.path.display()
                        )
                    })?;
            }
        }
        sort_and_deduplicate(&mut protected_roots);

        let hash_input = PolicyHashInput {
            format_version: POLICY_HASH_FORMAT_VERSION,
            scan_roots: &canonical_scan_roots,
            project_paths: &canonical_project_paths,
            lexical_exclusions: &lexical_exclusions,
            canonical_exclusions: &canonical_exclusions,
            protected_roots: &protected_roots,
            target_quiet_period_ms: config.target_quiet_period.as_millis(),
            scan_interval_ms: config.scan_interval.as_millis(),
            config_source,
        };
        let hash = hash_policy_input(&hash_input)?;

        Ok(Self {
            canonical_scan_roots,
            canonical_project_paths,
            lexical_exclusions,
            canonical_exclusions,
            protected_roots,
            target_quiet_period: config.target_quiet_period,
            scan_interval: config.scan_interval,
            config_source: config_source.to_path_buf(),
            hash,
            policy_hash_format_version: POLICY_HASH_FORMAT_VERSION,
        })
    }

    pub fn contains_project(&self, path: &Path) -> bool {
        self.canonical_scan_roots
            .iter()
            .any(|root| path.starts_with(root))
            || self
                .canonical_project_paths
                .iter()
                .any(|project| path == project)
    }

    pub fn is_excluded(&self, path: &Path) -> bool {
        self.canonical_exclusions
            .iter()
            .any(|exclusion| path.starts_with(exclusion))
            || self
                .lexical_exclusions
                .iter()
                .any(|exclusion| lexical_exclusion_matches(path, Path::new(exclusion)))
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn diagnostics(&self) -> ScopePolicyDiagnostics<'_> {
        ScopePolicyDiagnostics {
            policy_hash_format_version: self.policy_hash_format_version,
            hash: &self.hash,
            canonical_scan_roots: &self.canonical_scan_roots,
            canonical_project_paths: &self.canonical_project_paths,
            lexical_exclusions: &self.lexical_exclusions,
            canonical_exclusions: &self.canonical_exclusions,
            protected_roots: &self.protected_roots,
            target_quiet_period_ms: self.target_quiet_period.as_millis(),
            scan_interval_ms: self.scan_interval.as_millis(),
            config_source: &self.config_source,
        }
    }
}

fn hash_policy_input(input: &PolicyHashInput<'_>) -> Result<String> {
    let serialized = serde_json::to_vec(input).context("serialize cleanup policy hash input")?;
    let digest = Sha256::digest(serialized);
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hash, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(hash)
}

fn canonicalize_required(
    paths: &[PathBuf],
    kind: &str,
    canonicalizer: &dyn Canonicalizer,
) -> Result<Vec<PathBuf>> {
    paths
        .iter()
        .map(|path| {
            canonicalizer
                .canonicalize(path)
                .with_context(|| format!("canonicalize {kind} {}", path.display()))
        })
        .collect()
}

fn sort_and_deduplicate<T: Ord>(values: &mut Vec<T>) {
    values.sort();
    values.dedup();
}

fn lexical_exclusion_matches(path: &Path, exclusion: &Path) -> bool {
    if exclusion.as_os_str().is_empty() {
        return false;
    }
    if exclusion.is_absolute() {
        return path.starts_with(exclusion);
    }

    let exclusion_components = exclusion
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .map(|component| component.as_os_str())
        .collect::<Vec<_>>();
    if exclusion_components.is_empty() {
        return false;
    }
    let path_components = path
        .components()
        .map(|component| component.as_os_str())
        .collect::<Vec<_>>();
    path_components
        .windows(exclusion_components.len())
        .any(|window| window == exclusion_components)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_hash_format_version_changes_internal_hash() {
        let paths = Vec::<PathBuf>::new();
        let exclusions = Vec::<String>::new();
        let protected_roots = Vec::<ProtectedRoot>::new();
        let input = |format_version| PolicyHashInput {
            format_version,
            scan_roots: &paths,
            project_paths: &paths,
            lexical_exclusions: &exclusions,
            canonical_exclusions: &paths,
            protected_roots: &protected_roots,
            target_quiet_period_ms: 1,
            scan_interval_ms: 2,
            config_source: Path::new("/config.toml"),
        };

        let current = hash_policy_input(&input(POLICY_HASH_FORMAT_VERSION)).unwrap();
        let next = hash_policy_input(&input(POLICY_HASH_FORMAT_VERSION + 1)).unwrap();

        assert_ne!(current, next);
    }
}
