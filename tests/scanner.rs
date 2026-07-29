use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use car_go_clean::safety::{
    review_project, CleanDecision, ProjectClass, SafetyOptions, SkipReason,
};
use car_go_clean::scanner::{
    GitWorktreeError, GitWorktreeResolver, Scanner, ScannerOptions, WorktreeDiscovery,
};

#[derive(Clone)]
struct FakeResolver {
    result: Result<Vec<PathBuf>, String>,
    calls: Arc<Mutex<Vec<PathBuf>>>,
}

impl FakeResolver {
    fn paths(paths: Vec<PathBuf>) -> Self {
        Self {
            result: Ok(paths),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn failure(message: &str) -> Self {
        Self {
            result: Err(message.to_string()),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<PathBuf> {
        self.calls.lock().unwrap().clone()
    }
}

impl GitWorktreeResolver for FakeResolver {
    fn linked_worktrees(&self, primary: &Path) -> Result<Vec<PathBuf>, GitWorktreeError> {
        self.calls.lock().unwrap().push(primary.to_path_buf());
        self.result.clone().map_err(GitWorktreeError::new)
    }
}

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
        vec![
            root.path().join("deep/x/y").canonicalize().unwrap(),
            root.path().join("proj-a").canonicalize().unwrap(),
        ]
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

    assert_eq!(
        scanner.scan().unwrap(),
        vec![root.path().canonicalize().unwrap()]
    );
}

#[test]
fn configured_project_dir_does_not_override_exclusions() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("explicit-project");
    write_file(&project.join("Cargo.toml"), "[package]\n");

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![],
        project_dirs: vec![project.clone()],
        excludes: vec!["explicit-project".to_string()],
    });

    assert!(scanner.scan().unwrap().is_empty());
}

#[test]
fn configured_non_projects_remain_silently_ignored() {
    let root = tempfile::tempdir().unwrap();
    let non_project = root.path().join("non-project");
    fs::create_dir_all(&non_project).unwrap();
    let missing = root.path().join("missing");

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![],
        project_dirs: vec![non_project, missing],
        excludes: vec![],
    });

    let report = scanner.scan_with_errors().unwrap();
    assert!(report.projects.is_empty());
    assert!(report.errors.is_empty());
    assert!(report.worktree_discoveries.is_empty());
}

#[test]
fn configured_primary_project_discovers_ignored_linked_worktrees() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = primary.join(".worktrees/feature");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");
    write_file(&primary.join(".gitignore"), ".worktrees/\n");
    write_file(&linked.join("Cargo.toml"), "[workspace]\n");
    let resolver = FakeResolver::paths(vec![linked.clone()]);

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![],
            project_dirs: vec![primary.clone()],
            excludes: vec![],
        },
        Arc::new(resolver.clone()),
    );

    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();
    let report = scanner.scan_with_errors().unwrap();
    assert_eq!(
        report.projects,
        vec![canonical_primary.clone(), canonical_linked.clone()]
    );
    assert_eq!(resolver.calls(), vec![canonical_primary.clone()]);
    assert_eq!(
        report.worktree_discoveries,
        vec![WorktreeDiscovery::Success {
            primary: canonical_primary,
            linked: vec![canonical_linked],
            excluded: vec![],
            out_of_scope: vec![],
        }]
    );
}

#[test]
fn configured_linked_checkout_does_not_query_git_as_a_primary() {
    let root = tempfile::tempdir().unwrap();
    let linked = root.path().join("feature");
    write_file(&linked.join("Cargo.toml"), "[workspace]\n");
    write_file(
        &linked.join(".git"),
        "gitdir: ../router/.git/worktrees/feature\n",
    );
    let resolver = FakeResolver::paths(vec![]);

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![],
            project_dirs: vec![linked.clone()],
            excludes: vec![],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();
    assert_eq!(report.projects, vec![linked.canonicalize().unwrap()]);
    assert!(report.worktree_discoveries.is_empty());
    assert!(resolver.calls().is_empty());
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

    assert_eq!(
        scanner.scan().unwrap(),
        vec![root.path().join("kept").canonicalize().unwrap()]
    );
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

    assert_eq!(
        scanner.scan().unwrap(),
        vec![
            bun_cache.canonicalize().unwrap(),
            orb_stack_cache.canonicalize().unwrap()
        ]
    );
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
    write_file(
        &root.path().join("Library/CachesExtra/also-kept/Cargo.toml"),
        "[package]\nname='also-kept'\nversion='0.1.0'\n",
    );

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![root.path().to_path_buf()],
        project_dirs: vec![],
        excludes: vec!["Library/Caches".to_string()],
    });

    assert_eq!(
        scanner.scan().unwrap(),
        vec![
            root.path()
                .join("Library/CachesExtra/also-kept")
                .canonicalize()
                .unwrap(),
            root.path()
                .join("Library/Other/kept-crate")
                .canonicalize()
                .unwrap(),
        ]
    );
}

#[cfg(unix)]
#[test]
fn scan_skips_unreadable_directories_and_reports_errors() {
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let alias_parent = tempfile::tempdir().unwrap();
    let alias = alias_parent.path().join("scan-root-alias");
    symlink(root.path(), &alias).unwrap();
    write_file(
        &root.path().join("kept/Cargo.toml"),
        "[package]\nname='kept'\nversion='0.1.0'\n",
    );
    let blocked = root.path().join("blocked");
    fs::create_dir_all(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![alias],
        project_dirs: vec![],
        excludes: vec![],
    });

    let report = scanner.scan_with_errors().unwrap();

    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        report.projects,
        vec![root.path().join("kept").canonicalize().unwrap()]
    );
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].path, blocked.canonicalize().unwrap());
    assert!(report.errors[0].message.contains("Permission denied"));
}

#[test]
fn scan_discovers_ignored_in_scope_linked_worktree_once() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = primary.join(".worktrees/feature");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");
    write_file(&primary.join(".gitignore"), ".worktrees/\n");
    write_file(&linked.join("Cargo.toml"), "[workspace]\n");
    let resolver = FakeResolver::paths(vec![linked.clone(), linked.clone()]);

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_linked = linked.canonicalize().unwrap();
    assert_eq!(
        report.projects,
        vec![canonical_primary.clone(), canonical_linked.clone()]
    );
    assert_eq!(resolver.calls(), vec![canonical_primary.clone()]);
    assert_eq!(
        report.worktree_discoveries,
        vec![WorktreeDiscovery::Success {
            primary: canonical_primary,
            linked: vec![canonical_linked],
            excluded: vec![],
            out_of_scope: vec![],
        }]
    );
}

#[test]
fn scan_rejects_linked_worktree_outside_canonical_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");
    write_file(&outside.path().join("Cargo.toml"), "[workspace]\n");

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        },
        Arc::new(FakeResolver::paths(vec![outside.path().to_path_buf()])),
    );

    let report = scanner.scan_with_errors().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_outside = outside.path().canonicalize().unwrap();
    assert_eq!(report.projects, vec![canonical_primary.clone()]);
    assert_eq!(
        report.worktree_discoveries,
        vec![WorktreeDiscovery::Success {
            primary: canonical_primary,
            linked: vec![],
            excluded: vec![],
            out_of_scope: vec![canonical_outside],
        }]
    );
}

#[test]
fn scan_rejects_configured_excluded_linked_worktree() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = primary.join(".worktrees/excluded");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");
    write_file(&linked.join("Cargo.toml"), "[workspace]\n");

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec!["excluded".to_string()],
        },
        Arc::new(FakeResolver::paths(vec![linked.clone()])),
    );

    let report = scanner.scan_with_errors().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    assert_eq!(report.projects, vec![canonical_primary.clone()]);
    assert_eq!(
        report.worktree_discoveries,
        vec![WorktreeDiscovery::Success {
            primary: canonical_primary,
            linked: vec![],
            excluded: vec![linked],
            out_of_scope: vec![],
        }]
    );
}

#[cfg(unix)]
#[test]
fn scan_rejects_git_candidates_beneath_a_multi_component_exclusion_after_canonicalization() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let excluded = root.path().join("Library/Caches/team/worktree");
    let alias = root.path().join("worktree-alias");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");
    write_file(&excluded.join("Cargo.toml"), "[workspace]\n");
    symlink(&excluded, &alias).unwrap();
    let resolver = FakeResolver::paths(vec![excluded.clone(), alias]);

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec!["Library/Caches".to_string()],
        },
        Arc::new(resolver),
    );

    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_excluded = excluded.canonicalize().unwrap();
    let report = scanner.scan_with_errors().unwrap();

    assert_eq!(report.projects, vec![canonical_primary.clone()]);
    assert_eq!(
        report.worktree_discoveries,
        vec![WorktreeDiscovery::Success {
            primary: canonical_primary,
            linked: vec![],
            excluded: vec![canonical_excluded, excluded],
            out_of_scope: vec![],
        }]
    );
}

#[cfg(unix)]
#[test]
fn scan_rejects_linked_worktree_symlink_that_resolves_outside_root() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked_symlink = primary.join(".worktrees/feature");
    fs::create_dir_all(primary.join(".git")).unwrap();
    fs::create_dir_all(linked_symlink.parent().unwrap()).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");
    write_file(&outside.path().join("Cargo.toml"), "[workspace]\n");
    symlink(outside.path(), &linked_symlink).unwrap();

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        },
        Arc::new(FakeResolver::paths(vec![linked_symlink])),
    );

    let report = scanner.scan_with_errors().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    let canonical_outside = outside.path().canonicalize().unwrap();
    assert_eq!(report.projects, vec![canonical_primary.clone()]);
    assert_eq!(
        report.worktree_discoveries,
        vec![WorktreeDiscovery::Success {
            primary: canonical_primary,
            linked: vec![],
            excluded: vec![],
            out_of_scope: vec![canonical_outside],
        }]
    );
}

#[test]
fn scan_skips_linked_worktree_without_direct_cargo_toml() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = primary.join(".worktrees/feature");
    fs::create_dir_all(primary.join(".git")).unwrap();
    fs::create_dir_all(&linked).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");
    write_file(&linked.join("nested/Cargo.toml"), "[workspace]\n");

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        },
        Arc::new(FakeResolver::paths(vec![linked])),
    );

    let report = scanner.scan_with_errors().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    assert_eq!(report.projects, vec![canonical_primary.clone()]);
    assert_eq!(
        report.worktree_discoveries,
        vec![WorktreeDiscovery::Success {
            primary: canonical_primary,
            linked: vec![],
            excluded: vec![],
            out_of_scope: vec![],
        }]
    );
}

#[test]
fn scan_records_resolver_failure_and_retains_primary_project() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        },
        Arc::new(FakeResolver::failure("git failed")),
    );

    let report = scanner.scan_with_errors().unwrap();
    let canonical_primary = primary.canonicalize().unwrap();
    assert_eq!(report.projects, vec![canonical_primary.clone()]);
    assert_eq!(
        report.worktree_discoveries,
        vec![WorktreeDiscovery::Failure {
            primary: canonical_primary.clone(),
            message: "git failed".to_string(),
        }]
    );
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].path, canonical_primary);
    assert_eq!(report.errors[0].message, "git failed");
}

#[test]
fn scan_does_not_resolve_worktrees_for_linked_checkout_git_file() {
    let root = tempfile::tempdir().unwrap();
    let linked_checkout = root.path().join("feature");
    write_file(&linked_checkout.join("Cargo.toml"), "[workspace]\n");
    write_file(
        &linked_checkout.join(".git"),
        "gitdir: ../router/.git/worktrees/feature\n",
    );
    let resolver = FakeResolver::paths(vec![]);

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();
    assert_eq!(
        report.projects,
        vec![linked_checkout.canonicalize().unwrap()]
    );
    assert!(report.worktree_discoveries.is_empty());
    assert!(resolver.calls().is_empty());
}

#[cfg(unix)]
#[test]
fn scan_canonicalizes_non_git_alias_before_managed_cache_classification() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("Library/Caches/cached-project");
    let alias = root.path().join("workspace-alias");
    write_file(&project.join("Cargo.toml"), "[package]\n");
    write_file(&project.join("target/debug/blob.bin"), "payload");
    symlink(&project, &alias).unwrap();

    let scanner = Scanner::new(ScannerOptions {
        roots: vec![alias],
        project_dirs: vec![],
        excludes: vec![],
    });
    let report = scanner.scan_with_errors().unwrap();
    let canonical = project.canonicalize().unwrap();

    assert_eq!(report.projects, vec![canonical.clone()]);
    let review = review_project(
        &report.projects[0],
        &[],
        &[],
        SystemTime::now() + Duration::from_secs(1),
        &SafetyOptions {
            target_quiet_period: Duration::ZERO,
            include_managed_cache: false,
            include_active: false,
            force: false,
        },
    )
    .unwrap();
    assert_eq!(review.class, ProjectClass::ManagedCache);
    assert_eq!(
        review.decision,
        CleanDecision::Skipped(SkipReason::ManagedCache)
    );
}

#[cfg(unix)]
#[test]
fn scan_rejects_non_utf8_linked_path_as_discovery_failure() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("router");
    let linked = primary
        .join(".worktrees")
        .join(OsString::from_vec(b"\xff".to_vec()));
    fs::create_dir_all(primary.join(".git")).unwrap();
    write_file(&primary.join("Cargo.toml"), "[workspace]\n");

    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![root.path().to_path_buf()],
            project_dirs: vec![],
            excludes: vec![],
        },
        Arc::new(FakeResolver::paths(vec![linked])),
    );

    let canonical_primary = primary.canonicalize().unwrap();
    let report = scanner.scan_with_errors().unwrap();
    assert_eq!(report.projects, vec![canonical_primary.clone()]);
    assert!(matches!(
        report.worktree_discoveries.as_slice(),
        [WorktreeDiscovery::Failure { primary, message }]
            if primary == &canonical_primary && message.contains("non-UTF-8")
    ));
    assert_eq!(report.errors[0].path, canonical_primary);
}

#[test]
fn absolute_home_exclusion_prunes_before_manifest_and_git_discovery() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let excluded = home.join("Library");
    let legitimate = home.join("code/Library/project");
    write_file(
        &excluded.join("container-copy/Cargo.toml"),
        "[package]\nname='container-copy'\nversion='0.1.0'\n",
    );
    fs::create_dir_all(excluded.join("container-copy/.git")).unwrap();
    write_file(
        &legitimate.join("Cargo.toml"),
        "[package]\nname='project'\nversion='0.1.0'\n",
    );
    let resolver = FakeResolver::failure("excluded Git repository was inspected");
    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![home],
            project_dirs: vec![],
            excludes: vec![excluded.to_string_lossy().into_owned()],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();

    assert_eq!(report.projects, vec![legitimate.canonicalize().unwrap()]);
    assert!(report.errors.is_empty());
    assert!(resolver.calls().is_empty());
}

#[test]
fn excluded_missing_scan_root_produces_no_error() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("Library");
    let scanner = Scanner::new(ScannerOptions {
        roots: vec![missing.clone()],
        project_dirs: vec![],
        excludes: vec![missing.to_string_lossy().into_owned()],
    });

    let report = scanner.scan_with_errors().unwrap();
    assert!(report.projects.is_empty());
    assert!(report.errors.is_empty());
    assert!(report.worktree_discoveries.is_empty());
}

#[test]
fn excluded_explicit_project_does_not_resolve_worktrees() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("excluded-project");
    write_file(&project.join("Cargo.toml"), "[workspace]\n");
    fs::create_dir_all(project.join(".git")).unwrap();
    let resolver = FakeResolver::paths(vec![]);
    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![],
            project_dirs: vec![project.clone()],
            excludes: vec![project.to_string_lossy().into_owned()],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();
    assert!(report.projects.is_empty());
    assert!(report.errors.is_empty());
    assert!(report.worktree_discoveries.is_empty());
    assert!(resolver.calls().is_empty());
}

#[cfg(unix)]
#[test]
fn alias_to_excluded_root_is_rejected_after_canonicalization() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let excluded = root.path().join("Library/copied-crate");
    let alias = root.path().join("legacy-crate");
    write_file(&excluded.join("Cargo.toml"), "[workspace]\n");
    fs::create_dir_all(excluded.join(".git")).unwrap();
    symlink(&excluded, &alias).unwrap();
    let resolver = FakeResolver::paths(vec![]);
    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![alias],
            project_dirs: vec![],
            excludes: vec![root.path().join("Library").to_string_lossy().into_owned()],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();
    assert!(report.projects.is_empty());
    assert!(report.errors.is_empty());
    assert!(report.worktree_discoveries.is_empty());
    assert!(resolver.calls().is_empty());
}

#[cfg(unix)]
#[test]
fn alias_to_excluded_explicit_project_is_rejected_after_canonicalization() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let excluded = root.path().join("Library/copied-crate");
    let alias = root.path().join("legacy-crate");
    write_file(&excluded.join("Cargo.toml"), "[workspace]\n");
    fs::create_dir_all(excluded.join(".git")).unwrap();
    symlink(&excluded, &alias).unwrap();
    let resolver = FakeResolver::paths(vec![]);
    let scanner = Scanner::with_worktree_resolver(
        ScannerOptions {
            roots: vec![],
            project_dirs: vec![alias],
            excludes: vec![root.path().join("Library").to_string_lossy().into_owned()],
        },
        Arc::new(resolver.clone()),
    );

    let report = scanner.scan_with_errors().unwrap();
    assert!(report.projects.is_empty());
    assert!(report.errors.is_empty());
    assert!(report.worktree_discoveries.is_empty());
    assert!(resolver.calls().is_empty());
}
