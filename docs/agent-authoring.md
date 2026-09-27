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

`deck review` writes one 960×540 PNG per slide, `contact-sheet.png`, and `report.json`. Its stdout is the same JSON report. Each finding names a slide and the relevant element IDs. `ok: false` means an agent should inspect the images and fix the reported clipping, overlap, or missing picture; the command still exits successfully so scripts can read the report. The renderer uses software WebKitGTK in an offscreen GTK window. It needs a graphical session but opens no visible browser window.

## Agent loop

1. Read the brief and `hyperframe-slides schema json` for portable deck creation, or `schema source` for compact local edits. Give each slide one clear claim and include speaker notes when useful.
2. Use `deck new`, `slide add`, and the template commands, or import a portable version 1 JSON file. Use `deck bundle ID DIR` when an agent needs ordinary picture and font files.
3. For a saved deck, run `deck source ID`. Edit the returned `deck` object and apply it with `deck put-source FILE --if-revision REVISION`. The app rejects a stale revision. Add media through `image add`, `template logo`, or `template font`; the app creates checked asset files.
4. Run `deck validate ID`, then `deck render ID SLIDE_ID DIR` for each changed slide. Inspect its PNG and findings. Run `deck review ID DIR` for the whole deck before presenting. A clean report cannot judge narrative quality, brand fit, or factual accuracy.
5. Use `deck history ID` and revision-safe `deck restore` to recover a previous saved version when needed. The presenter decides when the deck is ready to show.

The JSON schemas describe the portable and local document shapes. The app also checks asset signatures and digests, unique slide IDs, and picture coordinates whose position plus size must remain within the slide. See the [storage guide](storage.md) for migration and bundle details.
