#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/lib.sh"
id=$1
hex=$2
src=$root/bench/refs/$id/frames
dst=$out/refs-composited/$id/frames
mkdir -p "$dst"
size=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=s=x:p=0 "$src/$(ls "$src" | head -1)")
for f in "$src"/*.png; do
  ffmpeg -v error -y -f lavfi -i "color=c=0x$hex:s=$size,format=rgb24" -i "$f" \
    -filter_complex "[1:v]format=rgba[o];[0:v][o]overlay=format=rgb,format=rgb24" -frames:v 1 "$dst/$(basename "$f")"
done
