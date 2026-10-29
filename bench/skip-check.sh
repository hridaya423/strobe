#!/usr/bin/env bash
set -euo pipefail

vrf=${VRF:-target/release/vrf}
label=$1
shift
dir=${SKIP_CHECK_DIR:-$(mktemp -d)}

"$vrf" render "$@" --no-skip --json -o "$dir/$label-off.mp4" >"$dir/$label-off.json"
"$vrf" render "$@" --json -o "$dir/$label-on.mp4" >"$dir/$label-on.json"
for run in off on; do
  ffmpeg -v error -i "$dir/$label-$run.mp4" -map 0:v:0 -f framemd5 - | grep -v '^#' | awk -F', *' '{print $NF}' >"$dir/$label-$run.md5"
done

frames=$(wc -l <"$dir/$label-on.md5" | tr -d ' ')
skipped=$(jq .skipped "$dir/$label-on.json")
if cmp -s "$dir/$label-off.md5" "$dir/$label-on.md5"; then
  verdict=identical
else
  verdict=DIFFERENT
fi
echo "$label: $frames frames, $skipped skipped, framemd5 $verdict (off skipped $(jq .skipped "$dir/$label-off.json"))"
[ "$verdict" = identical ]
