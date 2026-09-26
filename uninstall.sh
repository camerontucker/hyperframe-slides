#!/usr/bin/env bash
set -euo pipefail

data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
plugin_dir="$data_home/hyperframe-slides/gstreamer"

rm -f -- \
  "$HOME/.local/bin/hyperframe-slides" \
  "$HOME/.local/bin/hyperframe-slides-launch" \
  "$HOME/.local/share/applications/hyperframe-slides.desktop" \
  "$HOME/.local/share/icons/hicolor/scalable/apps/hyperframe-slides.svg" \
  "$plugin_dir/libgstautodetect.so"
rmdir -- "$plugin_dir" 2>/dev/null || true

echo "Removed HyperFrames Slides and its launcher. Your decks remain in $data_home/hyperframe-slides/decks."
