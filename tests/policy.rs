use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use car_go_clean::config::{load, Config};
use car_go_clean::policy::{
    Canonicalizer, Environment, ProtectedRootKind, RootProvenance, ScopePolicy,
};

#[derive(Default)]
struct TestEnvironment {
    values: BTreeMap<OsString, OsString>,
}

impl TestEnvironment {
    fn from_pairs(values: &[(&str, &str)]) -> Self {
        Self {
            values: values
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value)))
                .collect(),
        }
    }
}

impl Environment for TestEnvironment {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.values.get(OsStr::new(name)).cloned()
    }
}

#[derive(Default)]
struct TestCanonicalizer {
    outcomes: BTreeMap<PathBuf, Result<PathBuf, io::ErrorKind>>,
}

impl TestCanonicalizer {
    fn maps(mut self, from: &str, to: &str) -> Self {
        self.outcomes
            .insert(PathBuf::from(from), Ok(PathBuf::from(to)));
        self
    }

    fn fails(mut self, path: &str, kind: io::ErrorKind) -> Self {
        self.outcomes.insert(PathBuf::from(path), Err(kind));
        self
    }
}

impl Canonicalizer for TestCanonicalizer {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        match self.outcomes.get(path) {
            Some(Ok(canonical)) => Ok(canonical.clone()),
            Some(Err(kind)) => Err(io::Error::from(*kind)),
            None => Ok(path.to_path_buf()),
        }
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn test_config(scan_dirs: &[&str], project_dirs: &[&str], excludes: &[&str]) -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let body = format!(
        "scan_dirs = {}\nproject_dirs = {}\noverride_excludes = {}\n",
        serde_json::to_string(&strings(scan_dirs)).unwrap(),
        serde_json::to_string(&strings(project_dirs)).unwrap(),
        serde_json::to_string(&strings(excludes)).unwrap(),
    );
    fs::write(&path, body).unwrap();
    load(path).unwrap()
}

fn environment() -> TestEnvironment {
    TestEnvironment::from_pairs(&[("HOME", "/home/tester")])
}

fn build_with(
    config: &Config,
    config_source: &str,
    environment: &dyn Environment,
    canonicalizer: &dyn Canonicalizer,
) -> ScopePolicy {
    ScopePolicy::build_with_canonicalizer(
        config,
        Path::new(config_source),
        environment,
        canonicalizer,
    )
    .unwrap()
}

#[test]
fn policy_hash_is_stable_across_input_order() {
    let first = test_config(
        &["/scope/b", "/scope/a", "/scope/a"],
        &["/project/b", "/project/a", "/project/a"],
        &["vendor", "node_modules", "vendor"],
    );
    let second = test_config(
        &["/scope/a", "/scope/b"],
        &["/project/a", "/project/b"],
        &["node_modules", "vendor"],
    );
    let canonicalizer = TestCanonicalizer::default()
        .maps("/scope/a", "/physical/scope/a")
        .maps("/scope/b", "/physical/scope/b")
        .maps("/project/a", "/physical/project/a")
        .maps("/project/b", "/physical/project/b")
        .maps("vendor", "/physical/excludes/vendor")
        .maps("node_modules", "/physical/excludes/node_modules");

    let first = build_with(&first, "/config.toml", &environment(), &canonicalizer);
    let second = build_with(&second, "/config.toml", &environment(), &canonicalizer);

    assert_eq!(first.hash(), second.hash());
    assert_eq!(
        first.diagnostics().canonical_scan_roots,
        &[
            PathBuf::from("/physical/scope/a"),
            PathBuf::from("/physical/scope/b")
        ]
    );
}

#[test]
fn relative_exclusions_are_lexical_only_and_do_not_depend_on_process_working_directory() {
    let config = test_config(&["/scope"], &[], &[".git", "node_modules"]);
    let first_canonicalizer = TestCanonicalizer::default()
        .fails(".git", io::ErrorKind::PermissionDenied)
        .fails("node_modules", io::ErrorKind::Other);
    let second_canonicalizer = TestCanonicalizer::default()
        .maps(".git", "/checkout/b/.git")
        .maps("node_modules", "/checkout/b/node_modules");

    let first = build_with(
        &config,
        "/config.toml",
        &environment(),
        &first_canonicalizer,
    );
    let second = build_with(
        &config,
        "/config.toml",
        &environment(),
        &second_canonicalizer,
    );

    assert_eq!(first.hash(), second.hash());
    assert!(first.diagnostics().canonical_exclusions.is_empty());
    assert!(second.diagnostics().canonical_exclusions.is_empty());
}

#[test]
fn policy_hash_changes_for_each_enumerated_authority_input() {
    let baseline_config = test_config(&["/scope/a"], &["/project/a"], &["/exclude/a"]);
    let baseline_canonicalizer = TestCanonicalizer::default()
        .maps("/scope/a", "/physical/scope/a")
        .maps("/project/a", "/physical/project/a")
        .maps("/exclude/a", "/physical/exclude/a");
    let baseline_environment =
        TestEnvironment::from_pairs(&[("HOME", "/home/tester"), ("CARGO_HOME", "/manager")]);
    let baseline = build_with(
        &baseline_config,
        "/config/a.toml",
        &baseline_environment,
        &baseline_canonicalizer,
    );

    let scan_config = test_config(&["/scope/b"], &["/project/a"], &["/exclude/a"]);
    let scan = build_with(
        &scan_config,
        "/config/a.toml",
        &baseline_environment,
        &baseline_canonicalizer,
    );

    let project_config = test_config(&["/scope/a"], &["/project/b"], &["/exclude/a"]);
    let project = build_with(
        &project_config,
        "/config/a.toml",
        &baseline_environment,
        &baseline_canonicalizer,
    );

    let lexical_config = test_config(&["/scope/a"], &["/project/a"], &["/exclude/b"]);
    let lexical_canonicalizer =
        TestCanonicalizer::default().maps("/exclude/b", "/physical/exclude/a");
    let lexical = build_with(
        &lexical_config,
        "/config/a.toml",
        &baseline_environment,
        &lexical_canonicalizer,
    );

    let canonical_canonicalizer = TestCanonicalizer::default()
        .maps("/scope/a", "/physical/scope/a")
        .maps("/project/a", "/physical/project/a")
        .maps("/exclude/a", "/physical/exclude/b");
    let canonical = build_with(
        &baseline_config,
        "/config/a.toml",
        &baseline_environment,
        &canonical_canonicalizer,
    );

    let protected_environment = TestEnvironment::from_pairs(&[
        ("HOME", "/home/tester"),
        ("BUN_INSTALL_CACHE_DIR", "/manager"),
    ]);
    let protected = build_with(
        &baseline_config,
        "/config/a.toml",
        &protected_environment,
        &baseline_canonicalizer,
    );

    let mut quiet_config = baseline_config.clone();
    quiet_config.target_quiet_period += Duration::from_millis(1);
    let quiet = build_with(
        &quiet_config,
        "/config/a.toml",
        &baseline_environment,
        &baseline_canonicalizer,
    );

    let mut scan_interval_config = baseline_config.clone();
    scan_interval_config.scan_interval += Duration::from_millis(1);
    let scan_interval = build_with(
        &scan_interval_config,
        "/config/a.toml",
        &baseline_environment,
        &baseline_canonicalizer,
    );

    let config_source = build_with(
        &baseline_config,
        "/config/b.toml",
        &baseline_environment,
        &baseline_canonicalizer,
    );

    for (input, policy) in [
        ("canonical scan roots", scan),
        ("canonical project paths", project),
        ("lexical exclusions", lexical),
        ("canonical exclusions", canonical),
        ("protected roots and kinds", protected),
        ("target quiet period", quiet),
        ("scan interval", scan_interval),
        ("config source", config_source),
    ] {
        assert_ne!(baseline.hash(), policy.hash(), "{input}");
    }
}

#[test]
fn clean_interval_and_log_level_do_not_change_policy_hash() {
    let config = test_config(&["/scope"], &["/project"], &["vendor"]);
    let baseline = build_with(
        &config,
        "/config.toml",
        &environment(),
        &TestCanonicalizer::default(),
    );
    let mut changed = config.clone();
    changed.clean_interval += Duration::from_secs(17);
    changed.log_level = "debug".to_string();
    let changed = build_with(
        &changed,
        "/config.toml",
        &environment(),
        &TestCanonicalizer::default(),
    );

    assert_eq!(baseline.hash(), changed.hash());
}

#[test]
fn missing_configured_root_is_an_error() {
    for (label, config, missing) in [
        (
            "scan root",
            test_config(&["/missing-scan"], &[], &[]),
            "/missing-scan",
        ),
        (
            "explicit project",
            test_config(&[], &["/missing-project"], &[]),
            "/missing-project",
        ),
    ] {
        let canonicalizer = TestCanonicalizer::default().fails(missing, io::ErrorKind::NotFound);

        let error = ScopePolicy::build_with_canonicalizer(
            &config,
            Path::new("/config.toml"),
            &environment(),
            &canonicalizer,
        )
        .unwrap_err();

        let error = format!("{error:#}");
        assert!(error.contains(missing), "{label}: {error}");
        assert!(error.contains("canonicalize"), "{label}: {error}");
    }
}

#[test]
fn missing_speculative_exclusion_is_not_an_error() {
    let config = test_config(&["/scope"], &[], &["/home/tester/.colima"]);
    let canonicalizer =
        TestCanonicalizer::default().fails("/home/tester/.colima", io::ErrorKind::NotFound);

    let policy = build_with(&config, "/config.toml", &environment(), &canonicalizer);

    assert_eq!(
        policy.diagnostics().lexical_exclusions,
        ["/home/tester/.colima"]
    );
    assert!(policy.diagnostics().canonical_exclusions.is_empty());
    assert!(policy.is_excluded(Path::new("/home/tester/.colima/default/disk")));
}

#[test]
fn unreadable_exclusion_blocks_policy_construction() {
    let config = test_config(&["/scope"], &[], &["/protected/exclusion"]);
    let canonicalizer =
        TestCanonicalizer::default().fails("/protected/exclusion", io::ErrorKind::PermissionDenied);

    let error = ScopePolicy::build_with_canonicalizer(
        &config,
        Path::new("/config.toml"),
        &environment(),
        &canonicalizer,
    )
    .unwrap_err();

    let error = format!("{error:#}");
    assert!(
        error.contains("canonicalize exclusion /protected/exclusion"),
        "{error}"
    );
    assert!(
        error.to_lowercase().contains("permission denied"),
        "{error}"
    );
}

#[test]
fn relocated_manager_roots_have_environment_provenance() {
    let config = test_config(&["/scope"], &[], &[]);
    let environment = TestEnvironment::from_pairs(&[
        ("HOME", "/home/tester"),
        ("CARGO_HOME", "/relocated/cargo"),
        ("RUSTUP_HOME", "/relocated/rustup"),
        ("XDG_CACHE_HOME", "/relocated/cache"),
        ("XDG_DATA_HOME", "/relocated/data"),
        ("GOMODCACHE", "/relocated/go-mod"),
        ("BUN_INSTALL", "/relocated/bun"),
        ("BUN_INSTALL_CACHE_DIR", "/relocated/bun-cache"),
        ("COLIMA_HOME", "/relocated/colima"),
        ("LIMA_HOME", "/relocated/lima"),
    ]);

    let policy = build_with(
        &config,
        "/config.toml",
        &environment,
        &TestCanonicalizer::default(),
    );
    let roots = policy.diagnostics().protected_roots;

    for (path, kind, variable) in [
        ("/relocated/cargo", ProtectedRootKind::Cargo, "CARGO_HOME"),
        (
            "/relocated/rustup",
            ProtectedRootKind::Rustup,
            "RUSTUP_HOME",
        ),
        (
            "/relocated/go-mod",
            ProtectedRootKind::GoModule,
            "GOMODCACHE",
        ),
        (
            "/relocated/bun/install/cache",
            ProtectedRootKind::Bun,
            "BUN_INSTALL",
        ),
        (
            "/relocated/bun-cache",
            ProtectedRootKind::Bun,
            "BUN_INSTALL_CACHE_DIR",
        ),
        (
            "/relocated/colima",
            ProtectedRootKind::Container,
            "COLIMA_HOME",
        ),
        ("/relocated/lima", ProtectedRootKind::Container, "LIMA_HOME"),
        (
            "/relocated/cache",
            ProtectedRootKind::ManagedCache,
            "XDG_CACHE_HOME",
        ),
    ] {
        assert!(
            roots.iter().any(|root| {
                root.path == Path::new(path)
                    && root.kind == kind
                    && root.provenance == RootProvenance::Environment(variable.to_string())
            }),
            "{path} ({variable}) was not represented with environment provenance: {roots:#?}"
        );
    }

    for path in [
        "/relocated/data/containers",
        "/relocated/data/docker",
        "/relocated/data/rancher-desktop",
    ] {
        assert!(
            roots.iter().any(|root| {
                root.path == Path::new(path)
                    && root.kind == ProtectedRootKind::Container
                    && root.provenance == RootProvenance::Environment("XDG_DATA_HOME".to_string())
            }),
            "{path} was not represented with XDG_DATA_HOME provenance: {roots:#?}"
        );
    }
}

#[test]
fn manager_root_overrides_must_be_stable_absolute_paths() {
    let config = test_config(&["/scope"], &[], &[]);
    for (variable, value) in [
        ("CARGO_HOME", "relative/cargo"),
        ("RUSTUP_HOME", "/toolchains/../rustup"),
        ("COLIMA_HOME", "/containers/./colima"),
    ] {
        let environment =
            TestEnvironment::from_pairs(&[("HOME", "/home/tester"), (variable, value)]);
        let error = ScopePolicy::build_with_canonicalizer(
            &config,
            Path::new("/config.toml"),
            &environment,
            &TestCanonicalizer::default(),
        )
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains(variable), "{variable}: {error}");
        assert!(
            error.contains("absolute physical path"),
            "{variable}: {error}"
        );
    }
}

#[test]
fn manager_root_aliases_are_physical_before_policy_hashing() {
    let config = test_config(&["/scope"], &[], &[]);
    let alias_environment = TestEnvironment::from_pairs(&[
        ("HOME", "/home/tester"),
        ("CARGO_HOME", "/install-shell/cargo"),
    ]);
    let physical_environment = TestEnvironment::from_pairs(&[
        ("HOME", "/home/tester"),
        ("CARGO_HOME", "/physical/manager/cargo"),
    ]);
    let canonicalizer = TestCanonicalizer::default()
        .maps("/install-shell/cargo", "/physical/manager/cargo")
        .maps("/physical/manager/cargo", "/physical/manager/cargo");

    let alias = build_with(&config, "/config.toml", &alias_environment, &canonicalizer);
    let physical = build_with(
        &config,
        "/config.toml",
        &physical_environment,
        &canonicalizer,
    );

    assert_eq!(alias.hash(), physical.hash());
    assert!(alias.diagnostics().protected_roots.iter().any(|root| {
        root.path == Path::new("/physical/manager/cargo")
            && root.kind == ProtectedRootKind::Cargo
            && root.provenance == RootProvenance::Environment("CARGO_HOME".to_string())
    }));
    assert!(!alias
        .diagnostics()
        .protected_roots
        .iter()
        .any(|root| root.path == Path::new("/install-shell/cargo")));
}

#[test]
fn scope_and_exclusion_checks_use_canonical_authority_inputs() {
    let config = test_config(&["/scope"], &["/one-off"], &["vendor", "/excluded-alias"]);
    let canonicalizer = TestCanonicalizer::default()
        .maps("/scope", "/physical/scope")
        .maps("/one-off", "/physical/one-off")
        .maps("/excluded-alias", "/physical/excluded");
    let policy = build_with(&config, "/config.toml", &environment(), &canonicalizer);

    assert!(policy.contains_project(Path::new("/physical/scope/project")));
    assert!(policy.contains_project(Path::new("/physical/one-off")));
    assert!(!policy.contains_project(Path::new("/physical/one-off/nested")));
    assert!(!policy.contains_project(Path::new("/outside")));

    assert!(policy.is_excluded(Path::new("/physical/scope/vendor/project")));
    assert!(policy.is_excluded(Path::new("/excluded-alias/project")));
    assert!(policy.is_excluded(Path::new("/physical/excluded/project")));
    assert!(!policy.is_excluded(Path::new("/physical/scope/vendorized/project")));
}
