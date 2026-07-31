# Draft Smoke Generation Design

## Problem

The authenticated draft release smoke test invokes `health --skip-cargo`
against a fresh isolated state directory before running discovery. car-go-clean
0.4.0 correctly reports that state as incomplete with exit code 2 because no
discovery generation exists. The shell runs with `set -e`, so all four draft
smoke jobs stop before exercising the installer and Homebrew paths.

The release rehearsal does not have this mismatch: it runs `scan` before
`health`, which models a valid first-run sequence.

## Design

Update only the authenticated draft smoke workflow. After writing its isolated
configuration, run `scan` with the same `HOME`, XDG paths, configuration, and
state directory used by the following health check. Then retain the existing
`health --skip-cargo` command as an exit-zero assertion that the newly created
generation is valid.

Do not change runtime exit codes or weaken the health assertion. Exit code 2
for missing generations is intentional operator-facing behavior.

## Regression Coverage

Add a packaging test that inspects the `Verify authenticated draft install
paths` step and requires:

- an isolated `scan` command;
- the existing `health --skip-cargo` command; and
- `scan` to appear before `health`.

Run the focused packaging test, then the repository's complete formatting,
Clippy, locked-test, and release-script verification before replacing the
unpublished draft and tag.

## Release Recovery

The failed release is still a private draft and neither protected publication
environment was approved. After the fix passes CI and the exact release
rehearsal, delete the unpublished draft and tag, recreate the annotated tag at
the corrected commit, and rerun the release from its first stage.
