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
"$bin" deck validate "$deck_id" >/dev/null
"$bin" deck present "$deck_id" --audience > "$work/presenter.stdout" 2> "$work/presenter.stderr" &
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
  echo "Presenter did not start." >&2
  exit 1
fi

for slide_number in 1 2; do
  "$bin" present goto "$session" "$slide_number" >/dev/null
  passed=false
  for _ in {1..50}; do
    status=$("$bin" present status "$session")
    scene=$("$bin" present inspect-audience "$session")
    if jq -e --argjson number "$slide_number" '.audienceSlideNumber == $number' <<< "$status" >/dev/null \
      && jq -e '.frameReadable and ([.scenes[] | select(.visibility == "visible" and .inViewport and ([.clips[] | select(.visibility == "visible")] | length) > 0 and .header == "Visible on every slide")] | length) == 1' <<< "$scene" >/dev/null; then
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
test ! -s "$work/presenter.stderr"
echo "Live presenter and audience passed on both slides."
