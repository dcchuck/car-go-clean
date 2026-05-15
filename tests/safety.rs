use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use car_go_clean::activity::{
    activity_signals_for_process, path_is_within, process_matches_project, ActivitySignal,
};
use car_go_clean::safety::{
    classify_project, review_project, review_summary, CleanDecision, ProjectClass, ReviewSummary,
    SafetyOptions, SkipReason,
};

fn write_file(path: &Path, body: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn options() -> SafetyOptions {
    SafetyOptions {
        target_quiet_period: Duration::from_secs(2 * 60 * 60),
        include_managed_cache: false,
        include_active: false,
        force: false,
    }
}

#[test]
fn classifies_known_managed_cache_paths() {
    assert_eq!(
        classify_project(Path::new(
            "/Users/me/.bun/install/cache/pkg/node_modules/crate"
        )),
        ProjectClass::ManagedCache
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/go/pkg/mod/example.com/crate")),
        ProjectClass::ManagedCache
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/.cargo/registry/src/index/crate")),
        ProjectClass::ManagedCache
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/OrbStack/docker/containers/crate")),
        ProjectClass::ContainerStorage
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/src/workspace/crate")),
        ProjectClass::Workspace
    );
}

#[test]
fn similar_looking_paths_remain_workspaces() {
    assert_eq!(
        classify_project(Path::new("/Users/me/src/go/pkg/model/app")),
        ProjectClass::Workspace
    );
    assert_eq!(
        classify_project(Path::new("/tmp/my.cargo/registry/src-demo/app")),
        ProjectClass::Workspace
    );
    assert_eq!(
        classify_project(Path::new("/Users/me/src/docker/containers/app")),
        ProjectClass::Workspace
    );
}

#[test]
fn path_matching_treats_project_and_target_descendants_as_active() {
    let project = Path::new("/Users/me/src/app");
    assert!(path_is_within(Path::new("/Users/me/src/app"), project));
    assert!(path_is_within(
        Path::new("/Users/me/src/app/target/debug/app"),
        project
    ));
    assert!(!path_is_within(
        Path::new("/Users/me/src/application"),
        project
    ));
}

#[test]
fn process_command_arguments_can_match_project_or_target_paths() {
    let project = Path::new("/Users/me/src/app");
    let args = vec![
        PathBuf::from("/Users/me/.cargo/bin/cargo"),
        PathBuf::from("/Users/me/src/app"),
    ];

    assert!(process_matches_project(
        Some(Path::new("/tmp")),
        &args,
        project
    ));

    let args = vec![PathBuf::from("/Users/me/src/app/target/debug/server")];
    assert!(process_matches_project(None, &args, project));

    let args = vec![PathBuf::from("/Users/me/src/application")];
    assert!(!process_matches_project(None, &args, project));
}

#[test]
fn process_activity_marks_every_matching_nested_project() {
    let repo = tempfile::tempdir().unwrap();
    let member = repo.path().join("member");
    let projects = vec![repo.path().to_path_buf(), member.clone()];

    let signals = activity_signals_for_process(42, Some(&member), &[], &projects);
    let active_projects: Vec<_> = signals
        .iter()
        .map(|signal| signal.project_path.as_path())
        .collect();

    assert_eq!(signals.len(), 2);
    assert!(active_projects.contains(&repo.path()));
    assert!(active_projects.contains(&member.as_path()));
}

#[test]
fn missing_direct_target_is_skipped_even_with_force() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");

    let mut opts = options();
    opts.force = true;
    let review = review_project(project.path(), &[], &[], SystemTime::now(), &opts).unwrap();

    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::NoTarget)
    );
}

#[cfg(unix)]
#[test]
fn symlinked_target_is_skipped_as_missing_target() {
    use std::os::unix::fs::symlink;

    let project = tempfile::tempdir().unwrap();
    let real_target = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&real_target.path().join("debug/blob.bin"), &[0; 4096]);
    symlink(real_target.path(), project.path().join("target")).unwrap();

    let mut opts = options();
    opts.force = true;
    let review = review_project(project.path(), &[], &[], SystemTime::now(), &opts).unwrap();

    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::NoTarget)
    );
}

#[test]
fn old_direct_target_is_cleanable() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);

    let now = SystemTime::now() + Duration::from_secs(3 * 60 * 60);
    let review = review_project(project.path(), &[], &[], now, &options()).unwrap();

    assert_eq!(review.decision, CleanDecision::Cleanable);
    assert!(review.target_bytes >= 4096);
}

#[test]
fn recent_target_write_is_skipped() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);

    let review = review_project(project.path(), &[], &[], SystemTime::now(), &options()).unwrap();

    assert!(matches!(
        review.decision,
        CleanDecision::Skipped(SkipReason::ActiveRecentWrite { .. })
    ));
}

#[test]
fn related_scan_error_is_skipped_but_unrelated_error_is_not() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);
    let now = SystemTime::now() + Duration::from_secs(3 * 60 * 60);

    let unrelated = vec![PathBuf::from(
        "/Users/me/Pictures/Photos Library.photoslibrary",
    )];
    let review = review_project(project.path(), &unrelated, &[], now, &options()).unwrap();
    assert_eq!(review.decision, CleanDecision::Cleanable);

    let related = vec![project.path().join("target/debug")];
    let review = review_project(project.path(), &related, &[], now, &options()).unwrap();
    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::ScanError)
    );
}

#[test]
fn active_process_is_skipped_unless_included() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);
    let now = SystemTime::now() + Duration::from_secs(3 * 60 * 60);
    let signal = ActivitySignal {
        pid: 123,
        project_path: project.path().to_path_buf(),
        reason: "cwd inside project".to_string(),
    };

    let review = review_project(
        project.path(),
        &[],
        std::slice::from_ref(&signal),
        now,
        &options(),
    )
    .unwrap();
    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::ActiveProcess)
    );

    let mut opts = options();
    opts.include_active = true;
    let review = review_project(project.path(), &[], &[signal], now, &opts).unwrap();
    assert_eq!(review.decision, CleanDecision::Cleanable);
}

#[test]
fn review_summary_counts_cleanable_and_skip_reasons() {
    let project = tempfile::tempdir().unwrap();
    write_file(&project.path().join("Cargo.toml"), b"[package]\n");
    write_file(&project.path().join("target/debug/blob.bin"), &[0; 4096]);
    let old = SystemTime::now() + Duration::from_secs(3 * 60 * 60);
    let cleanable = review_project(project.path(), &[], &[], old, &options()).unwrap();

    let missing = tempfile::tempdir().unwrap();
    let skipped = review_project(missing.path(), &[], &[], old, &options()).unwrap();

    let summary = review_summary(&[cleanable, skipped]);

    assert_eq!(
        summary,
        ReviewSummary {
            total_projects: 2,
            cleanable_projects: 1,
            skipped_projects: 1,
            cleanable_bytes: 4096,
            active_recent_write: 0,
            active_process: 0,
            managed_cache: 0,
            container_storage: 0,
            scan_error: 0,
            no_target: 1,
            target_read_error: 0,
        }
    );
}
