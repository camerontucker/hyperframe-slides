#!/usr/bin/env bash
set -euo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
for command in cargo pkg-config gst-inspect-1.0; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "Missing $command. Install Rust, GTK 3, WebKitGTK 4.1, and GStreamer before running this installer." >&2
    exit 1
  fi
done
if ! pkg-config --exists gtk+-3.0 webkit2gtk-4.1; then
  echo "Missing GTK 3 or WebKitGTK 4.1 development files. Install them with: omarchy pkg add gtk3 webkit2gtk-4.1" >&2
  exit 1
fi

data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
plugin_dir="$data_home/hyperframe-slides/gstreamer"
if ! gst-inspect-1.0 autoaudiosink >/dev/null 2>&1; then
  if [[ ! -f "$plugin_dir/libgstautodetect.so" ]] || ! GST_PLUGIN_PATH="$plugin_dir" gst-inspect-1.0 autoaudiosink >/dev/null 2>&1; then
    if [[ $(gst-inspect-1.0 --version | head -1) != *"1.28.7"* ]]; then
      echo "Install gst-plugins-good with: omarchy pkg add gst-plugins-good" >&2
      exit 1
    fi
    for command in curl sha256sum bsdtar; do
      if ! command -v "$command" >/dev/null 2>&1; then
        echo "Missing $command. Install gst-plugins-good with: omarchy pkg add gst-plugins-good" >&2
        exit 1
      fi
    done
    package=$(mktemp -t hyperframe-slides-gst-XXXXXX)
    mkdir -p "$plugin_dir"
    plugin_tmp="$plugin_dir/.libgstautodetect.so.tmp-$$"
    trap 'rm -f -- "$package" "$plugin_tmp"' EXIT
    curl --fail --location --silent --show-error \
      'https://mirror.omarchy.org/extra/os/x86_64/gst-plugins-good-1.28.7-2-x86_64.pkg.tar.zst' \
      --output "$package"
    printf '%s  %s\n' '3b38b6c0576abcb34ecd7aec44eabd857fcf7ec9b0511dcd88aa0e0c6b9c2dc1' "$package" | sha256sum --check --status
    bsdtar -xOf "$package" usr/lib/gstreamer-1.0/libgstautodetect.so > "$plugin_tmp"
    mv -- "$plugin_tmp" "$plugin_dir/libgstautodetect.so"
    GST_PLUGIN_PATH="$plugin_dir" gst-inspect-1.0 autoaudiosink >/dev/null
    echo "Installed the missing GStreamer plugin for this user."
  fi
fi

cargo build --release --locked --manifest-path "$project_dir/Cargo.toml"

install -Dm755 "$project_dir/target/release/hyperframe-slides" "$HOME/.local/bin/hyperframe-slides"
install -Dm644 "$project_dir/assets/hyperframe-slides.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/hyperframe-slides.svg"

launcher="$HOME/.local/bin/hyperframe-slides-launch"
cat > "$launcher" <<'SCRIPT'
#!/bin/sh
export PATH="$HOME/.local/share/mise/shims:$PATH"
if ! gst-inspect-1.0 autoaudiosink >/dev/null 2>&1; then
  export GST_PLUGIN_PATH="${XDG_DATA_HOME:-$HOME/.local/share}/hyperframe-slides/gstreamer${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}"
fi
exec "$HOME/.local/bin/hyperframe-slides" "$@"
SCRIPT
chmod 755 "$launcher"

desktop="$HOME/.local/share/applications/hyperframe-slides.desktop"
mkdir -p "$(dirname "$desktop")"
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
