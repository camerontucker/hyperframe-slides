---
name: hyperframe-slides
description: Create, revise, validate, and visually review local HyperFrames Slides presentations through the CLI.
---

# HyperFrames Slides

1. Run `hyperframe-slides help authoring` for the current workflow and `hyperframe-slides schema json` for the deck shape.
2. Use `deck snapshot ID` before changing a saved deck. Pass its revision to `deck put` or `slide set` so a newer editor or agent change is never overwritten.
3. Use `deck bundle ID DIR` when you need readable JSON and separate image/font files. Edit the bundle and use `deck import-bundle DIR` to bring it back as a new deck.
4. Run `deck validate ID`, then `deck render ID SLIDE_ID DIR` for each changed slide. Inspect the PNG and JSON findings.
5. Run `deck review ID DIR` for the whole deck before presenting. Treat a clean report as a layout check, not a content or factual review.
6. Present only after a human approves the narrative and audience-safe content.
