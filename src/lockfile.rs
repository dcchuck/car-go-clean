use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::Path;

pub struct Lock {
    file: File,
}

impl Lock {
    pub fn release(self) -> Result<()> {
        FileExt::unlock(&self.file)?;
        Ok(())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn try_acquire(path: impl AsRef<Path>) -> Result<Lock> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open lock {}", path.display()))?;
    file.try_lock_exclusive()
        .with_context(|| format!("acquire lock {}", path.display()))?;
    Ok(Lock { file })
}
