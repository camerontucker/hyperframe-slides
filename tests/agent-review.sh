#!/usr/bin/env bash
set -euo pipefail

bin=${1:-./target/debug/hyperframe-slides}
if [[ -z ${WAYLAND_DISPLAY:-} && -z ${DISPLAY:-} ]]; then
  echo "A graphical session is required for WebKitGTK review." >&2
  exit 1
fi

work=$(mktemp -d /tmp/hyperframe-slides-review-XXXXXX)
trap 'rm -rf -- "$work"' EXIT
export HYPERFRAME_SLIDES_DATA_DIR="$work/data"

"$bin" schema json | jq -e '.["$defs"].slide and .properties.slides' >/dev/null
"$bin" schema source | jq -e '.["$id"] | endswith("/schema/storage.schema.json")' >/dev/null
deck=$("$bin" deck put examples/agent-demo-deck.json)
deck_id=$(jq -r .id <<< "$deck")
"$bin" deck validate "$deck_id" >/dev/null
"$bin" deck review "$deck_id" "$work/clean" > "$work/clean.json"
jq -e '.ok and .issueCount == 0 and .slideCount == 3 and (.slides | length) == 3' "$work/clean.json" >/dev/null
revision=$("$bin" deck revision "$deck_id" | jq -r .revision)
jq -e --arg revision "$revision" '.reportVersion == 2 and .deckRevision == $revision and .contactSheet == "contact-sheet.png" and (.slides[0].image | startswith("slide-01-")) and (.slides[0].contentHash | length == 64) and (has("directory") | not)' "$work/clean/report.json" >/dev/null
"$bin" deck render "$deck_id" draft "$work/single" > "$work/single.json"
jq -e --arg revision "$revision" '.deckRevision == $revision and .slideCount == 1 and .slides[0].slideId == "draft"' "$work/single/report.json" >/dev/null
python3 - "$work/clean/slide-01-draft.png" "$work/clean/contact-sheet.png" <<'PY'
import struct
import sys

def dimensions(path):
    with open(path, 'rb') as image:
        header = image.read(24)
    assert header[:8] == b'\x89PNG\r\n\x1a\n', path
    return struct.unpack('>II', header[16:24])

assert dimensions(sys.argv[1]) == (960, 540)
assert dimensions(sys.argv[2])[0] >= 960
PY

snapshot=$("$bin" deck snapshot "$deck_id")
revision=$(jq -r .revision <<< "$snapshot")
jq '.deck | .slides[0].title = ("A very long headline that exceeds the slide boundary. " * 30)' <<< "$snapshot" > "$work/crowded.json"
"$bin" deck put "$work/crowded.json" --if-revision "$revision" >/dev/null
"$bin" deck review "$deck_id" "$work/crowded" > "$work/crowded-report.json"
jq -e '.ok == false and .issueCount > 0 and any(.slides[0].issues[]; .code == "clipped")' "$work/crowded-report.json" >/dev/null

echo "Agent review passed: images and report generated; crowded text was flagged."
