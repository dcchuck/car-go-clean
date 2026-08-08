# README Quick Start and Legacy Upgrade Guide

**Status:** Approved

## Goal

Make the README's opening useful to a first-time user. It should show the
normal install and first reviewed cleanup path without leading with support
for historic installations. Preserve the v0.2/v0.3-to-v0.4 migration helper
as a dedicated reference for the sole existing user who may need it.

## Scope

- Replace the README's current opening `Install` and `Quick Start` material
  with a concise new-user Quick Start: install with Homebrew or the verified
  shell installer, verify the binary, make a dry-run review, execute that
  review with its printed ID, and inspect reclaimed-space statistics.
- Keep the surrounding safety notes only where they directly explain those
  commands: a review expires after 30 minutes, `run --review` executes only
  the inspected plan, and installation does not start the optional service.
- Remove v0.2/v0.3 checks, upgrade-helper warnings, and legacy migration
  instructions from the README, including the Agent Quick Start prompt.
- Create `docs/upgrading-v0.2-v0.3.md` as the canonical v0.2/v0.3 migration
  guide, extracting the complete helper procedure and safety constraints now
  embedded in the v0.4.0 release notes.
- Replace the detailed upgrade-helper section in `docs/releases/v0.4.0.md`
  with a short link to the new guide. The v0.4.1 release note may retain its
  one-sentence compatibility reference, but should link to the guide.

## README Structure

The README will begin with the project identity followed by `## Quick Start`.
The section will offer Homebrew first and the verified installer as an
alternative, then show the exact safe first-use commands:

```sh
car-go-clean version
car-go-clean run --dry-run --all
car-go-clean run --review REVIEW_ID
car-go-clean stats
```

The copy will tell readers to replace `REVIEW_ID` with the numeric identifier
printed by the dry run. It will keep the service optional and defer its
installation and operation to the existing `Background Service (Optional)`
section. The existing detailed explanations of policy, JSON output, exit
codes, and advanced cached-only operation remain in their focused sections.

## Legacy Guide

`docs/upgrading-v0.2-v0.3.md` will contain the migration-helper instructions
for v0.2.0 and v0.3.0 installations, including method ownership validation,
the two helper phases, service-state preservation and recovery, rollback,
and the configuration-key migration. It will explicitly target the v0.4
migration helper rather than imply that routine same-line updates need that
procedure.

The v0.4.0 release notes will retain their summary of v0.4 behavior but send
readers needing a legacy upgrade to this canonical guide, eliminating two
independently maintained procedural copies.

## Verification

- Review the rendered Markdown hierarchy and command flow in the README.
- Confirm no `v0.2`, `v0.3`, or `upgrade helper` references remain in
  `README.md`.
- Confirm each release-note link resolves to the new guide.
- Run the repository's applicable documentation/content checks, if provided.

## Non-Goals

- No CLI, installer, service, or cleanup-policy behavior changes.
- No change to routine upgrade guidance beyond linking legacy users to the
  dedicated guide.
- No attempt to support cross-method migration outside the existing helper's
  documented constraints.
