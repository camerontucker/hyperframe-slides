# Author decks with an agent

The app stores each deck as a compact JSON manifest and separate media files. An agent can create or change it through the CLI, then render a visual review without opening the editor. A person can adjust the result in GTK and present it in Zoom. Run `hyperframe-slides skill` for a short reusable authoring guide or `hyperframe-slides skill install` to place it in `~/.agents/skills/hyperframe-slides/`.

## Example: brief to reviewed deck

[`agent-demo-brief.md`](../examples/agent-demo-brief.md) is the source brief. [`agent-demo-deck.json`](../examples/agent-demo-deck.json) is a three-slide response with a shared template, concise text, speaker notes, and text-only entrance motion.

```bash
hyperframe-slides schema json > deck.schema.json
hyperframe-slides deck put examples/agent-demo-deck.json > created.json
DECK_ID=$(jq -r .id created.json)
hyperframe-slides deck validate "$DECK_ID"
hyperframe-slides deck review "$DECK_ID" ./review-agent-demo
jq -e '.ok' ./review-agent-demo/report.json
```

`deck review` writes one 960×540 PNG per slide, `contact-sheet.png`, and `report.json`. The report records the deck revision, each slide's content hash, and relative image paths so the review folder can move. Stdout also gives the absolute output directory. Each finding names a slide and the relevant element IDs. `ok: false` means an agent should inspect the images and fix the reported clipping, overlap, or missing picture; the command still exits successfully so scripts can read the report. The renderer uses software WebKitGTK in an offscreen GTK window. It needs a graphical session but opens no visible browser window.

## Agent loop

1. Read the brief and `hyperframe-slides schema json` when creating a new deck. Give each slide one clear claim and include speaker notes when useful.
2. Use scoped `slide`, `image`, and `template` commands for small changes. For substantial edits, save `deck revision ID`, export `deck bundle ID DIR`, edit its `presentation.json` and media files, then run `deck apply-bundle ID DIR --if-revision HASH`. A stale revision is rejected; the previous version remains in history. Use `deck import-bundle DIR` only to create a separate copy.
3. `deck source ID` and `deck put-source` expose the compact local storage format for advanced integrations. It is not the normal authoring format.
4. Run `deck validate ID`, then `deck render ID SLIDE_ID DIR` for each changed slide. Inspect its PNG and findings. Run `deck review ID DIR` for the whole deck before presenting. A clean report cannot judge narrative quality, brand fit, or factual accuracy.
5. Run `deck history ID`, then `deck diff ID HISTORY_HASH ./changes` to compare a saved version with the current deck. `changes/diff.json` lists changed, added, and removed slides with relative before/after PNG paths and layout findings. Review the images, then use `deck revert-slide ID HISTORY_HASH SLIDE_ID --if-revision CURRENT_HASH` to restore one slide if needed. The command rejects a stale current revision. **Menu → Review changes** offers the same visual comparison and single-slide revert in GTK. Use **Menu → Version history** or `deck restore ID HISTORY_HASH --if-revision CURRENT_HASH` to recover the entire deck. The presenter decides when the deck is ready to show.

The JSON schemas describe the portable and local document shapes. The app also checks asset signatures and digests, unique slide IDs, and picture coordinates whose position plus size must remain within the slide. See the [storage guide](storage.md) for migration and bundle details.

To place a photograph behind editable text, add it with `image add`, then run `image background DECK_ID SLIDE_ID IMAGE_ID on`. The picture fills the main slide area and receives a dark scrim for readable text. Each slide supports one background picture; `image background ... off` returns it to a normal, movable picture.

The `contrast` layout puts the headline and supporting text in separate left and right panels over a background image. Use it for side-by-side comparisons whose words must remain editable.

For a question with staged answers, set `layout` to `quiz` and add up to three strings in `revealOptions`. The audience window enters with all choices hidden; each Next press reveals one, then the following Next press advances. The editor's **Reveal options** field uses one choice per line. Static review images show all choices so an agent can check their placement.

To condense the left outline, set `outlineGroup` to the same major heading on adjacent slides. They appear as one section in the audience outline, and the active section remains highlighted until its last slide. The current slide's position within that section appears beside the heading. When `outlineGroup` is omitted, the app derives a heading from the eyebrow before `/` or `·`.

Use `slide sound DECK_ID SLIDE_ID modem` for an offline modem connection sound on slide entry, or `none` to clear it. It stops on navigation, replays when revisiting, and follows the presentation mute control.

Agents can mute or unmute a live presentation with `present sound SESSION off` or `on`.

### Bullet reveals

Enable **Fade in bullets one at a time (click / Space)** for a slide in the editor, or run `hyperframe-slides slide bullets DECK_ID SLIDE_ID on` (`off` disables it). The optional `revealBullets` boolean defaults to false. Each click on the slide, Space, Right arrow, or CLI `present next` reveals one top-level Markdown list item, including its nested supporting bullets. After the last item, Next advances to the next slide. Static previews and PDF export show all items.

### Single PDF export

`hyperframe-slides deck export-pdf DECK_ID OUTPUT.pdf` exports one landscape page per slide using native WebKit printing. Text stays selectable/searchable, fonts and shapes remain vector, and images are resized and compressed in the export copy to keep the PDF compact. All bullet items and reveal choices are visible; notes and audio are omitted. A desktop session is required for WebKit snapshots.

Repeat `--exclude SLIDE_ID` to omit slides from this export. The original deck is unchanged; page numbers and the grouped outline reflect the exported selection. Unknown IDs and excluding every slide are errors. Output is JSON with the absolute path, page count, source revision, included/excluded slide IDs and rendering warnings. The PDF is saved atomically with private file permissions. Supporting text is reduced slightly when needed to keep it clear of the footer and page number; the editable deck remains unchanged.

```bash
hyperframe-slides deck export-pdf my-deck ./presentation.pdf --exclude quiz-slide
```
