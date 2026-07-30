use crate::activity::{path_is_within, ActivitySignal};
use crate::identity::{
    compare_persisted, BootSessionId, FilesystemIdentity, IdentityComparison, IdentityProvider,
    ReviewedIdentity, SystemIdentityProvider,
};
use crate::policy::{ProtectedRootKind, ScopePolicy};
use crate::storage::{classify_protected_path_for, current_home_dir, HostPlatform, ProtectedKind};

use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectClass {
    Workspace,
    ManagedCache,
    ContainerStorage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanDecision {
    Cleanable,
    Skipped(SkipReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    NoTarget,
    ActiveRecentWrite { newest_age_secs: u64 },
    ActiveProcess,
    ManagedCache,
    ContainerStorage,
    ScanError,
    TargetReadError,
    InvalidManifest,
    ProjectIdentityUnavailable,
    TargetIdentityUnavailable,
    CrossDeviceTarget,
    CrossMountTarget,
    ProjectIdentityChanged,
    TargetIdentityChanged,
    OutOfScope,
    Excluded,
}

#[derive(Debug, Clone, Copy)]
pub struct SafetyOptions {
    pub target_quiet_period: Duration,
    pub include_managed_cache: bool,
    pub include_active: bool,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectReview {
    pub path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub class: ProjectClass,
    pub target_path: PathBuf,
    pub target_bytes: u64,
    pub reviewed_identity: Option<ReviewedIdentity>,
    pub decision: CleanDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewSummary {
    pub total_projects: usize,
    pub cleanable_projects: usize,
    pub skipped_projects: usize,
    pub cleanable_bytes: u64,
    pub active_recent_write: usize,
    pub active_process: usize,
    pub managed_cache: usize,
    pub container_storage: usize,
    pub scan_error: usize,
    pub no_target: usize,
    pub target_read_error: usize,
}

pub fn review_summary(reviews: &[ProjectReview]) -> ReviewSummary {
    let mut summary = ReviewSummary {
        total_projects: reviews.len(),
        cleanable_projects: 0,
        skipped_projects: 0,
        cleanable_bytes: 0,
        active_recent_write: 0,
        active_process: 0,
        managed_cache: 0,
        container_storage: 0,
        scan_error: 0,
        no_target: 0,
        target_read_error: 0,
    };

    for review in reviews {
        match &review.decision {
            CleanDecision::Cleanable => {
                summary.cleanable_projects += 1;
                summary.cleanable_bytes += review.target_bytes;
            }
            CleanDecision::Skipped(reason) => {
                summary.skipped_projects += 1;
                match reason {
                    SkipReason::NoTarget => summary.no_target += 1,
                    SkipReason::ActiveRecentWrite { .. } => summary.active_recent_write += 1,
                    SkipReason::ActiveProcess => summary.active_process += 1,
                    SkipReason::ManagedCache => summary.managed_cache += 1,
                    SkipReason::ContainerStorage => summary.container_storage += 1,
                    SkipReason::ScanError
                    | SkipReason::InvalidManifest
                    | SkipReason::ProjectIdentityUnavailable
                    | SkipReason::ProjectIdentityChanged
                    | SkipReason::OutOfScope
                    | SkipReason::Excluded => summary.scan_error += 1,
                    SkipReason::TargetReadError
                    | SkipReason::TargetIdentityUnavailable
                    | SkipReason::CrossDeviceTarget
                    | SkipReason::CrossMountTarget
                    | SkipReason::TargetIdentityChanged => summary.target_read_error += 1,
                }
            }
        }
    }

    summary
}

pub fn classify_project(path: &Path) -> ProjectClass {
    let protected = classify_protected_path_for(path, &current_home_dir(), HostPlatform::current());
    match protected {
        Some(ProtectedKind::ManagedCache) => ProjectClass::ManagedCache,
        Some(ProtectedKind::ContainerStorage) => ProjectClass::ContainerStorage,
        None => classify_legacy_component_patterns(path),
    }
}

fn classify_legacy_component_patterns(path: &Path) -> ProjectClass {
    let parts = path_components(path);

    if contains_sequence(&parts, &[".bun", "install", "cache"])
        || contains_sequence(&parts, &["go", "pkg", "mod"])
        || contains_sequence(&parts, &[".cargo", "registry", "src"])
        || contains_sequence(&parts, &[".cargo", "git", "checkouts"])
        || contains_sequence(&parts, &["Library", "Caches"])
    {
        ProjectClass::ManagedCache
    } else if contains_sequence(&parts, &["OrbStack", "docker"]) {
        ProjectClass::ContainerStorage
    } else {
        ProjectClass::Workspace
    }
}

pub fn review_project(
    project: &Path,
    scan_error_paths: &[PathBuf],
    activity: &[ActivitySignal],
    now: SystemTime,
    opts: &SafetyOptions,
) -> Result<ProjectReview> {
    review_project_with_discovery_blocks(project, scan_error_paths, &[], activity, now, opts)
}

pub fn review_project_with_discovery_blocks(
    project: &Path,
    scan_error_paths: &[PathBuf],
    discovery_blocked_paths: &[PathBuf],
    activity: &[ActivitySignal],
    now: SystemTime,
    opts: &SafetyOptions,
) -> Result<ProjectReview> {
    review_project_with_identity_provider(
        project,
        scan_error_paths,
        discovery_blocked_paths,
        activity,
        now,
        opts,
        &SystemIdentityProvider,
    )
}

#[doc(hidden)]
pub fn review_project_with_identity_provider(
    project: &Path,
    scan_error_paths: &[PathBuf],
    discovery_blocked_paths: &[PathBuf],
    activity: &[ActivitySignal],
    now: SystemTime,
    opts: &SafetyOptions,
    identity_provider: &dyn IdentityProvider,
) -> Result<ProjectReview> {
    let class = classify_project(project);
    let target_path = project.join("target");

    if !is_direct_regular_file(&project.join("Cargo.toml")) {
        return Ok(review(
            project,
            class,
            target_path,
            0,
            None,
            CleanDecision::Skipped(SkipReason::InvalidManifest),
        ));
    }

    if !is_direct_directory(project) {
        return Ok(review(
            project,
            class,
            target_path,
            0,
            None,
            CleanDecision::Skipped(SkipReason::ProjectIdentityUnavailable),
        ));
    }
    let project_identity = match identity_provider.identity(project) {
        Ok(identity) => identity,
        Err(_) => {
            return Ok(review(
                project,
                class,
                target_path,
                0,
                None,
                CleanDecision::Skipped(SkipReason::ProjectIdentityUnavailable),
            ));
        }
    };

    match fs::symlink_metadata(&target_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(review(
                project,
                class,
                target_path,
                0,
                None,
                CleanDecision::Skipped(SkipReason::NoTarget),
            ));
        }
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) | Err(_) => {
            return Ok(review(
                project,
                class,
                target_path,
                0,
                None,
                CleanDecision::Skipped(SkipReason::TargetIdentityUnavailable),
            ));
        }
    }
    let target_identity = match identity_provider.identity(&target_path) {
        Ok(identity) => identity,
        Err(_) => {
            return Ok(review(
                project,
                class,
                target_path,
                0,
                None,
                CleanDecision::Skipped(SkipReason::TargetIdentityUnavailable),
            ));
        }
    };
    if project_identity.device != target_identity.device {
        return Ok(review(
            project,
            class,
            target_path,
            0,
            None,
            CleanDecision::Skipped(SkipReason::CrossDeviceTarget),
        ));
    }
    if project_identity.mount != target_identity.mount {
        return Ok(review(
            project,
            class,
            target_path,
            0,
            None,
            CleanDecision::Skipped(SkipReason::CrossMountTarget),
        ));
    }

    let reviewed_identity = Some(ReviewedIdentity {
        project: project_identity,
        target: target_identity,
        boot_session: identity_provider.boot_session()?,
    });

    let target_bytes = match directory_size(&target_path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(review(
                project,
                class,
                target_path,
                0,
                reviewed_identity.clone(),
                CleanDecision::Skipped(SkipReason::TargetReadError),
            ));
        }
    };

    if !opts.include_managed_cache {
        match class {
            ProjectClass::ManagedCache => {
                return Ok(review(
                    project,
                    class,
                    target_path,
                    target_bytes,
                    reviewed_identity.clone(),
                    CleanDecision::Skipped(SkipReason::ManagedCache),
                ));
            }
            ProjectClass::ContainerStorage => {
                return Ok(review(
                    project,
                    class,
                    target_path,
                    target_bytes,
                    reviewed_identity.clone(),
                    CleanDecision::Skipped(SkipReason::ContainerStorage),
                ));
            }
            ProjectClass::Workspace => {}
        }
    }

    if !opts.force
        && (has_related_scan_error(project, &target_path, scan_error_paths)
            || has_exact_discovery_block(project, discovery_blocked_paths))
    {
        return Ok(review(
            project,
            class,
            target_path,
            target_bytes,
            reviewed_identity.clone(),
            CleanDecision::Skipped(SkipReason::ScanError),
        ));
    }

    if !opts.force && !opts.include_active && has_project_activity(project, activity) {
        return Ok(review(
            project,
            class,
            target_path,
            target_bytes,
            reviewed_identity.clone(),
            CleanDecision::Skipped(SkipReason::ActiveProcess),
        ));
    }

    if !opts.force {
        let newest_mtime = match newest_file_mtime(&target_path) {
            Ok(mtime) => mtime,
            Err(_) => {
                return Ok(review(
                    project,
                    class,
                    target_path,
                    target_bytes,
                    reviewed_identity.clone(),
                    CleanDecision::Skipped(SkipReason::TargetReadError),
                ));
            }
        };

        if let Some(mtime) = newest_mtime {
            let newest_age = now.duration_since(mtime).unwrap_or_default();
            if newest_age < opts.target_quiet_period {
                return Ok(review(
                    project,
                    class,
                    target_path,
                    target_bytes,
                    reviewed_identity.clone(),
                    CleanDecision::Skipped(SkipReason::ActiveRecentWrite {
                        newest_age_secs: newest_age.as_secs(),
                    }),
                ));
            }
        }
    }

    Ok(review(
        project,
        class,
        target_path,
        target_bytes,
        reviewed_identity,
        CleanDecision::Cleanable,
    ))
}

pub type ExecutionDecision = CleanDecision;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationIdentityStatus {
    Current,
    ReverifiedAcrossBoot,
    Rejected,
}

pub fn bind_review_to_observation(
    review: &mut ProjectReview,
    observed_project: &FilesystemIdentity,
    observed_target: Option<&FilesystemIdentity>,
    observed_boot: Option<&BootSessionId>,
) -> ObservationIdentityStatus {
    if review.decision != CleanDecision::Cleanable {
        return ObservationIdentityStatus::Rejected;
    }
    let Some(current) = review.reviewed_identity.as_ref() else {
        review.decision = CleanDecision::Skipped(SkipReason::ProjectIdentityUnavailable);
        return ObservationIdentityStatus::Rejected;
    };
    if review.canonical_path.as_deref() != Some(review.path.as_path()) {
        review.decision = CleanDecision::Skipped(SkipReason::ProjectIdentityChanged);
        return ObservationIdentityStatus::Rejected;
    }

    let project_comparison = compare_persisted(
        observed_boot,
        current.boot_session.as_ref(),
        observed_project,
        &current.project,
    );
    if project_comparison == IdentityComparison::Replaced {
        review.decision = CleanDecision::Skipped(SkipReason::ProjectIdentityChanged);
        return ObservationIdentityStatus::Rejected;
    }

    let target_comparison = match observed_target {
        Some(observed_target) => compare_persisted(
            observed_boot,
            current.boot_session.as_ref(),
            observed_target,
            &current.target,
        ),
        None if project_comparison == IdentityComparison::StaleAcrossBoot => {
            IdentityComparison::StaleAcrossBoot
        }
        None => IdentityComparison::Replaced,
    };
    if target_comparison == IdentityComparison::Replaced {
        review.decision = CleanDecision::Skipped(SkipReason::TargetIdentityChanged);
        return ObservationIdentityStatus::Rejected;
    }

    if project_comparison == IdentityComparison::StaleAcrossBoot
        || target_comparison == IdentityComparison::StaleAcrossBoot
    {
        ObservationIdentityStatus::ReverifiedAcrossBoot
    } else {
        ObservationIdentityStatus::Current
    }
}

#[allow(clippy::too_many_arguments)]
pub fn revalidate_before_clean(
    review: &ProjectReview,
    policy: &ScopePolicy,
    identity_provider: &dyn IdentityProvider,
    activity: &[ActivitySignal],
    scan_error_paths: &[PathBuf],
    discovery_blocked_paths: &[PathBuf],
    now: SystemTime,
    opts: &SafetyOptions,
) -> Result<ExecutionDecision> {
    if review.decision != CleanDecision::Cleanable {
        return Ok(review.decision.clone());
    }
    let Some(reviewed_identity) = review.reviewed_identity.as_ref() else {
        return Ok(CleanDecision::Skipped(
            SkipReason::ProjectIdentityUnavailable,
        ));
    };

    if let Some(reason) = policy_block_reason(policy, review, opts) {
        return Ok(CleanDecision::Skipped(reason));
    }

    let refreshed = review_project_with_identity_provider(
        &review.path,
        scan_error_paths,
        discovery_blocked_paths,
        activity,
        now,
        opts,
        identity_provider,
    )?;
    if refreshed.decision != CleanDecision::Cleanable {
        return Ok(refreshed.decision);
    }
    let Some(refreshed_identity) = refreshed.reviewed_identity.as_ref() else {
        return Ok(CleanDecision::Skipped(
            SkipReason::ProjectIdentityUnavailable,
        ));
    };
    if refreshed.canonical_path != review.canonical_path
        || refreshed.canonical_path.as_deref() != Some(review.path.as_path())
    {
        return Ok(CleanDecision::Skipped(SkipReason::ProjectIdentityChanged));
    }
    if refreshed_identity.project != reviewed_identity.project {
        return Ok(CleanDecision::Skipped(SkipReason::ProjectIdentityChanged));
    }
    if refreshed_identity.target != reviewed_identity.target {
        return Ok(CleanDecision::Skipped(SkipReason::TargetIdentityChanged));
    }

    if !is_direct_regular_file(&review.path.join("Cargo.toml"))
        || !is_direct_directory(&review.path)
    {
        return Ok(CleanDecision::Skipped(
            SkipReason::ProjectIdentityUnavailable,
        ));
    }
    if !is_direct_directory(&review.target_path) {
        return Ok(CleanDecision::Skipped(
            SkipReason::TargetIdentityUnavailable,
        ));
    }
    let project_identity = match identity_provider.identity(&review.path) {
        Ok(identity) => identity,
        Err(_) => {
            return Ok(CleanDecision::Skipped(
                SkipReason::ProjectIdentityUnavailable,
            ));
        }
    };
    let target_identity = match identity_provider.identity(&review.target_path) {
        Ok(identity) => identity,
        Err(_) => {
            return Ok(CleanDecision::Skipped(
                SkipReason::TargetIdentityUnavailable,
            ));
        }
    };
    if project_identity.device != target_identity.device {
        return Ok(CleanDecision::Skipped(SkipReason::CrossDeviceTarget));
    }
    if project_identity.mount != target_identity.mount {
        return Ok(CleanDecision::Skipped(SkipReason::CrossMountTarget));
    }
    if project_identity != reviewed_identity.project {
        return Ok(CleanDecision::Skipped(SkipReason::ProjectIdentityChanged));
    }
    if target_identity != reviewed_identity.target {
        return Ok(CleanDecision::Skipped(SkipReason::TargetIdentityChanged));
    }
    if let Some(reason) = policy_block_reason(policy, review, opts) {
        return Ok(CleanDecision::Skipped(reason));
    }
    let final_canonical_project = match fs::canonicalize(&review.path) {
        Ok(path) => path,
        Err(_) => {
            return Ok(CleanDecision::Skipped(
                SkipReason::ProjectIdentityUnavailable,
            ));
        }
    };
    if final_canonical_project != review.path {
        return Ok(CleanDecision::Skipped(SkipReason::ProjectIdentityChanged));
    }
    let final_canonical_target = match fs::canonicalize(&review.target_path) {
        Ok(path) => path,
        Err(_) => {
            return Ok(CleanDecision::Skipped(
                SkipReason::TargetIdentityUnavailable,
            ));
        }
    };
    if !policy.contains_project(&final_canonical_project) {
        return Ok(CleanDecision::Skipped(SkipReason::OutOfScope));
    }
    if policy.is_excluded(&final_canonical_project) || policy.is_excluded(&final_canonical_target) {
        return Ok(CleanDecision::Skipped(SkipReason::Excluded));
    }

    Ok(CleanDecision::Cleanable)
}

fn policy_block_reason(
    policy: &ScopePolicy,
    review: &ProjectReview,
    opts: &SafetyOptions,
) -> Option<SkipReason> {
    let Some(canonical_project) = review.canonical_path.as_deref() else {
        return Some(SkipReason::ProjectIdentityUnavailable);
    };
    if canonical_project != review.path {
        return Some(SkipReason::ProjectIdentityChanged);
    }
    let canonical_target = fs::canonicalize(&review.target_path).ok();
    if !policy.contains_project(canonical_project) {
        return Some(SkipReason::OutOfScope);
    }
    if policy.is_excluded(canonical_project)
        || canonical_target
            .as_deref()
            .is_some_and(|target| policy.is_excluded(target))
    {
        return Some(SkipReason::Excluded);
    }
    if !opts.include_managed_cache {
        return policy_protected_class(policy, canonical_project).map(|class| match class {
            ProjectClass::ManagedCache => SkipReason::ManagedCache,
            ProjectClass::ContainerStorage => SkipReason::ContainerStorage,
            ProjectClass::Workspace => unreachable!("protected roots are not workspaces"),
        });
    }
    None
}

fn policy_protected_class(policy: &ScopePolicy, path: &Path) -> Option<ProjectClass> {
    let physical_path = fs::canonicalize(path).ok();
    policy
        .diagnostics()
        .protected_roots
        .iter()
        .find(|root| {
            path == root.path
                || path.starts_with(&root.path)
                || physical_path.as_deref().is_some_and(|physical| {
                    physical == root.path
                        || physical.starts_with(&root.path)
                        || fs::canonicalize(&root.path)
                            .ok()
                            .is_some_and(|physical_root| {
                                physical == physical_root || physical.starts_with(physical_root)
                            })
                })
        })
        .map(|root| match root.kind {
            ProtectedRootKind::Container => ProjectClass::ContainerStorage,
            ProtectedRootKind::Cargo
            | ProtectedRootKind::Rustup
            | ProtectedRootKind::GoModule
            | ProtectedRootKind::Bun
            | ProtectedRootKind::ManagedCache => ProjectClass::ManagedCache,
        })
}

fn path_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn contains_sequence(parts: &[String], needle: &[&str]) -> bool {
    parts.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(part, needle)| part == needle)
    })
}

fn is_direct_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

fn is_direct_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn review(
    project: &Path,
    class: ProjectClass,
    target_path: PathBuf,
    target_bytes: u64,
    reviewed_identity: Option<ReviewedIdentity>,
    decision: CleanDecision,
) -> ProjectReview {
    let canonical_path = fs::canonicalize(project).ok();
    let decision = if canonical_path.is_none() && decision == CleanDecision::Cleanable {
        CleanDecision::Skipped(SkipReason::ProjectIdentityUnavailable)
    } else {
        decision
    };
    ProjectReview {
        path: project.to_path_buf(),
        canonical_path,
        class,
        target_path,
        target_bytes,
        reviewed_identity,
        decision,
    }
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut total = 0;

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;

        if metadata.file_type().is_symlink() {
            continue;
        }

        if metadata.is_dir() {
            total += directory_size(&entry.path())?;
        } else if metadata.is_file() {
            total += metadata.len();
        }
    }

    Ok(total)
}

fn newest_file_mtime(path: &Path) -> Result<Option<SystemTime>> {
    let mut newest = None;

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;

        if metadata.file_type().is_symlink() {
            continue;
        }

        if metadata.is_dir() {
            newest = newest_time(newest, newest_file_mtime(&entry.path())?);
        } else if metadata.is_file() {
            newest = newest_time(newest, Some(metadata.modified()?));
        }
    }

    Ok(newest)
}

fn newest_time(left: Option<SystemTime>, right: Option<SystemTime>) -> Option<SystemTime> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn has_related_scan_error(
    project: &Path,
    target_path: &Path,
    scan_error_paths: &[PathBuf],
) -> bool {
    scan_error_paths.iter().any(|scan_error_path| {
        path_is_within(scan_error_path, project)
            || path_is_within(project, scan_error_path)
            || path_is_within(scan_error_path, target_path)
            || path_is_within(target_path, scan_error_path)
    })
}

fn has_exact_discovery_block(project: &Path, discovery_blocked_paths: &[PathBuf]) -> bool {
    if discovery_blocked_paths
        .iter()
        .any(|blocked| blocked == project)
    {
        return true;
    }
    let Ok(canonical_project) = fs::canonicalize(project) else {
        return false;
    };
    discovery_blocked_paths
        .iter()
        .any(|blocked| blocked == &canonical_project)
}

fn has_project_activity(project: &Path, activity: &[ActivitySignal]) -> bool {
    activity
        .iter()
        .any(|signal| path_is_within(&signal.project_path, project))
}
