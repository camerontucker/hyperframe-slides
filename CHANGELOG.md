# Changelog

## v0.0.1-alpha.3 — 2026-10-02

- GTK4 editor with presentation actions in one menu and a direct, floating audience window.
- File-backed media, editable presentation bundles, revision-safe updates, slide overview, visual diffs, and slide-level recovery.
- Grouped outlines with clickable section navigation, quiz reveals, and smoother text and picture entrances.
- Optional bullet fades on click, Space, Right arrow, or CLI Next.
- Optional built-in modem sound on slide entry, with editor/CLI settings and live mute control.
- `deck export-pdf ID FILE [--exclude SLIDE_ID]...` produces one multipage landscape PDF with selectable text, embedded fonts, optimized pictures, and no notes or audio. Exclusions leave the original deck unchanged.
- PDF pagination preserves uniform bullet/sidebar spacing, and supporting text fits above the footer.

Source installation requires Rust/Cargo, GTK4, WebKitGTK 6.0, and GStreamer. Native rendering and PDF export require a desktop session.

## v0.0.1-alpha.1 — 2026-09-26

First public alpha release.

- Native Rust/GTK editor with a WebKitGTK HyperFrames preview and separate presenter and audience windows for Zoom.
- Markdown bullets, bold and emphasis; embedded pictures with drag placement; slide layouts, themes, animations, speaker notes, and a shared header, footer, and logo.
- JSON CLI for deck creation, revision-safe edits, validation, export, and live presenter control and inspection.
- Local, offline native presentation using bundled HyperFrames assets. The standalone HTML export still references GSAP from a CDN.
- Source installer and uninstaller for Omarchy, with GTK, WebKitGTK, GStreamer, and Rust/Cargo prerequisites.
