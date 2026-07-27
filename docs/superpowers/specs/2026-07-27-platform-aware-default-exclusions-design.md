# Platform-Aware Default Exclusions

**Status:** Approved

## Problem

`car-go-clean` scans `$HOME` by default and currently excludes only a small
set of relative path patterns. On macOS, this makes the scanner descend into
system-managed directories such as `~/Library`, `~/.Trash`, and
`~/OrbStack`.

The OrbStack case demonstrates the failure mode. The live state database
contained 8,728 paths beneath `~/OrbStack` that still had a `Cargo.toml`, but
none had a direct `target/` directory and none had ever been cleaned. They
were copied crate sources and build contexts inside container layers, not
valid cleanup targets. Traversing them nevertheless produced filesystem
errors and 14 Git dubious-ownership failures during linked-worktree
discovery.

The current safety review prevents `cargo clean` from running without a
direct `project/target`, so this is not a destructive-cleaning bug. It is a
discovery-scope, performance, state-quality, and diagnostic-noise problem.

## Terminology

- A **discovery candidate** is a directory containing `Cargo.toml`.
- A **valid cleanup target** is a discovery candidate with a direct,
  non-symlink `target/` directory that passes every safety gate.
- A **default exclusion** is an editable configuration default. It is not an
  unoverrideable safety rule.

This distinction matters because container images and dependency stores can
contain thousands of valid Cargo manifests without containing any project
target that `car-go-clean` should clean.

## Goals

- Continue discovering projects broadly beneath `$HOME`.
- Prune known operating-system, package-manager, container, and VM storage
  before reading their contents.
- Make the defaults appropriate for both macOS and Linux.
- Keep defaults editable so an advanced user can deliberately opt a path
  back into discovery.
- Retain managed-cache and container-storage classification as defense in
  depth.
- Reconcile cached candidates when a configuration begins excluding them.
- Produce zero OrbStack discovery candidates, worktree-discovery attempts,
  or new scan errors with the macOS defaults.

## Non-Goals

- Cleaning container images, volumes, VM disks, Cargo registries, Rust
  toolchains, or other package-manager caches.
- Replacing Docker, OrbStack, Podman, Colima, Lima, or Rancher Desktop cleanup
  commands.
- Guessing every possible custom storage location.
- Restricting discovery to conventional source directories such as `~/src`
  or `~/code`.
- Adding a general versioned configuration-migration framework.
- Deleting historical diagnostic records. Existing errors will age out of
  health windows normally.

## Default Exclusion Profiles

Pruning has one hard structural rule followed by editable default profiles.

### Hard structural pruning

`target` remains a hard scanner exclusion because it is build output and is
the directory the cleaner acts on after discovering its parent project. It
cannot be opted into discovery.

### Universal component defaults

These directory names are never useful discovery roots wherever they occur:

- `.git`
- `node_modules`

Unlike `target`, these entries are ordinary editable defaults.

### Home-anchored common exclusions

These paths are resolved beneath the active user's `$HOME`. Anchoring avoids
accidentally excluding a legitimate path such as `~/code/Library` merely
because one component has a system-directory name.

- `$HOME/.cargo`
- `$HOME/.rustup`
- `$HOME/.cache`
- `$HOME/.bun/install/cache`
- `$HOME/go/pkg/mod`
- `$HOME/.colima`
- `$HOME/.lima`
- `$HOME/.local/share/containers`

The last three cover Colima and Lima VM state and the conventional Podman /
rootless container storage root on supported Unix hosts.

### macOS additions

- `$HOME/Library`
- `$HOME/.Trash`
- `$HOME/OrbStack`

Excluding `Library` also covers the normal Docker Desktop and Rancher Desktop
application and VM data locations on macOS. `Library/Caches` is therefore no
longer needed as a separate default.

### Linux additions

- `$HOME/.local/share/docker`
- `$HOME/.docker/desktop`
- `$HOME/.local/share/rancher-desktop`
- `$HOME/.local/share/Trash`

The normal system-wide Docker Engine root, `/var/lib/docker`, is already
outside the default `$HOME` scan and needs no default exclusion.

## Configuration Semantics

The platform profile is used when configuration is absent or when an
`excludes` field is omitted. An explicitly configured `excludes` list remains
the complete user-selected list; defaults are not silently merged back into
it. This preserves the ability to opt a managed location into discovery.

Home-anchored defaults are represented as absolute paths at runtime because
the existing matcher treats a relative multi-component exclusion as a
sequence that can match anywhere in a path.

There is one current user. The existing configuration will be updated
directly when the feature ships. No generalized migration system is needed.

Custom manager roots selected through environment variables or application
settings are outside this change; users can add those paths to `excludes`.

## Scan and Safety Flow

For every directory considered during a scan:

1. Apply hard and configured exclusions before `read_dir`.
2. Do not inspect `Cargo.toml`, Git metadata, or ignore files beneath an
   excluded directory.
3. Do not invoke `git worktree list` for anything beneath an excluded
   directory.
4. Continue ordinary manifest discovery for paths that remain in scope.
5. Require a direct, readable, non-symlink `project/target`.
6. Apply managed-cache, container-storage, activity, quiet-period, scan-error,
   and worktree-discovery safety gates.
7. Invoke:

   ```text
   cargo clean --target-dir <project>/target
   ```

   from the project directory with `CARGO_TARGET_DIR` removed from the
   environment.

The managed-cache and container-storage classifiers remain unchanged. If a
user removes an exclusion, those later safety gates still protect the
location by default.

## Exclusion Reconciliation

A successful scan also reconciles already cached state against the active
exclusions. This operation is based only on explicit exclusion matches; it
does not delete every project absent from a scan, because absence can result
from an unrelated read error.

Reconciliation removes:

- cached project rows whose canonical path now matches an exclusion;
- linked-worktree rows whose primary or linked path now matches an exclusion;
- active worktree-discovery failures whose primary or canonical primary now
  matches an exclusion.

The operation does not touch project files, target directories, container
data, clean-event history, or historical error records. It only removes
discovery and blocking state that is now outside configured scope.

For the current installation, whose state was inspected on 2026-07-27, the
first successful scan after adding `$HOME/OrbStack` will remove the 8,728
cached false-positive project rows and 14 active OrbStack
worktree-discovery failures. Subsequent scans will not recreate them.

## Error Handling

- An excluded directory is silently pruned; its unreadability is irrelevant
  because the scanner must not attempt to read it.
- Failure to resolve an optional home path does not fail configuration
  loading. The lexical absolute path is still usable by the matcher.
- State reconciliation is transactional so partially removed worktree
  provenance cannot remain after an error.
- Errors outside excluded roots continue to be recorded and continue to
  participate in fail-closed safety review.
- A user who opts a managed root back in receives the existing scan and
  worktree diagnostics for that root.

## Testing

Tests will cover:

1. Exact common, macOS, and Linux profiles through a platform-parameterized
   helper so both profiles are verified on every CI host.
2. Home anchoring: `$HOME/Library` is excluded while
   `$HOME/code/Library` remains discoverable.
3. Pre-traversal pruning: an excluded tree containing Cargo manifests, Git
   repositories, and unreadable directories yields no candidates, no
   worktree calls, and no scan errors.
4. Ordinary projects outside excluded roots remain discoverable and
   cleanable.
5. Explicit `excludes` configuration continues to replace defaults rather
   than merge with them.
6. Reconciliation removes excluded project rows, linked-worktree
   associations, and active discovery failures while preserving unrelated
   state and historical diagnostics.
7. An OrbStack-shaped fixture yields zero findings and never invokes the
   cleaner.
8. Existing safety-classification tests continue to pass when managed roots
   are deliberately opted back into scanning.

## Documentation and Release

The configuration reference will list the default profiles, explain that
defaults are editable, and distinguish discovery candidates from valid
cleanup targets. The README will retain the short statement that `$HOME` is
scanned by default and link to the configuration reference for the detailed
lists.

This change is suitable for a patch release because it narrows default
discovery without broadening cleanup authority. The current user's config is
updated alongside installation of that release, followed by a successful
scan and verification that OrbStack contributes zero cached candidates.

## Reference Locations

- Docker Desktop for Mac stores its VM data beneath
  `~/Library/Containers/com.docker.docker`:
  <https://docs.docker.com/desktop/troubleshoot-and-support/faqs/macfaqs/>
- Colima defaults `COLIMA_HOME` to `$HOME/.colima`:
  <https://github.com/abiosoft/colima/blob/main/docs/FAQ.md>
- Lima defaults `LIMA_HOME` to `$HOME/.lima`:
  <https://lima-vm.io/docs/dev/internals/>
- Rancher Desktop uses
  `~/Library/Application Support/rancher-desktop` on macOS and
  `~/.local/share/rancher-desktop` on Linux:
  <https://docs.rancherdesktop.io/how-to-guides/provisioning-scripts/>
- Rootless Podman uses `$HOME/.local/share/containers/storage` by default:
  <https://docs.podman.io/en/v4.0.0/markdown/podman.1.html>
