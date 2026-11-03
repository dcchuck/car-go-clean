//! Read-only observations of historical failures. Never grants cleanup authority.
use crate::cleaner::cache_tag_status;
use crate::policy::ScopePolicy;
use crate::store::CleanupFailure;
use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const RETRY_DELAY: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureState {
    TargetMissing,
    TargetEmpty,
    NeedsAttention,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureObservation {
    pub path: String,
    pub state: FailureState,
    pub reason: String,
    pub retry_after: Option<u64>,
}

pub fn observe(
    failure: &CleanupFailure,
    policy: &ScopePolicy,
    now: SystemTime,
) -> FailureObservation {
    let project = Path::new(&failure.path);
    let mut observation = FailureObservation {
        path: failure.path.clone(),
        state: FailureState::Unverified,
        reason: "outside current scope, excluded, or protected".into(),
        retry_after: None,
    };
    let target = project.join("target");
    if !policy.contains_project(project)
        || policy.is_excluded(project)
        || policy.is_excluded(&target)
        || policy
            .diagnostics()
            .protected_roots
            .iter()
            .any(|root| target.starts_with(&root.path))
    {
        return observation;
    }
    match inspect(&target) {
        Ok(state) => {
            observation.reason = match state {
                FailureState::TargetMissing => {
                    "target observed absent; historical failure retained"
                }
                FailureState::TargetEmpty => {
                    "target observed empty; nothing to clean; historical failure retained"
                }
                _ => "target still contains entries; inspect recorded failure before retrying",
            }
            .into();
            observation.state = state;
            if observation.state == FailureState::NeedsAttention {
                observation.retry_after = retry_after(failure, now);
            }
        }
        Err(error) => observation.reason = error,
    }
    observation
}

// Check every component, including ancestors: a dangling ancestor symlink must
// not become "missing". Recheck metadata after inspection to detect replacement
// or concurrent directory changes. These are snapshots, never durable resolution.
fn inspect(target: &Path) -> Result<FailureState, String> {
    inspect_with_hook(target, || {})
}

fn inspect_with_hook(target: &Path, after_read: impl FnOnce()) -> Result<FailureState, String> {
    if !target.is_absolute()
        || target
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("non-absolute or non-normalized historical path".into());
    }
    let mut path = PathBuf::new();
    let mut checked = Vec::new();
    let mut state = FailureState::NeedsAttention;
    for component in target.components() {
        path.push(component.as_os_str());
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                checked.push((path.clone(), signature(&meta)));
            }
            Ok(_) => return Err("symlink or non-directory in target path; not reconciled".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                state = FailureState::TargetMissing;
                break;
            }
            Err(e) => return Err(format!("cannot inspect path: {e}")),
        }
    }
    if state != FailureState::TargetMissing {
        let mut entries = fs::read_dir(target).map_err(|e| format!("cannot read target: {e}"))?;
        if entries
            .next()
            .transpose()
            .map_err(|e| format!("cannot read target entry: {e}"))?
            .is_none()
        {
            state = FailureState::TargetEmpty;
        }
    }
    after_read();
    for (path, before) in checked.iter().rev() {
        let after = fs::symlink_metadata(path)
            .map_err(|e| format!("path changed during inspection: {e}"))?;
        let after_signature = signature(&after);
        let contents_matter = path == target
            || (state == FailureState::TargetMissing
                && checked.last().is_some_and(|(last, _)| last == path));
        if !after.is_dir()
            || after.file_type().is_symlink()
            || (after_signature.0, after_signature.1) != (before.0, before.1)
            || (contents_matter && after_signature != *before)
        {
            return Err("path changed during inspection; not reconciled".into());
        }
    }
    if state == FailureState::TargetMissing
        && !matches!(fs::symlink_metadata(&path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    {
        return Err("missing path changed during inspection; not reconciled".into());
    }
    Ok(state)
}

#[cfg(unix)]
fn signature(meta: &fs::Metadata) -> (u64, u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        meta.dev(),
        meta.ino(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

/// Bounded backoff, not a claim that all nested contents are unchanged. Only
/// repeated cache-marker errors qualify. A valid marker or top-level write ends
/// the delay early. Explicit reviewed runs bypass it; all safety checks still run.
pub fn retry_after(failure: &CleanupFailure, now: SystemTime) -> Option<u64> {
    if failure.reason != "cargo_cache_tag"
        || !failure.previous_cache_tag_failure
        || failure.consecutive_failures < 2
    {
        return None;
    }
    let last = u64::try_from(failure.last_attempt_at).ok()?;
    let now = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let deadline = last.checked_add(RETRY_DELAY.as_secs())?;
    if now < last || now >= deadline {
        return None;
    }
    let target = Path::new(&failure.path).join("target");
    if inspect(&target).ok()? != FailureState::NeedsAttention {
        return None;
    }
    let modified = fs::symlink_metadata(&target)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    if modified > Duration::from_secs(last) {
        return None;
    }
    match cache_tag_status(&target) {
        "missing" | "invalid_signature" => Some(deadline),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().canonicalize().unwrap().join("project");
        fs::create_dir(&project).unwrap();
        (dir, project)
    }

    fn failure(project: &Path, last: u64) -> CleanupFailure {
        CleanupFailure {
            path: project.to_string_lossy().into_owned(),
            last_attempt_at: last as i64,
            consecutive_failures: 2,
            previous_cache_tag_failure: true,
            bytes_remaining_at_attempt: 1,
            exit_code: Some(101),
            stderr_excerpt: "missing CACHEDIR.TAG".into(),
            reason: "cargo_cache_tag".into(),
            remediation: String::new(),
        }
    }

    #[test]
    fn missing_empty_and_reappearing_targets_are_observations_not_permanent_resolution() {
        let (_dir, project) = fixture();
        let target = project.join("target");
        assert_eq!(inspect(&target).unwrap(), FailureState::TargetMissing);
        fs::create_dir(&target).unwrap();
        assert_eq!(inspect(&target).unwrap(), FailureState::TargetEmpty);
        fs::write(target.join("zero-byte"), []).unwrap();
        assert_eq!(inspect(&target).unwrap(), FailureState::NeedsAttention);
        fs::remove_file(target.join("zero-byte")).unwrap();
        symlink("missing", target.join("dangling")).unwrap();
        assert_eq!(inspect(&target).unwrap(), FailureState::NeedsAttention);
    }

    #[test]
    fn symlink_ancestors_targets_and_non_directories_remain_unverified() {
        let (dir, project) = fixture();
        let target = project.join("target");
        symlink("missing", &target).unwrap();
        assert!(inspect(&target).is_err());
        let alias = dir.path().canonicalize().unwrap().join("alias");
        symlink(&project, &alias).unwrap();
        assert!(inspect(&alias.join("target")).is_err());
        fs::remove_file(&target).unwrap();
        fs::write(&target, []).unwrap();
        assert!(inspect(&target).is_err());
        assert!(inspect(Path::new("relative/target")).is_err());
        assert!(inspect(&project.join("../project/target")).is_err());
    }

    #[test]
    fn concurrent_create_and_replacement_are_not_reconciled() {
        let (_dir, project) = fixture();
        let target = project.join("target");
        assert!(inspect_with_hook(&target, || fs::create_dir(&target).unwrap()).is_err());
        assert!(
            inspect_with_hook(&target, || fs::write(target.join("build"), b"new").unwrap())
                .is_err()
        );
        fs::remove_file(target.join("build")).unwrap();
        assert!(inspect_with_hook(&target, || {
            fs::rename(&target, project.join("old")).unwrap();
            fs::create_dir(&target).unwrap();
        })
        .is_err());
    }

    #[test]
    fn unreadable_directory_is_not_empty() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (_dir, project) = fixture();
        let target = project.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();
        let result = inspect(&target);
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn backoff_is_bounded_and_valid_marker_or_new_output_allows_retry() {
        let (_dir, project) = fixture();
        let target = project.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("artifact"), b"x").unwrap();
        let last = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 2;
        let f = failure(&project, last);
        assert_eq!(
            retry_after(&f, UNIX_EPOCH + Duration::from_secs(last)),
            Some(last + RETRY_DELAY.as_secs())
        );
        assert_eq!(
            retry_after(&f, UNIX_EPOCH + Duration::from_secs(last) + RETRY_DELAY),
            None
        );
        assert_eq!(
            retry_after(&f, UNIX_EPOCH + Duration::from_secs(last - 1)),
            None
        );
        let mut different_previous_error = f.clone();
        different_previous_error.previous_cache_tag_failure = false;
        assert_eq!(
            retry_after(
                &different_previous_error,
                UNIX_EPOCH + Duration::from_secs(last)
            ),
            None
        );
        let mut first = f.clone();
        first.consecutive_failures = 1;
        assert_eq!(
            retry_after(&first, UNIX_EPOCH + Duration::from_secs(last)),
            None
        );
        let mut other = f.clone();
        other.reason = "cargo_failed".into();
        assert_eq!(
            retry_after(&other, UNIX_EPOCH + Duration::from_secs(last)),
            None
        );
        fs::write(
            target.join("CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55",
        )
        .unwrap();
        assert_eq!(
            retry_after(&f, UNIX_EPOCH + Duration::from_secs(last)),
            None
        );
        fs::remove_file(target.join("CACHEDIR.TAG")).unwrap();
        let file = fs::File::open(&target).unwrap();
        file.set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(last + 1)),
        )
        .unwrap();
        assert_eq!(
            retry_after(&f, UNIX_EPOCH + Duration::from_secs(last + 2)),
            None
        );
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::policy::Environment;
    use std::ffi::OsString;
    struct Home(PathBuf);
    impl Environment for Home {
        fn var_os(&self, name: &str) -> Option<OsString> {
            (name == "HOME").then(|| self.0.as_os_str().to_owned())
        }
    }
    #[test]
    fn excluded_protected_and_out_of_scope_history_stays_unverified() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().canonicalize().unwrap();
        let cfg = root.join("config.toml");
        fs::write(
            &cfg,
            format!(
                "scan_dirs = [{}]\noverride_excludes = [\"excluded\"]\n",
                toml::Value::String(root.to_string_lossy().into_owned())
            ),
        )
        .unwrap();
        let policy = ScopePolicy::build(
            &crate::config::load(&cfg).unwrap(),
            &cfg,
            &Home(root.clone()),
        )
        .unwrap();
        for path in [
            root.join("excluded/project"),
            root.join(".cargo/project"),
            root.join(if cfg!(target_os = "macos") {
                "OrbStack/project"
            } else {
                ".local/share/containers/project"
            }),
            PathBuf::from("/outside/project"),
        ] {
            let f = CleanupFailure {
                path: path.to_string_lossy().into_owned(),
                last_attempt_at: 1,
                consecutive_failures: 2,
                previous_cache_tag_failure: true,
                bytes_remaining_at_attempt: 1,
                exit_code: Some(101),
                stderr_excerpt: String::new(),
                reason: "cargo_cache_tag".into(),
                remediation: String::new(),
            };
            assert_eq!(
                observe(&f, &policy, SystemTime::now()).state,
                FailureState::Unverified,
                "{}",
                path.display()
            );
        }
    }
}
