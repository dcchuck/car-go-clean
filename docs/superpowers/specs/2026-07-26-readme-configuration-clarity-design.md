# README clarity design

## Goal

Make the GitHub README easier to scan by removing the logo's excess visual
padding and moving advanced configuration semantics out of the front page.

## Header

- Keep the original logo artwork unchanged.
- Create a cropped README-specific logo asset that contains the crab and wordmark
  with only a small visual margin.
- Point the centered README image at that asset and reduce its display width so
  the opening header reads as a compact project identity rather than a hero.

## Documentation split

The README remains the quick-start surface:

- optional configuration, default location, and minimal TOML example;
- short bullets explaining scan roots, linked worktrees, exclusions, and the
  conservative behavior on discovery failure;
- the existing concise safe-cleaning model and common commands;
- a prominent link to `docs/configuration.md` for operational detail.

`docs/configuration.md` becomes the detailed reference, organized into:

1. config file location and fields;
2. scan roots, explicit project directories, and exclusions;
3. linked-worktree discovery and durable failure blocking;
4. clean-safety gates and review/override commands;
5. state, logs, and scheduler persistence.

The moved text must retain the existing safety guarantees; this is a structural
and editorial change, not a relaxation of cleanup behavior.

## Validation

- README references the cropped logo and the new configuration guide.
- The guide preserves the documented configuration paths, worktree safety
  semantics, state location, and log behavior.
- Markdown links resolve to tracked repository files.
