use crate::store::Store;
use anyhow::Result;
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
        let mut removed = Vec::new();
        for project in self.store.all_projects()? {
            let path = PathBuf::from(&project.path);
            if !self.verify(&path)? {
                self.store.remove_project(&path)?;
                removed.push(path);
            }
        }
        Ok(removed)
    }
}
