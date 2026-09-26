# HyperFrames Slides

A native Rust/GTK presentation editor for Omarchy. Slides render in WebKitGTK using bundled HyperFrames player assets. Presenting opens native presenter and audience windows without Chromium, Node, `npx`, a localhost server, or a network connection. Share the **HyperFrames Audience** window in Zoom.

![HyperFrames Slides editor with a picture, bullet points, and shared header and footer](assets/readme-editor.png)

Decks are local JSON documents in `${XDG_DATA_HOME:-~/.local/share}/hyperframe-slides/decks/`. The editor saves drafts even when they exceed presentation limits; the status bar reports when a draft cannot be presented. The preview uses the presentation's slide markup and styling and warns when text may be clipped. CLI and editor changes to the active deck sync within about a second. If a disk edit conflicts with unsaved editor work, the editor preserves its version and offers **Reload** to load the disk version.

The app limits its data directories to the current user and writes decks and exported HTML with owner-only permissions. On launch it also tightens permissions on older deck files. A deck may contain private speaker notes and embedded pictures, so review a file before sharing it.

## Install

GTK 3, WebKitGTK 4.1, GStreamer, and Rust/Cargo are required. The installer checks these dependencies, builds with the repository lockfile, and adds a desktop launcher. On GStreamer 1.28.7 it can place a missing `autoaudiosink` plugin under your user data directory after verifying a pinned package checksum. On other versions, install `gst-plugins-good` through Omarchy if the installer asks for it.

```bash
./install.sh
```

Launch **HyperFrames Slides** from Omarchy's app launcher or run `~/.local/bin/hyperframe-slides`.

To update from a source checkout, pull the latest changes and run `./install.sh` again. To remove the application and launcher while keeping your decks, run `./uninstall.sh` from the checkout.

## Author slides

- The **Supporting text** field accepts Markdown: `- item` for bullets, `1. item` for numbered lists, `**bold**`, and `*emphasis*`. The Bold, Italic, and Bullets buttons insert the corresponding markup. Headline text also supports bold and emphasis.
- **Insert picture** opens a file chooser. You can also drop a PNG, JPEG, GIF, or WebP file onto the picture drop area and drag an inserted picture in the preview to place it. Pictures are embedded in the deck and in HTML exports, so they remain available offline. The editor arranges up to eight pictures on one slide; agents can set each picture's `x`, `y`, `width`, and `height` percentages through the CLI or deck JSON.
- **Template · Every slide** sets a shared header, footer, and logo. The logo uses **Set logo** or `template logo` in the CLI. Changes appear on every slide, including the audience window.
- **Slide animation** offers None, Fade, Rise, and Zoom. Each slide stores its choice in the `animation` field.

The editor requests WebKitGTK hardware acceleration and enables WebGL. On this Omarchy machine, the native WebKit processes were verified using the AMD DRM render node. `present gpu SESSION_ID` reports live render-node use; WebKit may mask the WebGL renderer name.

## Present in Zoom

1. Create a deck, choose a layout and theme, and add speaker notes.
2. Click **Present** or press Ctrl+P.
3. Click **Audience** in the presenter window.
4. Share the **HyperFrames Audience** window in Zoom. Keep it visible, ideally on a second monitor.

The audience window follows the presenter position. Each presentation uses an immutable in-memory snapshot, so a deck edited after presentation starts cannot silently alter an ongoing session. Present again to use the updated deck. Speaker notes are available in the presenter window, but are not displayed in the audience window. Edit notes in the GTK editor or CLI to save them with the deck; changes made inside HyperFrames' presenter view remain in that view.

## Agent and CLI use

Every deck field and slide operation can be controlled through the CLI. Data commands emit JSON on stdout and errors as JSON on stderr. `-` reads JSON from standard input; `deck put` replaces a complete deck, making it suitable for agents that generate a document in one pass. Run `hyperframe-slides schema` for a JSON template and command list.

```bash
hyperframe-slides deck new "Quarterly update"
hyperframe-slides deck list
hyperframe-slides deck get DECK_ID
hyperframe-slides deck put deck.json
hyperframe-slides slide add DECK_ID slide.json
hyperframe-slides slide duplicate DECK_ID SLIDE_ID
hyperframe-slides slide set DECK_ID SLIDE_ID slide.json
hyperframe-slides slide move DECK_ID SLIDE_ID 2
hyperframe-slides slide delete DECK_ID SLIDE_ID
hyperframe-slides slide animation DECK_ID SLIDE_ID zoom
hyperframe-slides image add DECK_ID SLIDE_ID picture.png "Alt text"
hyperframe-slides image remove DECK_ID SLIDE_ID IMAGE_ID
hyperframe-slides image position DECK_ID SLIDE_ID IMAGE_ID 54 28 38 50
hyperframe-slides template header DECK_ID "Company name"
hyperframe-slides template footer DECK_ID "Confidential"
hyperframe-slides template logo DECK_ID logo.png
hyperframe-slides template logo-clear DECK_ID
hyperframe-slides deck validate DECK_ID
hyperframe-slides deck export DECK_ID ./export
hyperframe-slides deck export-audience DECK_ID ./audience-export
hyperframe-slides deck present DECK_ID
hyperframe-slides deck present DECK_ID --audience
hyperframe-slides present list
hyperframe-slides present status SESSION_ID
hyperframe-slides present gpu SESSION_ID
hyperframe-slides present inspect SESSION_ID
hyperframe-slides present inspect-audience SESSION_ID
hyperframe-slides present next SESSION_ID
hyperframe-slides present prev SESSION_ID
hyperframe-slides present goto SESSION_ID 3
hyperframe-slides present audience SESSION_ID
hyperframe-slides present audience-close SESSION_ID
hyperframe-slides present close SESSION_ID
```

`deck present` prints a `session` ID before opening the windows. An agent can run it as a background process, then use the `present` commands from another process to inspect and control the live deck. `goto` uses 1-based slide numbers. `present status` reports the audience's slide number when that window is open; `present inspect` and `present inspect-audience` report visible scenes, clip visibility, viewport bounds, and image loading. The control socket is local to the user, lives under the app's data directory, and is removed when the presenter closes. GUI-launched presentations also appear in `present list`.

`deck export` includes presenter metadata and speaker notes. Use `deck export-audience` when distributing HTML to an audience; it omits speaker notes. Exports are HyperFrames compositions. They can be opened by the HyperFrames CLI, which is optional for the native application. The compatibility alias `hyperframe-slides --export DECK_ID DIRECTORY` remains available.

The embedded runtime is pinned to HyperFrames `0.8.78` and GSAP `3.15.0`; see [vendor assets](assets/vendor/README.md). Exported standalone HTML still references GSAP from a CDN, so the standalone export is not guaranteed to run offline. The native presenter uses the bundled copy.

## Release checks

```bash
cargo fmt --check
cargo test --quiet
cargo clippy --quiet -- -D warnings
cargo build --release --locked
bash -n install.sh uninstall.sh tests/live-presenter.sh
./tests/live-presenter.sh ~/.local/bin/hyperframe-slides-launch
git diff --check
```

The app code is [MIT licensed](LICENSE). Bundled runtime licenses are listed with the [vendor assets](assets/vendor/README.md).
