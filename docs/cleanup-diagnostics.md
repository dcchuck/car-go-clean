# Diagnosing retained build artifacts

`projects --all --json` explains discovery and current eligibility for cached
projects. `scan` refreshes discovery; `run --dry-run --all` makes a fresh review
without deleting outputs. Both write local state. `health` and `status` show
recorded unresolved cleanup failures as advisory diagnostics. Their exit codes
retain the existing discovery/authority semantics; a complete outcome does not
mean all previous cleanups succeeded. Their
failure history is not a statement that a path still exists or has the same
contents. State inspection commands currently open/migrate the state database;
for a strictly read-only investigation, use a SQLite online backup and pass its
separate directory with `--state-dir`.

## Missing or invalid CACHEDIR.TAG

Cargo validates explicitly selected target directories in recent toolchains.
car-go-clean deliberately passes `--target-dir` to constrain cleanup to the
reviewed direct target. A missing marker is not evidence that a directory is
safe to delete. Empty mountpoints, copied artifacts, partial builds, foreign
files, and legacy outputs need different treatment.

Inspect the selected toolchain, target ownership and filesystem identity,
symlinks, ancestor/project Cargo config, environment target/build-dir overrides,
and whether other projects or containers share the target. Cargo-shaped files
alone do not prove exclusive ownership. Never manufacture a marker, downgrade
Cargo, remove the explicit target argument, or use `rm -rf` to bypass refusal.
After verifying an exclusive Cargo output, the owning project's normal build
workflow may restore its cache metadata. Check the resulting marker and obtain
a fresh cleanup review; this is not guaranteed to repair an invalid existing
marker. Removal or repair of existing outputs needs a separately approved plan.

Truly empty direct directories are left alone. Nonempty failures retain Cargo's
stderr and retry on the ordinary cleanup schedule; there is no rapid retry loop
or automatic repair. There is currently no acknowledgement command to suppress
a historical unresolved failure. A successful measured cleanup for the same
path resolves the diagnostic; it does not affect the health/status exit code.
Do not trigger builds or deletion merely to clear historical diagnostics.

## Discovery and activity

Scanning respects `.gitignore`, exclusions, authority roots and symlink
boundaries. Scanning stops at the first Cargo manifest, including when a nested
project has its own target. Explicit project entries remain exact entries and
do not recursively grant authority to descendants. Linked Git
worktrees outside scope remain outside scope.

Only direct `<project>/target` directories are cleanup candidates. Arbitrary
`CARGO_TARGET_DIR`, Cargo `build.target-dir`, shared outputs, and separately
configured intermediate directories are not additional cleanup authority.
The command pins both target and build-dir to the reviewed direct directory.

Any process whose cwd or command references a project can block cleanup,
including UI dev servers and shells. This is intentionally conservative;
`active_process` does not mean a Rust build was identified. A recent regular-file
mtime blocks cleanup within the quiet period. Directory-only changes and
metadata-preserving copies are limitations of that heuristic. Process sampling,
identity checks and final pre-spawn validation reduce races but cannot make
arbitrary external builds atomic with cleanup. Do not use `--active` or `--force`
just because a retained target is large.

## Containers

Host targets and named Docker build volumes are distinct storage. Keep OrbStack,
Docker/BuildKit caches, databases, and container filesystems outside host cleanup.
Use the owning build-stack tooling for volume/cache lifecycle. Any future
integration should start with opt-in, read-only reporting. Destruction would
require exact owner labels and volume IDs, checks for all running/stopped
container references and builders, an expiring review, and a separate approval.
Never infer permission from a volume name or use broad volume/system pruning.

References: [Cargo validation change](https://github.com/rust-lang/cargo/pull/16712),
[Cargo clean implementation](https://doc.rust-lang.org/beta/nightly-rustc/src/cargo/ops/cargo_clean.rs.html).
