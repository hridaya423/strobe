#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/assets"

for i in 1 2 3 4 5 6; do
  ffmpeg -v error -y -f lavfi \
    -i "gradients=s=2400x1600:c0=0x$(printf '%06x' $((i * 2654435 % 16777215))):c1=0x$(printf '%06x' $((i * 7919911 % 16777215))):c2=0x$(printf '%06x' $((i * 104729 % 16777215))):nb_colors=3:seed=$i:duration=1:speed=0.01:rate=1" \
    -frames:v 1 -vf "noise=alls=8:allf=t+u:all_seed=$i,drawgrid=w=150:h=150:t=2:c=white@0.08" \
    -q:v 5 "images/photo-$i.jpg"
done

ffmpeg -v error -y -f lavfi -i "testsrc2=s=1920x1080:r=30:d=5" \
  -c:v libx264 -preset slow -crf 26 -pix_fmt yuv420p -g 30 -movflags +faststart -an video/clip.mp4

cd ..
corpus=../corpus
mkdir -p $corpus/synth-text-hf/assets/fonts $corpus/synth-grid-hf/assets/fonts
cp assets/fonts/*.woff2 $corpus/synth-text-hf/assets/fonts/
cp assets/fonts/jetbrains-mono-latin-400-normal.woff2 $corpus/synth-grid-hf/assets/fonts/
mkdir -p $corpus/synth-images-hf/assets/images $corpus/synth-video-hf/assets/video
cp assets/images/*.jpg $corpus/synth-images-hf/assets/images/
cp assets/video/clip.mp4 $corpus/synth-video-hf/assets/video/
mkdir -p $corpus/synth-remotion/src/assets
cp assets/fonts/*.woff2 assets/images/*.jpg assets/video/clip.mp4 $corpus/synth-remotion/src/assets/
