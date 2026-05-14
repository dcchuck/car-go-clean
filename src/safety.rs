use crate::activity::{path_is_within, ActivitySignal};

use anyhow::Result;
use serde::Serialize;
use std::fs;
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
    pub class: ProjectClass,
    pub target_path: PathBuf,
    pub target_bytes: u64,
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
                    SkipReason::ScanError => summary.scan_error += 1,
                    SkipReason::TargetReadError => summary.target_read_error += 1,
                }
            }
        }
    }

    summary
}

pub fn classify_project(path: &Path) -> ProjectClass {
    let parts = path_components(path);

    if contains_sequence(&parts, &[".bun", "install", "cache"])
        || contains_sequence(&parts, &["go", "pkg", "mod"])
        || contains_sequence(&parts, &[".cargo", "registry", "src"])
        || contains_sequence(&parts, &[".cargo", "git", "checkouts"])
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
    let class = classify_project(project);
    let target_path = project.join("target");

    if !is_direct_directory(&target_path) {
        return Ok(review(
            project,
            class,
            target_path,
            0,
            CleanDecision::Skipped(SkipReason::NoTarget),
        ));
    }

    let target_bytes = match directory_size(&target_path) {
        Ok(bytes) => bytes,
        Err(_) if opts.force => 0,
        Err(_) => {
            return Ok(review(
                project,
                class,
                target_path,
                0,
                CleanDecision::Skipped(SkipReason::TargetReadError),
            ));
        }
    };

    if !opts.force && !opts.include_managed_cache {
        match class {
            ProjectClass::ManagedCache => {
                return Ok(review(
                    project,
                    class,
                    target_path,
                    target_bytes,
                    CleanDecision::Skipped(SkipReason::ManagedCache),
                ));
            }
            ProjectClass::ContainerStorage => {
                return Ok(review(
                    project,
                    class,
                    target_path,
                    target_bytes,
                    CleanDecision::Skipped(SkipReason::ContainerStorage),
                ));
            }
            ProjectClass::Workspace => {}
        }
    }

    if !opts.force && has_related_scan_error(project, &target_path, scan_error_paths) {
        return Ok(review(
            project,
            class,
            target_path,
            target_bytes,
            CleanDecision::Skipped(SkipReason::ScanError),
        ));
    }

    if !opts.force && !opts.include_active && has_project_activity(project, activity) {
        return Ok(review(
            project,
            class,
            target_path,
            target_bytes,
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
        CleanDecision::Cleanable,
    ))
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

fn review(
    project: &Path,
    class: ProjectClass,
    target_path: PathBuf,
    target_bytes: u64,
    decision: CleanDecision,
) -> ProjectReview {
    ProjectReview {
        path: project.to_path_buf(),
        class,
        target_path,
        target_bytes,
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
        path_is_within(scan_error_path, project) || path_is_within(scan_error_path, target_path)
    })
}

fn has_project_activity(project: &Path, activity: &[ActivitySignal]) -> bool {
    activity
        .iter()
        .any(|signal| path_is_within(&signal.project_path, project))
}
