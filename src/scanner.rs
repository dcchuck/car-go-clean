use anyhow::Result;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

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
            self.walk(root, &mut found)?;
        }
        for project in &self.opts.project_dirs {
            if has_cargo_toml(project) {
                found.insert(project.clone());
            }
        }
        Ok(found.into_iter().collect())
    }

    fn walk(&self, dir: &Path, found: &mut BTreeSet<PathBuf>) -> Result<()> {
        let Ok(meta) = fs::metadata(dir) else {
            return Ok(());
        };
        if !meta.is_dir() || self.should_skip(dir) {
            return Ok(());
        }
        if has_cargo_toml(dir) {
            found.insert(dir.to_path_buf());
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() && !file_type.is_symlink() {
                self.walk(&entry.path(), found)?;
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
