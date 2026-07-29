use crate::policy::{
    Environment, ProcessEnvironment, ProtectedRoot, ProtectedRootKind, RootProvenance,
};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPlatform {
    MacOs,
    Linux,
    Other,
}

impl HostPlatform {
    pub(crate) fn current() -> Self {
        match env::consts::OS {
            "macos" => Self::MacOs,
            "linux" => Self::Linux,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtectedKind {
    ManagedCache,
    ContainerStorage,
}

pub(crate) fn current_home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn protected(path: PathBuf, kind: ProtectedRootKind, provenance: RootProvenance) -> ProtectedRoot {
    ProtectedRoot {
        path,
        kind,
        provenance,
    }
}

fn default_root(home: &Path, relative: &str, kind: ProtectedRootKind) -> ProtectedRoot {
    protected(home.join(relative), kind, RootProvenance::Default)
}

fn environment_root(
    environment: &dyn Environment,
    variable: &str,
    suffix: Option<&str>,
    kind: ProtectedRootKind,
) -> Option<ProtectedRoot> {
    let value = environment.var_os(variable)?;
    if value.is_empty() {
        return None;
    }
    let mut path = PathBuf::from(value);
    if let Some(suffix) = suffix {
        path.push(suffix);
    }
    Some(protected(
        path,
        kind,
        RootProvenance::Environment(variable.to_string()),
    ))
}

pub fn protected_roots_for(
    platform: HostPlatform,
    home: &Path,
    environment: &dyn Environment,
) -> Vec<ProtectedRoot> {
    let mut roots = Vec::new();
    if home.is_absolute() {
        roots.extend([
            default_root(home, ".cargo", ProtectedRootKind::Cargo),
            default_root(home, ".rustup", ProtectedRootKind::Rustup),
            default_root(home, ".cache", ProtectedRootKind::ManagedCache),
            default_root(home, ".bun/install/cache", ProtectedRootKind::Bun),
            default_root(home, "go/pkg/mod", ProtectedRootKind::GoModule),
            default_root(home, ".colima", ProtectedRootKind::Container),
            default_root(home, ".lima", ProtectedRootKind::Container),
            default_root(
                home,
                ".local/share/containers",
                ProtectedRootKind::Container,
            ),
        ]);
        match platform {
            HostPlatform::MacOs => roots.extend([
                default_root(home, "Library", ProtectedRootKind::ManagedCache),
                default_root(home, ".Trash", ProtectedRootKind::ManagedCache),
                default_root(home, "OrbStack", ProtectedRootKind::Container),
            ]),
            HostPlatform::Linux => roots.extend([
                default_root(home, ".local/share/docker", ProtectedRootKind::Container),
                default_root(home, ".docker/desktop", ProtectedRootKind::Container),
                default_root(
                    home,
                    ".local/share/rancher-desktop",
                    ProtectedRootKind::Container,
                ),
                default_root(home, ".local/share/Trash", ProtectedRootKind::ManagedCache),
            ]),
            HostPlatform::Other => {}
        }
    }

    roots.extend(
        [
            environment_root(environment, "CARGO_HOME", None, ProtectedRootKind::Cargo),
            environment_root(environment, "RUSTUP_HOME", None, ProtectedRootKind::Rustup),
            environment_root(
                environment,
                "XDG_CACHE_HOME",
                None,
                ProtectedRootKind::ManagedCache,
            ),
            environment_root(
                environment,
                "XDG_DATA_HOME",
                Some("containers"),
                ProtectedRootKind::Container,
            ),
            environment_root(
                environment,
                "XDG_DATA_HOME",
                Some("docker"),
                ProtectedRootKind::Container,
            ),
            environment_root(
                environment,
                "XDG_DATA_HOME",
                Some("rancher-desktop"),
                ProtectedRootKind::Container,
            ),
            environment_root(environment, "GOMODCACHE", None, ProtectedRootKind::GoModule),
            environment_root(
                environment,
                "BUN_INSTALL",
                Some("install/cache"),
                ProtectedRootKind::Bun,
            ),
            environment_root(
                environment,
                "BUN_INSTALL_CACHE_DIR",
                None,
                ProtectedRootKind::Bun,
            ),
            environment_root(
                environment,
                "COLIMA_HOME",
                None,
                ProtectedRootKind::Container,
            ),
            environment_root(environment, "LIMA_HOME", None, ProtectedRootKind::Container),
        ]
        .into_iter()
        .flatten(),
    );

    let mut seen = BTreeSet::new();
    roots.retain(|root| seen.insert(root.clone()));
    roots
}

pub(crate) fn classify_protected_path_for(
    path: &Path,
    home: &Path,
    platform: HostPlatform,
) -> Option<ProtectedKind> {
    classify_protected_path_with_environment(path, home, platform, &ProcessEnvironment)
}

fn classify_protected_path_with_environment(
    path: &Path,
    home: &Path,
    platform: HostPlatform,
    environment: &dyn Environment,
) -> Option<ProtectedKind> {
    protected_root_for_path_with_environment(path, home, platform, environment).map(|root| {
        match root.kind {
            ProtectedRootKind::Container => ProtectedKind::ContainerStorage,
            ProtectedRootKind::Cargo
            | ProtectedRootKind::Rustup
            | ProtectedRootKind::GoModule
            | ProtectedRootKind::Bun
            | ProtectedRootKind::ManagedCache => ProtectedKind::ManagedCache,
        }
    })
}

fn protected_root_for_path_with_environment(
    path: &Path,
    home: &Path,
    platform: HostPlatform,
    environment: &dyn Environment,
) -> Option<ProtectedRoot> {
    fn within(path: &Path, root: &Path) -> bool {
        path == root || path.starts_with(root)
    }

    let physical_path = fs::canonicalize(path).ok();
    protected_roots_for(platform, home, environment)
        .into_iter()
        .find(|root| {
            within(path, &root.path)
                || physical_path.as_deref().is_some_and(|physical| {
                    within(physical, &root.path)
                        || fs::canonicalize(&root.path)
                            .ok()
                            .is_some_and(|physical_root| within(physical, &physical_root))
                })
        })
        .or_else(|| structural_root(path))
        .or_else(|| physical_path.as_deref().and_then(structural_root))
}

fn structural_root(path: &Path) -> Option<ProtectedRoot> {
    const PATTERNS: &[(&[&str], ProtectedRootKind)] = &[
        (&[".bun", "install", "cache"], ProtectedRootKind::Bun),
        (&["go", "pkg", "mod"], ProtectedRootKind::GoModule),
        (&[".cargo", "registry", "src"], ProtectedRootKind::Cargo),
        (&[".cargo", "git", "checkouts"], ProtectedRootKind::Cargo),
        (&["Library", "Caches"], ProtectedRootKind::ManagedCache),
        (&["OrbStack", "docker"], ProtectedRootKind::Container),
    ];

    let components = path.components().collect::<Vec<_>>();
    for (pattern, kind) in PATTERNS {
        let Some(start) = components.windows(pattern.len()).position(|window| {
            window
                .iter()
                .zip(*pattern)
                .all(|(actual, expected)| actual.as_os_str() == *expected)
        }) else {
            continue;
        };
        let mut root = PathBuf::new();
        for component in &components[..start + pattern.len()] {
            root.push(component.as_os_str());
        }
        return Some(protected(root, kind.clone(), RootProvenance::Structural));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::Path;

    struct EmptyEnvironment;

    impl Environment for EmptyEnvironment {
        fn var_os(&self, _name: &str) -> Option<OsString> {
            None
        }
    }

    struct TestEnvironment(BTreeMap<String, OsString>);

    impl TestEnvironment {
        fn one(name: &str, value: &str) -> Self {
            Self(BTreeMap::from([(name.to_string(), OsString::from(value))]))
        }
    }

    impl Environment for TestEnvironment {
        fn var_os(&self, name: &str) -> Option<OsString> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn macos_profile_maps_every_protected_root() {
        let home = Path::new("/Users/tester");
        let roots = protected_roots_for(HostPlatform::MacOs, home, &EmptyEnvironment);

        assert_eq!(
            roots,
            vec![
                default_root(home, ".cargo", ProtectedRootKind::Cargo),
                default_root(home, ".rustup", ProtectedRootKind::Rustup),
                default_root(home, ".cache", ProtectedRootKind::ManagedCache),
                default_root(home, ".bun/install/cache", ProtectedRootKind::Bun),
                default_root(home, "go/pkg/mod", ProtectedRootKind::GoModule),
                default_root(home, ".colima", ProtectedRootKind::Container),
                default_root(home, ".lima", ProtectedRootKind::Container),
                default_root(
                    home,
                    ".local/share/containers",
                    ProtectedRootKind::Container,
                ),
                default_root(home, "Library", ProtectedRootKind::ManagedCache),
                default_root(home, ".Trash", ProtectedRootKind::ManagedCache),
                default_root(home, "OrbStack", ProtectedRootKind::Container),
            ]
        );
    }

    #[test]
    fn linux_profile_maps_every_protected_root() {
        let home = Path::new("/home/tester");
        let roots = protected_roots_for(HostPlatform::Linux, home, &EmptyEnvironment);

        assert_eq!(
            roots,
            vec![
                default_root(home, ".cargo", ProtectedRootKind::Cargo),
                default_root(home, ".rustup", ProtectedRootKind::Rustup),
                default_root(home, ".cache", ProtectedRootKind::ManagedCache),
                default_root(home, ".bun/install/cache", ProtectedRootKind::Bun),
                default_root(home, "go/pkg/mod", ProtectedRootKind::GoModule),
                default_root(home, ".colima", ProtectedRootKind::Container),
                default_root(home, ".lima", ProtectedRootKind::Container),
                default_root(
                    home,
                    ".local/share/containers",
                    ProtectedRootKind::Container,
                ),
                default_root(home, ".local/share/docker", ProtectedRootKind::Container,),
                default_root(home, ".docker/desktop", ProtectedRootKind::Container),
                default_root(
                    home,
                    ".local/share/rancher-desktop",
                    ProtectedRootKind::Container,
                ),
                default_root(home, ".local/share/Trash", ProtectedRootKind::ManagedCache,),
            ]
        );
    }

    #[test]
    fn relative_or_missing_home_returns_no_anchored_roots() {
        assert!(
            protected_roots_for(HostPlatform::MacOs, Path::new(""), &EmptyEnvironment).is_empty()
        );
        assert!(protected_roots_for(
            HostPlatform::Linux,
            Path::new("relative-home"),
            &EmptyEnvironment
        )
        .is_empty());
    }

    #[test]
    fn similarly_named_nonstructural_paths_outside_home_are_not_classified() {
        let home = Path::new("/Users/tester");

        assert_eq!(
            classify_protected_path_for(
                Path::new("/tmp/tester/.cargo/registry/src-demo/copied-crate"),
                home,
                HostPlatform::MacOs,
            ),
            None
        );
    }

    #[test]
    fn relocated_manager_shapes_retain_structural_provenance() {
        let root = protected_root_for_path_with_environment(
            Path::new("/opt/relocated/.cargo/registry/src/index/crate"),
            Path::new("/Users/tester"),
            HostPlatform::MacOs,
            &EmptyEnvironment,
        )
        .unwrap();

        assert_eq!(root.kind, ProtectedRootKind::Cargo);
        assert_eq!(root.provenance, RootProvenance::Structural);
        assert_eq!(root.path, Path::new("/opt/relocated/.cargo/registry/src"));
    }

    #[test]
    fn path_classifier_consumes_environment_relocated_roots() {
        let environment = TestEnvironment::one("CARGO_HOME", "/opt/toolchains/cargo");

        assert_eq!(
            classify_protected_path_with_environment(
                Path::new("/opt/toolchains/cargo/bin/package"),
                Path::new("/Users/tester"),
                HostPlatform::MacOs,
                &environment,
            ),
            Some(ProtectedKind::ManagedCache)
        );
    }
}
