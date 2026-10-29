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
  "$vrf" render "$@" --no-skip --json -o "$dir/$label-off2.mp4" >"$dir/$label-off2.json"
  ffmpeg -v error -i "$dir/$label-off2.mp4" -map 0:v:0 -f framemd5 - | grep -v '^#' | awk -F', *' '{print $NF}' >"$dir/$label-off2.md5"
  if cmp -s "$dir/$label-off.md5" "$dir/$label-off2.md5"; then
    verdict=DIFFERENT
  else
    verdict="not repeatable ($(diff "$dir/$label-off.md5" "$dir/$label-off2.md5" | grep -c '^<') frames differ between two runs with skipping off)"
  fi
fi
echo "$label: $frames frames, $skipped skipped, framemd5 $verdict (off skipped $(jq .skipped "$dir/$label-off.json"))"
[ "$verdict" = identical ] || [ "${verdict#not repeatable}" != "$verdict" ]
