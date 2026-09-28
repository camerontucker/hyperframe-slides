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

Use `image background DECK_ID SLIDE_ID IMAGE_ID on` to make an added picture fill the main slide area behind editable text. Only one background picture is allowed per slide; turn it off to position it as a normal picture.

Choose `contrast` as a slide layout for two editable text panels over one background picture. Its headline appears on the left and supporting text on the right.

Choose `quiz` and add `revealOptions` (up to three strings) for staged answer choices. Each Next press reveals one choice; another press advances after the last choice. Review images show all options for layout inspection.

Set the same `outlineGroup` on adjacent slides to keep one major heading highlighted in the left presentation outline across the group. The active heading shows progress through the group's slides. Omit the field to derive the heading from the eyebrow text.
