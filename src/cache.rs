use crate::store::Store;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct Cache<'a> {
    store: &'a Store,
}

impl<'a> Cache<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn verify(&self, path: impl AsRef<Path>) -> Result<bool> {
        let path = path.as_ref();
        Ok(path.is_dir() && path.join("Cargo.toml").is_file())
    }

    pub fn sync_on_disk(&self) -> Result<Vec<PathBuf>> {
        self.store.normalize_resolvable_project_aliases()?;
        let mut removed = Vec::new();
        for project in self.store.all_projects()? {
            let path = PathBuf::from(&project.path);
            if !self.verify(&path)? {
                self.store.remove_project(&path)?;
                removed.push(path);
                continue;
            }
            let canonical = path
                .canonicalize()
                .with_context(|| format!("canonicalize cached project {}", path.display()))?;
            if canonical != path {
                self.store.replace_cached_project_path(&path, &canonical)?;
            }
        }
        Ok(removed)
    }

    pub fn reconcile_for_review<F>(&self, is_excluded: F) -> Result<Vec<PathBuf>>
    where
        F: FnMut(&Path) -> bool,
    {
        self.store
            .reconcile_excluded_worktree_discovery_state(is_excluded)?;
        self.store.normalize_resolvable_project_aliases()?;
        Ok(Vec::new())
    }
}
