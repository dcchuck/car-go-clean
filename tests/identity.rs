use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};
use car_go_clean::config;
use car_go_clean::identity::{
    compare_persisted, BootSessionId, FilesystemIdentity, IdentityComparison, IdentityProvider,
    ReviewedIdentity,
};
use car_go_clean::policy::{Environment, ScopePolicy};
use car_go_clean::safety::{
    review_project_with_identity_provider, CleanDecision, SafetyOptions, SkipReason,
};

#[derive(Default)]
struct FakeIdentityProvider {
    boot_session: Option<BootSessionId>,
    boot_error: bool,
    identities: BTreeMap<PathBuf, FilesystemIdentity>,
}

impl FakeIdentityProvider {
    fn with_boot(boot_session: Option<&str>) -> Self {
        Self {
            boot_session: boot_session.map(|value| BootSessionId(value.to_string())),
            boot_error: false,
            identities: BTreeMap::new(),
        }
    }

    fn with_boot_error(mut self) -> Self {
        self.boot_error = true;
        self
    }

    fn with_identity(mut self, path: &Path, device: u64, inode: u64) -> Self {
        self.identities
            .insert(path.to_path_buf(), FilesystemIdentity { device, inode });
        self
    }
}

impl IdentityProvider for FakeIdentityProvider {
    fn boot_session(&self) -> Result<Option<BootSessionId>> {
        if self.boot_error {
            return Err(anyhow!("fake boot session unavailable"));
        }
        Ok(self.boot_session.clone())
    }

    fn identity(&self, path: &Path) -> Result<FilesystemIdentity> {
        self.identities
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow!("no fake identity for {}", path.display()))
    }
}

struct EmptyEnvironment;

impl Environment for EmptyEnvironment {
    fn var_os(&self, _name: &str) -> Option<std::ffi::OsString> {
        None
    }
}

fn identity(device: u64, inode: u64) -> FilesystemIdentity {
    FilesystemIdentity { device, inode }
}

fn options() -> SafetyOptions {
    SafetyOptions {
        target_quiet_period: Duration::from_secs(2 * 60 * 60),
        include_managed_cache: false,
        include_active: false,
        force: true,
    }
}

fn write_project(project: &Path) {
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
}

#[test]
fn same_boot_device_or_inode_change_is_rejected() {
    let project = Path::new("/scope/project");
    let provider = FakeIdentityProvider::with_boot(Some("boot-a"))
        .with_identity(project, 8, 13)
        .with_identity(&project.join("target"), 8, 21);
    let boot = provider.boot_session().unwrap();
    let current_project = provider.identity(project).unwrap();
    let current_target = provider.identity(&project.join("target")).unwrap();

    assert_eq!(
        compare_persisted(
            Some(&BootSessionId("boot-a".to_string())),
            boot.as_ref(),
            &current_project,
            &current_project,
        ),
        IdentityComparison::Matches
    );
    assert_eq!(
        compare_persisted(
            Some(&BootSessionId("boot-a".to_string())),
            boot.as_ref(),
            &identity(7, 13),
            &current_project,
        ),
        IdentityComparison::Replaced
    );
    assert_eq!(
        compare_persisted(
            Some(&BootSessionId("boot-a".to_string())),
            boot.as_ref(),
            &identity(8, 20),
            &current_target,
        ),
        IdentityComparison::Replaced
    );
}

#[test]
fn different_boot_restats_and_reauthorizes_only_when_still_in_policy() {
    let root = tempfile::tempdir().unwrap();
    let in_scope = root.path().join("scope/project");
    let outside = root.path().join("outside/project");
    write_project(&in_scope);
    write_project(&outside);
    let in_scope = in_scope.canonicalize().unwrap();
    let outside = outside.canonicalize().unwrap();

    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "scan_dirs = [{}]\nproject_dirs = []\noverride_excludes = []\n",
            serde_json::to_string(&root.path().join("scope")).unwrap()
        ),
    )
    .unwrap();
    let config = config::load(&config_path).unwrap();
    let policy = ScopePolicy::build(&config, &config_path, &EmptyEnvironment).unwrap();
    let provider = FakeIdentityProvider::with_boot(Some("boot-b"))
        .with_identity(&in_scope, 9, 30)
        .with_identity(&in_scope.join("target"), 9, 31)
        .with_identity(&outside, 9, 40)
        .with_identity(&outside.join("target"), 9, 41);
    let current_boot = provider.boot_session().unwrap();
    let current_project = provider.identity(&in_scope).unwrap();

    assert_eq!(
        compare_persisted(
            Some(&BootSessionId("boot-a".to_string())),
            current_boot.as_ref(),
            &identity(8, 20),
            &current_project,
        ),
        IdentityComparison::StaleAcrossBoot
    );

    let refreshed = ReviewedIdentity {
        project: provider.identity(&in_scope).unwrap(),
        target: provider.identity(&in_scope.join("target")).unwrap(),
        boot_session: current_boot.clone(),
    };
    assert!(policy.contains_project(&in_scope));
    assert!(!policy.is_excluded(&in_scope));
    assert_eq!(refreshed.project, identity(9, 30));
    assert!(!policy.contains_project(&outside));
}

#[cfg(unix)]
#[test]
fn target_symlink_is_rejected_before_identity_comparison() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let real_target = root.path().join("real-target");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&real_target).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    symlink(&real_target, project.join("target")).unwrap();

    let provider = FakeIdentityProvider::with_boot(Some("boot-a"))
        .with_boot_error()
        .with_identity(&project, 1, 10)
        .with_identity(&project.join("target"), 1, 11);
    let review = review_project_with_identity_provider(
        &project,
        &[],
        &[],
        &[],
        SystemTime::now(),
        &options(),
        &provider,
    )
    .unwrap();

    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::TargetIdentityUnavailable)
    );
    assert_eq!(review.reviewed_identity, None);
}

#[test]
fn project_and_target_on_different_devices_are_rejected() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let provider = FakeIdentityProvider::with_boot(Some("boot-a"))
        .with_boot_error()
        .with_identity(project.path(), 1, 10)
        .with_identity(&project.path().join("target"), 2, 11);

    let review = review_project_with_identity_provider(
        project.path(),
        &[],
        &[],
        &[],
        SystemTime::now(),
        &options(),
        &provider,
    )
    .unwrap();

    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::CrossDeviceTarget)
    );
    assert_eq!(review.reviewed_identity, None);
}

#[test]
fn cleanable_review_captures_exact_identity_and_boot_session() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let provider = FakeIdentityProvider::with_boot(Some("review-boot"))
        .with_identity(project.path(), 7, 70)
        .with_identity(&project.path().join("target"), 7, 71);

    let review = review_project_with_identity_provider(
        project.path(),
        &[],
        &[],
        &[],
        SystemTime::now(),
        &options(),
        &provider,
    )
    .unwrap();

    assert_eq!(review.decision, CleanDecision::Cleanable);
    assert_eq!(
        review.reviewed_identity,
        Some(ReviewedIdentity {
            project: identity(7, 70),
            target: identity(7, 71),
            boot_session: Some(BootSessionId("review-boot".to_string())),
        })
    );

    let unavailable = FakeIdentityProvider::with_boot(Some("unused"))
        .with_boot_error()
        .with_identity(project.path(), 7, 70)
        .with_identity(&project.path().join("target"), 7, 71);
    let error = review_project_with_identity_provider(
        project.path(),
        &[],
        &[],
        &[],
        SystemTime::now(),
        &options(),
        &unavailable,
    )
    .unwrap_err();
    assert!(error.to_string().contains("fake boot session unavailable"));
}

#[test]
fn unavailable_boot_id_requires_exact_persisted_identity() {
    let project = Path::new("/scope/project");
    let provider = FakeIdentityProvider::with_boot(None).with_identity(project, 200, 300);
    let current_boot = provider.boot_session().unwrap();
    let current = provider.identity(project).unwrap();

    assert_eq!(
        compare_persisted(
            Some(&BootSessionId("boot-a".to_string())),
            current_boot.as_ref(),
            &current,
            &current,
        ),
        IdentityComparison::Matches
    );
    assert_eq!(
        compare_persisted(
            Some(&BootSessionId("boot-a".to_string())),
            current_boot.as_ref(),
            &identity(8, 20),
            &current,
        ),
        IdentityComparison::Replaced
    );
    assert_eq!(
        compare_persisted(None, current_boot.as_ref(), &current, &current),
        IdentityComparison::Matches
    );
    assert_eq!(
        compare_persisted(None, current_boot.as_ref(), &identity(8, 20), &current),
        IdentityComparison::Replaced
    );
}
