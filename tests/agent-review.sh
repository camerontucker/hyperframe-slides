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

current_revision=$("$bin" deck revision "$deck_id" | jq -r .revision)
"$bin" deck diff "$deck_id" "$revision" | jq -e --arg current "$current_revision" '.currentRevision == $current and (.changes | length) == 1 and (.changes[0].beforeImage? | not)' >/dev/null
"$bin" deck diff "$deck_id" "$revision" "$work/diff" > "$work/diff-output.json"
jq -e --arg old "$revision" --arg current "$current_revision" '.beforeRevision == $old and .currentRevision == $current and (.changes | length) == 1 and .changes[0].id == "draft" and .changes[0].kind == "changed" and .changes[0].summary == ["Headline changed"] and (.changes[0].beforeIssues | length) == 0 and any(.changes[0].afterIssues[]; .code == "clipped") and (has("directory") | not)' "$work/diff/diff.json" >/dev/null
before_image=$(jq -r '.changes[0].beforeImage' "$work/diff/diff.json")
after_image=$(jq -r '.changes[0].afterImage' "$work/diff/diff.json")
test -f "$work/diff/$before_image"
test -f "$work/diff/$after_image"
"$bin" deck revert-slide "$deck_id" "$revision" draft --if-revision "$current_revision" >/dev/null
"$bin" deck get "$deck_id" | jq -e '.slides[0].title == "An agent can turn a brief into a local deck."' >/dev/null
if "$bin" deck revert-slide "$deck_id" "$revision" draft --if-revision "$current_revision" >/dev/null 2>&1; then
  echo "A stale slide revert unexpectedly succeeded" >&2
  exit 1
fi

echo "Agent review passed: images, findings, visual diff, and revision-safe slide revert."
