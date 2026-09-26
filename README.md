# HyperFrames Slides

A native Rust/GTK presentation editor for Omarchy. It saves decks as local JSON, exports real HyperFrames slideshow compositions, and opens the HyperFrames presenter in a WebKitGTK window. No Chromium window or browser tab is required.

## Install

System components: GTK 3 and WebKitGTK 4.1. Rust/Cargo and Node's `npx` are needed to build and run HyperFrames. The installer uses the system's `gst-plugins-good` when available; on the current Omarchy GStreamer version it can install the missing WebKitGTK audio plugin under your user data directory.

```bash
./install.sh
```

Launch **HyperFrames Slides** from Omarchy's app launcher, or run `~/.local/bin/hyperframe-slides`.

## Use with Zoom

1. Create a deck. Add slides, choose a layout and theme, and write speaker notes.
2. Click **Present** (or press Ctrl+P). This opens a native WebKitGTK presenter window.
3. Click **Audience** in the presenter toolbar to open a separate audience window.
4. In Zoom, share the **audience window**. Keep it visible, preferably on a second monitor, while controlling slides from the presenter window.

Decks are saved in `${XDG_DATA_HOME:-~/.local/share}/hyperframe-slides/decks/`. The **Export HTML** button saves a HyperFrames composition. For CLI use, `hyperframe-slides --export DECK_ID DIRECTORY` writes `DIRECTORY/index.html`. Run `npx hyperframes present DIRECTORY` to present that export elsewhere.

The app targets HyperFrames CLI `0.8.78` for live presentations. Presenter notes edited inside HyperFrames' presenter view are held by that view; edit them in the GTK editor to save them with the deck.
