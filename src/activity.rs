use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySignal {
    pub pid: u32,
    pub project_path: PathBuf,
    pub reason: String,
}

pub trait ProcessInspector {
    fn active_projects(&self, projects: &[PathBuf]) -> Result<Vec<ActivitySignal>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopProcessInspector;

impl ProcessInspector for NoopProcessInspector {
    fn active_projects(&self, _projects: &[PathBuf]) -> Result<Vec<ActivitySignal>> {
        Ok(Vec::new())
    }
}

pub fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}
