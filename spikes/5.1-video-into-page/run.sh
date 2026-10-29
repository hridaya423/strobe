#!/bin/sh
set -u
cd "$(dirname "$0")"
LOGS=../../docs/spikes/raw/5.1
CLIPS=${CLIPS:-/tmp/vsp-clips}
tag=$1; shift
echo "== $tag $*"
./target/release/video-spike run "$@" --tag "$tag" > "$LOGS/$tag.txt" 2>&1 || echo "FAILED $tag"
grep -E "^frames|^  (seek|paint|post|total|prep|child)" "$LOGS/$tag.txt" | tr '\n' '|'; echo
