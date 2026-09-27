# Deck storage and recovery

Current local decks use a version 2 JSON manifest at `decks/DECK_ID.json` and media files under `decks/DECK_ID.assets/`. The `src`, logo, and font paths are relative to `decks/`. Asset names contain a SHA-256 digest, and the app checks the file type, size, digest, and path before loading it. Images and fonts are never fetched from the network.

Version 1 decks with embedded data URIs still open. The next save converts one to version 2. To convert explicitly, run `hyperframe-slides deck migrate DECK_ID`. Before replacing a manifest, the app retains its prior version under `history/DECK_ID/`. The last 30 saved revisions are kept. Asset files are retained so older revisions remain restorable; they are not currently garbage collected.

There are three file representations: the logical **deck**, private **local storage** (version 2), and a portable editable **bundle**. A **snapshot** is a self-contained version 1 JSON copy. The in-memory deck currently uses logical schema 1; local `schemaVersion: 2` identifies its file-backed storage encoding, not a newer logical deck model. A future logical schema change will need its own version distinction. Agents should use scoped CLI commands for small changes and bundles for larger edits. `deck source` / `deck put-source` expose local storage for advanced integrations only. The source must refer to existing, valid assets. `deck snapshot ID` and `deck get ID` emit embedded JSON for compatibility. Both JSON schemas are in `schema/`.

`deck bundle ID DIR` creates a separate editable folder:

```text
DIR/
  presentation.json
  assets/
  fonts/
```

The manifest contains `format: "hyperframe-slides-bundle-v1"` and a `deck` object whose media fields point to files in the folder. Exported media has checksum filenames; agents may add files with simple names such as `assets/chart.png` and reference them from the manifest. A bundle is an editable project, not an integrity-protected package: referenced files are checked for safe paths, type, and size; checksum filenames are also checked against their bytes. On apply, the app copies media into content-addressed local storage, where identity is checked on every read. Record `deck revision ID` before editing. `deck apply-bundle ID DIR --if-revision HASH` validates the bundle, requires the matching deck ID and unchanged target revision, then saves the previous manifest in history. It checks the revision while holding the document lock before reading bundle media. `deck import-bundle DIR` creates a separate local deck. Bundle export refuses an existing destination directory.

Run `deck history ID` or **Menu → Version history** to list saved revisions. Compare with `deck diff ID HISTORY_HASH [DIR]` or **Menu → Review changes**; the optional directory renders before/after PNGs and a `diff.json` report with relative paths and layout findings. Restore one slide with `deck revert-slide ID HISTORY_HASH SLIDE_ID --if-revision CURRENT_HASH`, or the whole deck with `deck restore ID HISTORY_HASH --if-revision CURRENT_HASH`. Both save the current version in history before writing and reject a stale current hash. A bundle or embedded version 1 JSON export is still the best long-term portable backup because local history depends on the matching asset directory.

Saving writes immutable content-addressed assets before replacing the manifest. A committed manifest therefore references assets already on disk; a failed save may leave unreferenced assets. Future garbage collection must keep every asset referenced by the current manifest or any retained history manifest, and remove only unreferenced assets after checking both. No asset deletion runs today.
