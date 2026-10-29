#!/usr/bin/env bash
set -uo pipefail

vrf=${VRF:-target/release/vrf}
label=$1
shift
dir=${SKIP_CHECK_DIR:-$(mktemp -d)}

render() {
  "$vrf" render "$@" --json -o "$dir/$label-$run.mp4" >"$dir/$label-$run.json" || return 1
  ffmpeg -v error -i "$dir/$label-$run.mp4" -map 0:v:0 -f framemd5 - | grep -v '^#' | awk -F', *' '{print $NF}' >"$dir/$label-$run.md5"
}

run=off render "$@" --no-skip || { echo "$label: render failed"; exit 1; }
run=on render "$@" || { echo "$label: render failed with skipping on"; exit 1; }
frames=$(wc -l <"$dir/$label-on.md5" | tr -d ' ')
skipped=$(jq .skipped "$dir/$label-on.json")
if cmp -s "$dir/$label-off.md5" "$dir/$label-on.md5"; then
  verdict=identical
else
  run=off2 render "$@" --no-skip || { echo "$label: render failed"; exit 1; }
  if cmp -s "$dir/$label-off2.md5" "$dir/$label-on.md5"; then
    verdict="identical to the second run with skipping off (the two runs with skipping off differ in $(diff "$dir/$label-off.md5" "$dir/$label-off2.md5" | grep -c '^<' || true) frames)"
  elif cmp -s "$dir/$label-off.md5" "$dir/$label-off2.md5"; then
    verdict=DIFFERENT
  else
    verdict="not repeatable: three runs, no two alike"
  fi
fi
echo "$label: $frames frames, $skipped skipped, framemd5 $verdict"
case "$verdict" in DIFFERENT | "not repeatable"*) exit 1 ;; esac
