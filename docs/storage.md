# Deck storage and recovery

Current local decks use a version 2 JSON manifest at `decks/DECK_ID.json` and media files under `decks/DECK_ID.assets/`. The `src`, logo, and font paths are relative to `decks/`. Asset names contain a SHA-256 digest, and the app checks the file type, size, digest, and path before loading it. Images and fonts are never fetched from the network.

Version 1 decks with embedded data URIs still open. The next save converts one to version 2. To convert explicitly, run `hyperframe-slides deck migrate DECK_ID`. Before replacing a manifest, the app retains its prior version under `history/DECK_ID/`. The last 30 saved revisions are kept. Asset files are retained so older revisions remain restorable; they are not currently garbage collected.

For agent editing, use `deck source ID` to get the compact manifest and revision, edit its `deck` object, then run `deck put-source FILE --if-revision HASH`. The source must refer to existing, valid assets. Use `image add`, `template logo`, or `template font` to add media. `deck snapshot ID` and `deck get ID` emit embedded version 1 JSON for portability and compatibility. Both JSON schemas are in `schema/`.

`deck bundle ID DIR` creates a separate editable folder:

```text
DIR/
  presentation.json
  assets/
  fonts/
```

The manifest contains `format: "hyperframe-slides-bundle-v1"` and a `deck` object whose media fields point to files in the folder. `deck import-bundle DIR` validates every reference and imports a new local deck. It does not overwrite the source deck. Bundle export is atomic and refuses an existing destination directory.

Run `deck history ID` to list saved revisions. Restore with `deck restore ID HISTORY_HASH --if-revision CURRENT_HASH`; the current version is saved in history before restore. A stale current hash is rejected. A bundle or embedded version 1 JSON export is still the best long-term portable backup because local history depends on the matching asset directory.
