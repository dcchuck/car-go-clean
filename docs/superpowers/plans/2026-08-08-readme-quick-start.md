# README Quick Start and Legacy Upgrade Guide Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give new users a compact README quick start while preserving the v0.2/v0.3 upgrade helper in a dedicated guide.

**Architecture:** The README becomes a first-use guide from installation through reviewed cleanup. A new migration document becomes the single home for the historical helper procedure; both v0.4 release notes link to it rather than duplicate its long instructions. This is a documentation-only change: command semantics and release assets are unchanged.

**Tech Stack:** CommonMark Markdown, shell-based repository checks, Cargo test harness.

## Global Constraints

- The README opening must target fresh installations and first reviewed cleanup.
- `README.md` must contain no `v0.2`, `v0.3`, or `upgrade helper` references.
- The quick-start commands must use `run --dry-run --all`, followed by `run --review REVIEW_ID` and `stats`; it must say that `REVIEW_ID` is printed by the preview.
- The existing Homebrew formula and checksum-verifying shell installer remain the only documented normal installation sources.
- The dedicated guide must preserve the existing helper's method-ownership, service-state, recovery, rollback, and configuration-migration constraints.
- The v0.4.0 and v0.4.1 release notes must link to `docs/upgrading-v0.2-v0.3.md` for the historical migration path.
- Do not change Rust code, installation scripts, service behavior, or upgrade-helper behavior.

---

### Task 1: Establish the Canonical Legacy Migration Guide

**Files:**
- Create: `docs/upgrading-v0.2-v0.3.md`
- Modify: `docs/releases/v0.4.0.md:52-154`
- Modify: `docs/releases/v0.4.1.md:16-21`

**Interfaces:**
- Consumes: the approved helper and configuration-migration prose in `docs/releases/v0.4.0.md`.
- Produces: a standalone `docs/upgrading-v0.2-v0.3.md` target referenced by both release-note files.

- [x] **Step 1: Extract the helper procedure into the new guide**

  Create `docs/upgrading-v0.2-v0.3.md` with the title `# Upgrading from v0.2 or v0.3 to v0.4` and this opening paragraph:

  ```markdown
  This guide is only for an installed `v0.2.0` or `v0.3.0` binary. Use the
  v0.4 migration helper below to preserve its configuration, state, service,
  logs, and cleanup history.
  ```

  Copy the current `## v0.2/v0.3 upgrade helper` and `## Configuration
  migration` content from `docs/releases/v0.4.0.md:52-154` verbatim below
  that paragraph, preserving every code block and paragraph. This retains the
  verified downloads, both ownership methods, the two helper phases, recovery
  and rollback guidance, and the `excludes` migration command without
  rewording the operational procedure.

- [x] **Step 2: Replace duplicate v0.4.0 release-note procedure with its canonical link**

  Replace the `## v0.2/v0.3 upgrade helper` and `## Configuration migration` sections (current lines 52-154) with:

  ```markdown
  ## v0.2/v0.3 upgrade helper

  For the state-preserving migration from v0.2.0 or v0.3.0, follow the
  [legacy upgrade guide](../upgrading-v0.2-v0.3.md).
  ```

  Leave the `Service operation` and `Release compatibility gate` sections in place.

- [x] **Step 3: Link the v0.4.1 compatibility note to the guide**

  Replace the final two sentences of `docs/releases/v0.4.1.md`'s `## Upgrade` section with:

  ```markdown
  For a normal same-line update, stop an active daemon, upgrade with the method
  that owns `car-go-clean`, then generate a fresh dry-run review before cleaning.
  For a v0.2/v0.3-to-v0.4 migration, use the
  [legacy upgrade guide](../upgrading-v0.2-v0.3.md).
  ```

- [x] **Step 4: Verify guide extraction and release-note links**

  Run:

  ```sh
  test -f docs/upgrading-v0.2-v0.3.md
  rg -n "legacy upgrade guide|upgrading-v0\.2-v0\.3\.md" \
    docs/releases/v0.4.0.md docs/releases/v0.4.1.md
  rg -n -- "--method homebrew|--method shell|--execute-review REVIEW_ID|config migrate" \
    docs/upgrading-v0.2-v0.3.md
  ```

  Expected: both release notes link to the new guide, and the guide retains each helper phase, both ownership methods, and configuration migration.

- [x] **Step 5: Commit the migration documentation**

  ```sh
  git add docs/upgrading-v0.2-v0.3.md docs/releases/v0.4.0.md docs/releases/v0.4.1.md
  git commit -m "docs: separate legacy upgrade guide"
  ```

### Task 2: Replace the README Opening with a New-User Quick Start

**Files:**
- Modify: `README.md:9-272`

**Interfaces:**
- Consumes: the published Homebrew formula, verified installer URL, review-plan semantics, and existing `Background Service (Optional)` section.
- Produces: a self-contained `Quick Start` section and an Agent Quick Start that omit legacy-upgrade instructions.

- [x] **Step 1: Rewrite `Install` and `Quick Start` as one compact `Quick Start` section**

  Replace the current `## Install` and `## Quick Start` sections (lines 9-160) with `## Quick Start`. Use this installation lead-in:

  ````markdown
  Install with Homebrew:

  ```sh
  brew install dcchuck/tap/car-go-clean
  ```

  Or use the checksum-verifying installer:

  ```sh
  curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/dcchuck/car-go-clean/releases/latest/download/car-go-clean-installer.sh | sh
  export PATH="$HOME/.local/bin:$PATH"
  hash -r 2>/dev/null || true
  ```
  ````

  Retain one short sentence that the installer verifies a matching release
  archive before replacing the binary and does not require `sudo`. Follow the
  installation lead-in with this command sequence:

  ```sh
  car-go-clean version
  car-go-clean run --dry-run --all

  # After reviewing the preview:
  car-go-clean run --review REVIEW_ID
  car-go-clean stats
  ```

  Explain that the dry run scans without invoking Cargo, emits the numeric
  `REVIEW_ID`, expires after 30 minutes, and that reviewed execution can remove
  targets that become unsafe but cannot add targets not shown in the review.
  State that the install does not start a daemon and link readers to
  `Background Service (Optional)` for that separate choice. Do not add
  historic-version checks, `service stop` guidance, exit-code protocol, JSON
  detail, or cached-only details to this section.

- [x] **Step 2: Simplify Agent Quick Start to the normal path**

  Remove the legacy-version and helper-specific paragraphs from the quoted prompt (current lines 187-206 and the helper exception at lines 252-258). Retain the normal safety posture: inspect the environment and existing installation; use only the two official sources; verify the installed binary; check `service status` and `health`; request approval before a persistent service change or reviewed cleanup; use `run --review REVIEW_ID`, not dynamic bare `run`.

  Replace version-specific existing-install instructions with one concise rule: read the target release's upgrade notes before replacing an existing binary. The prompt must not contain `v0.2`, `v0.3`, or `upgrade helper`.

- [x] **Step 3: Confirm the README is concise and legacy-free**

  Run:

  ```sh
  rg -n "^## (Quick Start|Agent Quick Start|Background Service \(Optional\))" README.md
  if rg -n -i "v0\.2|v0\.3|upgrade helper" README.md
  then
    exit 1
  fi
  rg -n -- "run --dry-run --all|run --review REVIEW_ID|car-go-clean stats" README.md
  ```

  Expected: `Quick Start` is the first README section after the introduction, no legacy phrases are present, and the reviewed cleanup flow appears in the README.

- [x] **Step 4: Commit the README rewrite**

  ```sh
  git add README.md
  git commit -m "docs: streamline readme quick start"
  ```

### Task 3: Run Documentation-Focused Verification

**Files:**
- Verify: `README.md`
- Verify: `docs/upgrading-v0.2-v0.3.md`
- Verify: `docs/releases/v0.4.0.md`
- Verify: `docs/releases/v0.4.1.md`
- Test: `tests/release-notes.sh`
- Test: `tests/packaging.rs:114-125`

**Interfaces:**
- Consumes: the final documentation files and repository release-note composition script.
- Produces: evidence that the documentation is structurally sound and does not change packaging contracts.

- [x] **Step 1: Check Markdown whitespace and document references**

  Run:

  ```sh
  git diff --check HEAD~2..HEAD
  test -f docs/upgrading-v0.2-v0.3.md
  rg -n "\]\(\.\./upgrading-v0\.2-v0\.3\.md\)" \
    docs/releases/v0.4.0.md docs/releases/v0.4.1.md
  ```

  Expected: no whitespace errors, the guide exists, and each release note has a relative link to it.

- [x] **Step 2: Run applicable repository checks**

  Run:

  ```sh
  make test-release-notes
  cargo test --locked --test packaging readme_uses_compact_logo_asset
  ```

  Expected: the generated v0.4.1 release notes still exactly match their source, and the README continues to use the required compact logo markup.

- [ ] **Step 3: Inspect the final diff and repository state**

  Run:

  ```sh
  git diff --check HEAD~2..HEAD
  git status --short
  git log --oneline -3
  ```

  Expected: no whitespace errors, a clean working tree, and commits for the design, legacy guide, and README rewrite.
