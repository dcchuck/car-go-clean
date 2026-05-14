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
