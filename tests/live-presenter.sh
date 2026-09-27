#!/usr/bin/env bash
set -euo pipefail

bin=${1:-./target/debug/hyperframe-slides}
if [[ ! -x $bin ]]; then
  echo "Build the app first or pass the installed binary path." >&2
  exit 1
fi
if [[ -z ${WAYLAND_DISPLAY:-} && -z ${DISPLAY:-} ]]; then
  echo "This check needs a graphical desktop session." >&2
  exit 1
fi

work=$(mktemp -d /tmp/hyperframe-slides-live-XXXXXX)
export HYPERFRAME_SLIDES_DATA_DIR="$work/data"
session=
presenter_pid=
cleanup() {
  result=$?
  if (( result != 0 )) && [[ -s $work/presenter.stderr ]]; then
    cat "$work/presenter.stderr" >&2
  fi
  if [[ -n $session ]]; then
    "$bin" present close "$session" >/dev/null 2>&1 || true
  fi
  if [[ -n $presenter_pid ]]; then
    wait "$presenter_pid" 2>/dev/null || true
  fi
  rm -rf -- "$work"
}
trap cleanup EXIT

deck_json=$("$bin" deck new 'Live presentation check')
deck_id=$(jq -r .id <<< "$deck_json")
"$bin" slide add "$deck_id" >/dev/null
"$bin" template header "$deck_id" 'Visible on every slide' >/dev/null
snapshot=$("$bin" deck snapshot "$deck_id")
revision=$(jq -r .revision <<< "$snapshot")
first_slide_id=$(jq -r '.deck.slides[0].id' <<< "$snapshot")
second_slide_id=$(jq -r '.deck.slides[1].id' <<< "$snapshot")
jq '.deck | .slides[0].title = "First slide headline" | .slides[0].body = "First slide supporting text" | .slides[1].layout = "split" | .slides[1].title = "Second slide headline" | .slides[1].body = "Second slide supporting text"' <<< "$snapshot" > "$work/deck.json"
"$bin" deck put "$work/deck.json" --if-revision "$revision" >/dev/null
python3 - "$work/picture.png" <<'PY'
import struct
import sys
import zlib

def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

png = b'\x89PNG\r\n\x1a\n'
png += chunk(b'IHDR', struct.pack('>2I5B', 1, 1, 8, 2, 0, 0, 0))
png += chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00'))
png += chunk(b'IEND', b'')
open(sys.argv[1], 'wb').write(png)
PY
"$bin" image add "$deck_id" "$first_slide_id" "$work/picture.png" 'Red pixel' >/dev/null
"$bin" image add "$deck_id" "$second_slide_id" "$work/picture.png" 'Red pixel' >/dev/null
"$bin" deck validate "$deck_id" >/dev/null
"$bin" deck present "$deck_id" > "$work/presenter.stdout" 2> "$work/presenter.stderr" &
presenter_pid=$!

started=false
for _ in {1..100}; do
  status=$("$bin" present list)
  session=$(jq -r '.[0].session // empty' <<< "$status")
  if [[ -n $session ]] && jq -e '.[0].ready and .[0].audienceOpen' <<< "$status" >/dev/null; then
    started=true
    break
  fi
  sleep 0.1
done
if [[ $started != true ]]; then
  cat "$work/presenter.stderr" >&2
  echo "Audience did not start." >&2
  exit 1
fi

status=$("$bin" present status "$session")
scene=$("$bin" present inspect "$session")
if ! jq -e '.notesEnabled == false' <<< "$status" >/dev/null \
  || ! jq -e '.notesEnabled == false and .notesPaneVisible == false' <<< "$scene" >/dev/null; then
  echo 'Audience speaker notes must stay hidden.' >&2
  exit 1
fi
if "$bin" present notes "$session" on >/dev/null 2>&1; then
  echo 'Audience unexpectedly allowed speaker notes.' >&2
  exit 1
fi
if [[ -n ${HYPRLAND_INSTANCE_SIGNATURE:-} ]]; then
  windows=$(hyprctl clients -j | jq --argjson pid "$presenter_pid" '[.[] | select(.pid == $pid and (.title | startswith("HyperFrames ")))]')
  if ! jq -e 'length == 1 and (.[0].title | startswith("HyperFrames Audience")) and .[0].floating' <<< "$windows" >/dev/null; then
    echo 'Presentation should open exactly one floating Audience window.' >&2
    exit 1
  fi
fi

# Idle local clients must not hold the GTK event loop while the agent asks for status.
python3 - "$HYPERFRAME_SLIDES_DATA_DIR/control/$session.sock" "$work/slow-ready" <<'PY' &
import socket
import sys
import time

clients = []
for _ in range(16):
    client = socket.socket(socket.AF_UNIX)
    client.connect(sys.argv[1])
    clients.append(client)
open(sys.argv[2], "w").close()
time.sleep(1)
for client in clients:
    client.close()
PY
slow_pid=$!
for _ in {1..100}; do
  [[ -e $work/slow-ready ]] && break
  sleep 0.01
done
start_ms=$(date +%s%3N)
"$bin" present status "$session" >/dev/null
elapsed_ms=$(( $(date +%s%3N) - start_ms ))
wait "$slow_pid"
if (( elapsed_ms >= 600 )); then
  echo "Idle control clients delayed presentation status by ${elapsed_ms}ms." >&2
  exit 1
fi

for slide_number in 1 2; do
  if [[ $slide_number == 1 ]]; then
    expected_title='First slide headline'
    expected_body='First slide supporting text'
  else
    expected_title='Second slide headline'
    expected_body='Second slide supporting text'
  fi
  "$bin" present goto "$session" "$slide_number" >/dev/null
  passed=false
  for _ in {1..50}; do
    status=$("$bin" present status "$session")
    scene=$("$bin" present inspect-audience "$session")
    if jq -e --argjson number "$slide_number" '.audienceSlideNumber == $number' <<< "$status" >/dev/null \
      && jq -e --arg title "$expected_title" --arg body "$expected_body" '.frameReadable and ([.scenes[] | select(.visibility == "visible" and .inViewport and .header == "Visible on every slide" and .title == $title and (.body | gsub("[[:space:]]+$"; "")) == $body and .headlineVisible and .bodyVisible and (.images | length) == 1 and all(.images[]; .loaded) and (.overlaps | length) == 0)] | length) == 1' <<< "$scene" >/dev/null; then
      passed=true
      break
    fi
    sleep 0.1
  done
  if [[ $passed != true ]]; then
    echo "Audience slide $slide_number was not visible in the viewport." >&2
    printf '%s\n' "$scene" >&2
    exit 1
  fi
done

"$bin" present close "$session" >/dev/null
session=
wait "$presenter_pid"
presenter_pid=
if [[ -s $work/presenter.stderr ]]; then
  echo "Audience emitted diagnostics (content checks passed):" >&2
  cat "$work/presenter.stderr" >&2
fi
echo "Live audience passed on both slides."
