use std::fs;
use std::path::{Path, PathBuf};

use car_go_clean::scanner::{Scanner, ScannerOptions};

fn write_file(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn scan_finds_cargo_toml_and_stops_descending() {
    let root = tempfile::tempdir().unwrap();
    write_file(
        &root.path().join("proj-a/Cargo.toml"),
        "[package]\nname='a'\nversion='0.1.0'\n",
    );
    write_file(
        &root.path().join("proj-a/sub/Cargo.toml"),
        "[package]\nname='sub'\nversion='0.1.0'\n",
    );
    write_file(
        &root.path().join("deep/x/y/Cargo.toml"),
        "[package]\nname='y'\nversion='0.1.0'\n",
    );
    write_file(&root.path().join("ignored/node_modules/Cargo.toml"), "");

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec!["node_modules".to_string()],
    });

    let mut got = scanner.scan().unwrap();
    got.sort();

    assert_eq!(
        got,
        vec![root.path().join("deep/x/y"), root.path().join("proj-a")]
    );
}

#[test]
fn scan_includes_project_dirs_that_contain_cargo_toml() {
    let root = tempfile::tempdir().unwrap();
    write_file(
        &root.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    );

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![],
        project_dirs: vec![PathBuf::from(root.path())],
        excludes: vec![],
    });

    assert_eq!(scanner.scan().unwrap(), vec![root.path().to_path_buf()]);
}

#[test]
fn scan_respects_gitignore_files_in_scan_roots() {
    let root = tempfile::tempdir().unwrap();
    write_file(&root.path().join(".gitignore"), "ignored/\n");
    write_file(
        &root.path().join("kept/Cargo.toml"),
        "[package]\nname='kept'\nversion='0.1.0'\n",
    );
    write_file(
        &root.path().join("ignored/Cargo.toml"),
        "[package]\nname='ignored'\nversion='0.1.0'\n",
    );

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });

    assert_eq!(scanner.scan().unwrap(), vec![root.path().join("kept")]);
}

#[test]
fn scan_includes_cache_and_container_project_roots_when_not_excluded() {
    let root = tempfile::tempdir().unwrap();
    let bun_cache = root
        .path()
        .join(".bun/install/cache/@tauri-apps/cli@2.5.0@@@1");
    let orb_stack_cache = root.path().join(
        "OrbStack/docker/volumes/minikube/lib/docker/overlay2/layer/diff/src/index.crates.io/crate-1.0.0",
    );
    write_file(
        &bun_cache.join("Cargo.toml"),
        "[package]\nname='tauri-cli'\nversion='2.5.0'\n",
    );
    write_file(
        &orb_stack_cache.join("Cargo.toml"),
        "[package]\nname='cached-crate'\nversion='1.0.0'\n",
    );

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });

    assert_eq!(scanner.scan().unwrap(), vec![bun_cache, orb_stack_cache]);
}

#[test]
fn multi_component_excludes_skip_matching_subtrees() {
    let root = tempfile::tempdir().unwrap();
    write_file(
        &root.path().join("Library/Caches/cached-crate/Cargo.toml"),
        "[package]\nname='cached-crate'\nversion='0.1.0'\n",
    );
    write_file(
        &root.path().join("Library/Other/kept-crate/Cargo.toml"),
        "[package]\nname='kept-crate'\nversion='0.1.0'\n",
    );

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec!["Library/Caches".to_string()],
    });

    assert_eq!(
        scanner.scan().unwrap(),
        vec![root.path().join("Library/Other/kept-crate")]
    );
}

#[cfg(unix)]
#[test]
fn scan_skips_unreadable_directories_and_reports_errors() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    write_file(
        &root.path().join("kept/Cargo.toml"),
        "[package]\nname='kept'\nversion='0.1.0'\n",
    );
    let blocked = root.path().join("blocked");
    fs::create_dir_all(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec![],
    });

    let report = scanner.scan_with_errors().unwrap();

    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(report.projects, vec![root.path().join("kept")]);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].path, blocked);
    assert!(report.errors[0].message.contains("Permission denied"));
}
