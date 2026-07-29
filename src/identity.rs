use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootSessionId(pub String);

pub trait IdentityProvider {
    fn boot_session(&self) -> Result<Option<BootSessionId>>;
    fn identity(&self, path: &Path) -> Result<FilesystemIdentity>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedIdentity {
    pub project: FilesystemIdentity,
    pub target: FilesystemIdentity,
    pub boot_session: Option<BootSessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityComparison {
    Matches,
    StaleAcrossBoot,
    Replaced,
}

pub fn compare_persisted(
    observed_boot: Option<&BootSessionId>,
    current_boot: Option<&BootSessionId>,
    observed: &FilesystemIdentity,
    current: &FilesystemIdentity,
) -> IdentityComparison {
    match (observed_boot, current_boot) {
        (Some(observed_boot), Some(current_boot)) if observed_boot == current_boot => {
            if observed == current {
                IdentityComparison::Matches
            } else {
                IdentityComparison::Replaced
            }
        }
        _ => IdentityComparison::StaleAcrossBoot,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemIdentityProvider;

impl IdentityProvider for SystemIdentityProvider {
    fn boot_session(&self) -> Result<Option<BootSessionId>> {
        platform_boot_session()
    }

    fn identity(&self, path: &Path) -> Result<FilesystemIdentity> {
        direct_identity(path)
    }
}

#[cfg(unix)]
fn direct_identity(path: &Path) -> Result<FilesystemIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("read direct filesystem identity for {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("{} is a symlink", path.display());
    }
    Ok(FilesystemIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn direct_identity(path: &Path) -> Result<FilesystemIdentity> {
    Err(anyhow::anyhow!(
        "filesystem identity is not supported for {} on this platform",
        path.display()
    ))
}

#[cfg(target_os = "linux")]
fn platform_boot_session() -> Result<Option<BootSessionId>> {
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .context("read Linux boot session identity")?;
    let boot_id = boot_id.trim();
    if boot_id.is_empty() {
        bail!("Linux boot session identity is empty");
    }
    Ok(Some(BootSessionId(boot_id.to_string())))
}

#[cfg(target_os = "macos")]
fn platform_boot_session() -> Result<Option<BootSessionId>> {
    let mut boot_time = std::mem::MaybeUninit::<libc::timeval>::uninit();
    let mut length = std::mem::size_of::<libc::timeval>();
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            boot_time.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("read macOS boot session identity");
    }
    if length != std::mem::size_of::<libc::timeval>() {
        bail!(
            "macOS boot session identity had unexpected size {length}, expected {}",
            std::mem::size_of::<libc::timeval>()
        );
    }
    let boot_time = unsafe { boot_time.assume_init() };
    Ok(Some(BootSessionId(format!(
        "{}:{}",
        boot_time.tv_sec, boot_time.tv_usec
    ))))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_boot_session() -> Result<Option<BootSessionId>> {
    Ok(None)
}
