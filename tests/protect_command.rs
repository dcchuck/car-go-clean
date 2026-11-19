#![cfg(unix)]

use car_go_clean::activity::{
    protect_command_claim, NoopProcessInspector, ProcessInspector, ProtectCommandInspector,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn script(dir: &Path, body: &str) -> String {
    let path = dir.join("protect");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn exit_zero_claims_and_exit_one_releases() {
    let dir = tempfile::tempdir().unwrap();
    let program = script(
        dir.path(),
        r#"case "$1" in */keep) exit 0 ;; *) exit 1 ;; esac"#,
    );

    assert!(protect_command_claim(&program, &[], Path::new("/work/keep")).is_some());
    assert!(protect_command_claim(&program, &[], Path::new("/work/free")).is_none());
}

#[test]
fn configured_arguments_come_before_the_project_path() {
    let dir = tempfile::tempdir().unwrap();
    let program = script(
        dir.path(),
        r#"[ "$1" = check ] && [ "$2" = /work/keep ] && exit 0; exit 1"#,
    );

    assert!(protect_command_claim(&program, &["check".into()], Path::new("/work/keep")).is_some());
}

#[test]
fn failures_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let project = Path::new("/work/any");

    let other_exit = script(dir.path(), "exit 3");
    assert_eq!(
        protect_command_claim(&other_exit, &[], project).as_deref(),
        Some("protect_command exited 3")
    );

    let signaled = script(dir.path(), "kill -9 $$");
    assert!(protect_command_claim(&signaled, &[], project).is_some());

    let missing = dir.path().join("missing").to_string_lossy().into_owned();
    assert!(protect_command_claim(&missing, &[], project)
        .unwrap()
        .starts_with("protect_command failed to start"));
}

#[test]
fn inspector_adds_claims_to_inner_activity() {
    let dir = tempfile::tempdir().unwrap();
    let command = vec![script(
        dir.path(),
        r#"case "$1" in */keep) exit 0 ;; *) exit 1 ;; esac"#,
    )];
    let projects = vec![PathBuf::from("/work/keep"), PathBuf::from("/work/free")];

    let signals = ProtectCommandInspector::new(&NoopProcessInspector, &command)
        .active_projects(&projects)
        .unwrap();

    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].project_path, PathBuf::from("/work/keep"));
    assert_eq!(signals[0].reason, "protect_command claimed project");
}

#[test]
fn empty_command_claims_nothing() {
    let projects = vec![PathBuf::from("/work/keep")];

    let signals = ProtectCommandInspector::new(&NoopProcessInspector, &[])
        .active_projects(&projects)
        .unwrap();

    assert!(signals.is_empty());
}
