---
name: hyperframe-slides
description: Create, revise, validate, and visually review local HyperFrames Slides presentations through the CLI.
---

# HyperFrames Slides

1. Run `hyperframe-slides help authoring` for the current workflow and `hyperframe-slides schema json` for the deck shape.
2. For a small change, use a scoped `slide`, `image`, or `template` command. The app applies it to the current deck under a document lock.
3. For substantial editing, run `deck revision ID`, then `deck bundle ID DIR`. Edit `presentation.json` and its media files. Apply it with `deck apply-bundle ID DIR --if-revision HASH`; a stale revision is rejected and the old version enters history. Use `deck import-bundle DIR` only when creating a separate copy.
4. `deck source` and `deck put-source` expose advanced local storage integration; ordinary authoring should use scoped commands or bundles.
5. Run `deck validate ID`, then `deck render ID SLIDE_ID DIR` for each changed slide. Inspect the PNG and JSON findings. Run `deck review ID DIR` for the whole deck before presenting.
6. Compare with history using `deck diff ID HISTORY_HASH DIR`; inspect `DIR/diff.json` and its before/after PNGs. Restore one slide with `deck revert-slide ID HISTORY_HASH SLIDE_ID --if-revision CURRENT_HASH` after confirming the current revision. The GTK editor has **Menu → Review changes** for the same comparison.
7. Treat a clean report as a layout check. A human approves the narrative and audience-safe content before presenting.
