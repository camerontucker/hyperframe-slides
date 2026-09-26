#!/usr/bin/env bash
set -euo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cargo build --release --manifest-path "$project_dir/Cargo.toml"

plugin_dir="$HOME/.local/share/hyperframe-slides/gstreamer"
if ! gst-inspect-1.0 autoaudiosink >/dev/null 2>&1; then
  if [[ ! -f "$plugin_dir/libgstautodetect.so" ]] || ! GST_PLUGIN_PATH="$plugin_dir" gst-inspect-1.0 autoaudiosink >/dev/null 2>&1; then
    if [[ $(gst-inspect-1.0 --version | head -1) != *"1.28.7"* ]]; then
      echo "Install gst-plugins-good with: omarchy pkg add gst-plugins-good" >&2
      exit 1
    fi
    package=$(mktemp -t hyperframe-slides-gst-XXXXXX)
    trap 'rm -f "$package"' EXIT
    curl --fail --location --silent --show-error \
      'https://mirror.omarchy.org/extra/os/x86_64/gst-plugins-good-1.28.7-2-x86_64.pkg.tar.zst' \
      --output "$package"
    printf '%s  %s\n' '3b38b6c0576abcb34ecd7aec44eabd857fcf7ec9b0511dcd88aa0e0c6b9c2dc1' "$package" | sha256sum --check --status
    mkdir -p "$plugin_dir"
    bsdtar -xOf "$package" usr/lib/gstreamer-1.0/libgstautodetect.so > "$plugin_dir/libgstautodetect.so"
    GST_PLUGIN_PATH="$plugin_dir" gst-inspect-1.0 autoaudiosink >/dev/null
    echo "Installed the missing GStreamer plugin for this user."
  fi
fi

install -Dm755 "$project_dir/target/release/hyperframe-slides" "$HOME/.local/bin/hyperframe-slides"
install -Dm644 "$project_dir/assets/hyperframe-slides.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/hyperframe-slides.svg"

launcher="$HOME/.local/bin/hyperframe-slides-launch"
cat > "$launcher" <<'SCRIPT'
#!/bin/sh
export PATH="$HOME/.local/share/mise/shims:$PATH"
if ! gst-inspect-1.0 autoaudiosink >/dev/null 2>&1; then
  export GST_PLUGIN_PATH="$HOME/.local/share/hyperframe-slides/gstreamer${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}"
fi
exec "$HOME/.local/bin/hyperframe-slides" "$@"
SCRIPT
chmod 755 "$launcher"

desktop="$HOME/.local/share/applications/hyperframe-slides.desktop"
cat > "$desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=HyperFrames Slides
GenericName=Presentation Editor
Comment=Create and present HyperFrames decks in native windows
Exec=$launcher
Icon=hyperframe-slides
Terminal=false
Categories=Office;Presentation;
Keywords=slides;presentations;zoom;hyperframes;
StartupWMClass=hyperframe-slides
DESKTOP
echo "Installed HyperFrames Slides in the Omarchy app launcher."
