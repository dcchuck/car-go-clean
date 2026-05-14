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

impl Scanner {
    pub fn new(opts: ScannerOptions) -> Self {
        Self { opts }
    }

    pub fn scan(&self) -> Result<Vec<PathBuf>> {
        let mut found = BTreeSet::new();
        for root in &self.opts.roots {
            self.walk(root, &[], &mut found)?;
        }
        for project in &self.opts.project_dirs {
            if has_cargo_toml(project) {
                found.insert(project.clone());
            }
        }
        Ok(found.into_iter().collect())
    }

    fn walk(
        &self,
        dir: &Path,
        parent_ignores: &[Arc<Gitignore>],
        found: &mut BTreeSet<PathBuf>,
    ) -> Result<()> {
        let Ok(meta) = fs::metadata(dir) else {
            return Ok(());
        };
        if !meta.is_dir() || self.should_skip(dir) || is_ignored(parent_ignores, dir, true) {
            return Ok(());
        }
        if has_cargo_toml(dir) {
            found.insert(dir.to_path_buf());
            return Ok(());
        }
        let ignores = ignores_for(dir, parent_ignores);
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() && !file_type.is_symlink() {
                self.walk(&entry.path(), &ignores, found)?;
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
