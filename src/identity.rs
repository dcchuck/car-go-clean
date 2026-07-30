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
        (Some(_), Some(_)) => IdentityComparison::StaleAcrossBoot,
        _ if observed == current => IdentityComparison::Matches,
        _ => IdentityComparison::Replaced,
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
    Ok(linux_boot_session_from_read(fs::read_to_string(
        "/proc/sys/kernel/random/boot_id",
    )))
}

#[cfg(any(target_os = "linux", test))]
fn linux_boot_session_from_read(read: std::io::Result<String>) -> Option<BootSessionId> {
    let value = read.ok()?;
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || bytes.iter().enumerate().any(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte != b'-'
            } else {
                !byte.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    Some(BootSessionId(value.to_string()))
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
    if result != 0 || length != std::mem::size_of::<libc::timeval>() {
        return Ok(macos_boot_session_from_sysctl_result(result, length, None));
    }
    Ok(macos_boot_session_from_sysctl_result(
        result,
        length,
        Some(unsafe { boot_time.assume_init() }),
    ))
}

#[cfg(any(target_os = "macos", test))]
fn macos_boot_session_from_sysctl_result(
    result: i32,
    length: usize,
    boot_time: Option<libc::timeval>,
) -> Option<BootSessionId> {
    if result != 0 || length != std::mem::size_of::<libc::timeval>() {
        return None;
    }
    macos_boot_session_from_timeval(boot_time?)
}

#[cfg(any(target_os = "macos", test))]
fn macos_boot_session_from_timeval(boot_time: libc::timeval) -> Option<BootSessionId> {
    if boot_time.tv_sec < 0 || boot_time.tv_usec < 0 || boot_time.tv_usec >= 1_000_000 {
        return None;
    }
    Some(BootSessionId(format!(
        "{}:{}",
        boot_time.tv_sec, boot_time.tv_usec
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_boot_session() -> Result<Option<BootSessionId>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn linux_boot_session_accepts_only_valid_uuid_text() {
        assert_eq!(
            linux_boot_session_from_read(Err(io::Error::from(io::ErrorKind::NotFound))),
            None
        );
        assert_eq!(linux_boot_session_from_read(Ok("  \n".to_string())), None);
        assert_eq!(
            linux_boot_session_from_read(Ok("not-a-boot-uuid\n".to_string())),
            None
        );
        assert_eq!(
            linux_boot_session_from_read(Ok("ABCDEF01-2345-6789-abcd-ef0123456789\n".to_string())),
            Some(BootSessionId(
                "ABCDEF01-2345-6789-abcd-ef0123456789".to_string()
            ))
        );
    }

    #[test]
    fn macos_sysctl_result_requires_valid_shape_and_timeval() {
        let valid = libc::timeval {
            tv_sec: 1_725_000_000,
            tv_usec: 42,
        };
        assert_eq!(
            macos_boot_session_from_sysctl_result(
                -1,
                std::mem::size_of::<libc::timeval>(),
                Some(valid),
            ),
            None
        );
        assert_eq!(
            macos_boot_session_from_sysctl_result(0, 1, Some(valid)),
            None
        );
        assert_eq!(
            macos_boot_session_from_sysctl_result(
                0,
                std::mem::size_of::<libc::timeval>(),
                Some(valid),
            ),
            Some(BootSessionId("1725000000:42".to_string()))
        );
        assert_eq!(
            macos_boot_session_from_timeval(libc::timeval {
                tv_sec: -1,
                tv_usec: 42,
            }),
            None
        );
        assert_eq!(
            macos_boot_session_from_timeval(libc::timeval {
                tv_sec: 1_725_000_000,
                tv_usec: 1_000_000,
            }),
            None
        );
    }
}
