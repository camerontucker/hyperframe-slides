# Author decks with an agent

The app stores each deck as one local JSON document. An agent can create or change that document through the CLI, then render a visual review without opening the editor. A person can adjust the result in GTK and present it in Zoom.

## Example: brief to reviewed deck

[`agent-demo-brief.md`](../examples/agent-demo-brief.md) is the source brief. [`agent-demo-deck.json`](../examples/agent-demo-deck.json) is a three-slide response with a shared template, concise text, speaker notes, and text-only entrance motion.

```bash
hyperframe-slides schema json > deck.schema.json
hyperframe-slides deck import examples/agent-demo-deck.json > imported.json
DECK_ID=$(jq -r .id imported.json)
hyperframe-slides deck validate "$DECK_ID"
hyperframe-slides deck review "$DECK_ID" ./review-agent-demo
jq -e '.ok' ./review-agent-demo/report.json
```

`deck review` writes one 960×540 PNG per slide, `contact-sheet.png`, and `report.json`. Its stdout is the same JSON report. Each finding names a slide and the relevant element IDs. `ok: false` means an agent should inspect the images and fix the reported clipping, overlap, or missing picture; the command still exits successfully so scripts can read the report. The renderer uses software WebKitGTK in an offscreen GTK window. It needs a graphical session but opens no visible browser window.

## Agent loop

1. Read the brief and `hyperframe-slides schema json`. Give each slide one clear claim and include speaker notes when useful.
2. Write a deck JSON file or use `deck new`, `slide add`, and the template commands. Use `deck import` for an external JSON file; it assigns a new ID if the original ID is already in the library.
3. Run `deck validate ID`, then `deck review ID DIR`. Inspect the contact sheet and any affected slide PNGs, including when the report is clean: the checks cannot judge narrative quality, brand fit, or whether a layout feels crowded.
4. To revise a stored deck, call `deck snapshot ID` and use its `revision` with `deck put FILE --if-revision REVISION` or `slide set`. The app rejects a stale revision rather than overwriting a newer edit.
5. Run review again after edits. The presenter decides when the deck is ready to show.

The JSON Schema describes the document shape. The app's validator also checks facts the schema cannot express conveniently, such as embedded asset signatures, unique slide IDs, and picture coordinates whose position plus size must remain within the slide.
