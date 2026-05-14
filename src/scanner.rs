use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct ScannerOptions {
    pub roots: Vec<PathBuf>,
    pub project_dirs: Vec<PathBuf>,
    pub excludes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Scanner {
    opts: ScannerOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub projects: Vec<PathBuf>,
    pub errors: Vec<ScanError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
}

impl Scanner {
    pub fn new(opts: ScannerOptions) -> Self {
        Self { opts }
    }

    pub fn scan(&self) -> Result<Vec<PathBuf>> {
        Ok(self.scan_with_errors()?.projects)
    }

    pub fn scan_with_errors(&self) -> Result<ScanReport> {
        let mut found = BTreeSet::new();
        let mut errors = Vec::new();
        for root in &self.opts.roots {
            self.walk(root, &[], &mut found, &mut errors)?;
        }
        for project in &self.opts.project_dirs {
            if has_cargo_toml(project) {
                found.insert(project.clone());
            }
        }
        Ok(ScanReport {
            projects: found.into_iter().collect(),
            errors,
        })
    }

    fn walk(
        &self,
        dir: &Path,
        parent_ignores: &[Arc<Gitignore>],
        found: &mut BTreeSet<PathBuf>,
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
            found.insert(dir.to_path_buf());
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
                self.walk(&entry.path(), &ignores, found, errors)?;
            }
        }
        Ok(())
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
                    || path
                        .components()
                        .any(|component| component.as_os_str() == exclude.as_str()))
        })
    }
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
