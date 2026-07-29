use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostPlatform {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtectedRoot {
    pub(crate) path: PathBuf,
    pub(crate) kind: ProtectedKind,
}

pub(crate) fn current_home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn protected(home: &Path, relative: &str, kind: ProtectedKind) -> ProtectedRoot {
    ProtectedRoot {
        path: home.join(relative),
        kind,
    }
}

pub(crate) fn protected_roots_for(home: &Path, platform: HostPlatform) -> Vec<ProtectedRoot> {
    if !home.is_absolute() {
        return Vec::new();
    }

    let mut roots = vec![
        protected(home, ".cargo", ProtectedKind::ManagedCache),
        protected(home, ".rustup", ProtectedKind::ManagedCache),
        protected(home, ".cache", ProtectedKind::ManagedCache),
        protected(home, ".bun/install/cache", ProtectedKind::ManagedCache),
        protected(home, "go/pkg/mod", ProtectedKind::ManagedCache),
        protected(home, ".colima", ProtectedKind::ContainerStorage),
        protected(home, ".lima", ProtectedKind::ContainerStorage),
        protected(
            home,
            ".local/share/containers",
            ProtectedKind::ContainerStorage,
        ),
    ];
    match platform {
        HostPlatform::MacOs => roots.extend([
            protected(home, "Library", ProtectedKind::ManagedCache),
            protected(home, ".Trash", ProtectedKind::ManagedCache),
            protected(home, "OrbStack", ProtectedKind::ContainerStorage),
        ]),
        HostPlatform::Linux => roots.extend([
            protected(home, ".local/share/docker", ProtectedKind::ContainerStorage),
            protected(home, ".docker/desktop", ProtectedKind::ContainerStorage),
            protected(
                home,
                ".local/share/rancher-desktop",
                ProtectedKind::ContainerStorage,
            ),
            protected(home, ".local/share/Trash", ProtectedKind::ManagedCache),
        ]),
        HostPlatform::Other => {}
    }
    roots
}

pub(crate) fn classify_protected_path_for(
    path: &Path,
    home: &Path,
    platform: HostPlatform,
) -> Option<ProtectedKind> {
    fn within(path: &Path, root: &Path) -> bool {
        path == root || path.starts_with(root)
    }

    let physical_path = fs::canonicalize(path).ok();
    protected_roots_for(home, platform)
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
        .map(|root| root.kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn macos_profile_maps_every_protected_root() {
        let home = Path::new("/Users/tester");
        let roots = protected_roots_for(home, HostPlatform::MacOs);

        assert_eq!(
            roots,
            vec![
                protected(home, ".cargo", ProtectedKind::ManagedCache),
                protected(home, ".rustup", ProtectedKind::ManagedCache),
                protected(home, ".cache", ProtectedKind::ManagedCache),
                protected(home, ".bun/install/cache", ProtectedKind::ManagedCache),
                protected(home, "go/pkg/mod", ProtectedKind::ManagedCache),
                protected(home, ".colima", ProtectedKind::ContainerStorage),
                protected(home, ".lima", ProtectedKind::ContainerStorage),
                protected(
                    home,
                    ".local/share/containers",
                    ProtectedKind::ContainerStorage,
                ),
                protected(home, "Library", ProtectedKind::ManagedCache),
                protected(home, ".Trash", ProtectedKind::ManagedCache),
                protected(home, "OrbStack", ProtectedKind::ContainerStorage),
            ]
        );
    }

    #[test]
    fn linux_profile_maps_every_protected_root() {
        let home = Path::new("/home/tester");
        let roots = protected_roots_for(home, HostPlatform::Linux);

        assert_eq!(
            roots,
            vec![
                protected(home, ".cargo", ProtectedKind::ManagedCache),
                protected(home, ".rustup", ProtectedKind::ManagedCache),
                protected(home, ".cache", ProtectedKind::ManagedCache),
                protected(home, ".bun/install/cache", ProtectedKind::ManagedCache),
                protected(home, "go/pkg/mod", ProtectedKind::ManagedCache),
                protected(home, ".colima", ProtectedKind::ContainerStorage),
                protected(home, ".lima", ProtectedKind::ContainerStorage),
                protected(
                    home,
                    ".local/share/containers",
                    ProtectedKind::ContainerStorage,
                ),
                protected(home, ".local/share/docker", ProtectedKind::ContainerStorage,),
                protected(home, ".docker/desktop", ProtectedKind::ContainerStorage),
                protected(
                    home,
                    ".local/share/rancher-desktop",
                    ProtectedKind::ContainerStorage,
                ),
                protected(home, ".local/share/Trash", ProtectedKind::ManagedCache),
            ]
        );
    }

    #[test]
    fn relative_or_missing_home_returns_no_anchored_roots() {
        assert!(protected_roots_for(Path::new(""), HostPlatform::MacOs).is_empty());
        assert!(protected_roots_for(Path::new("relative-home"), HostPlatform::Linux).is_empty());
    }

    #[test]
    fn similarly_named_paths_outside_home_are_not_classified() {
        let home = Path::new("/Users/tester");

        assert_eq!(
            classify_protected_path_for(
                Path::new("/tmp/tester/.cargo/registry/src/copied-crate"),
                home,
                HostPlatform::MacOs,
            ),
            None
        );
    }
}
