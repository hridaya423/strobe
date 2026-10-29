#!/bin/sh
cd "$(dirname "$0")"
C=${CLIPS:-/tmp/vsp-clips}
case "$1" in
cfr1080)
  ./run.sh 1080-fetch-nv12 fetch $C/cfr1080.mp4 --format nv12
  ./run.sh 1080-fetch-bgra fetch $C/cfr1080.mp4 --format bgra
  ./run.sh 1080-fetch-rgba-imagedata fetch $C/cfr1080.mp4 --format rgba --draw imagedata
  ./run.sh 1080-fetch-bgra-bitmap fetch $C/cfr1080.mp4 --format bgra --draw bitmap
  ./run.sh 1080-shared-nv12 shared $C/cfr1080.mp4 --format nv12
  ./run.sh 1080-shared-bgra shared $C/cfr1080.mp4 --format bgra
  ./run.sh 1080-image-bmp image-bmp $C/cfr1080.mp4
  ./run.sh 1080-image-jpeg image-jpeg $C/cfr1080.mp4
  ./run.sh 1080-composite composite $C/cfr1080.mp4
  ./run.sh 1080-video-vp9 video $C/cfr1080-vp9.webm --epsilon 0.0005
  ;;
cfr2160)
  ./run.sh 2160-fetch-nv12 fetch $C/cfr2160.mp4 --format nv12
  ./run.sh 2160-fetch-bgra fetch $C/cfr2160.mp4 --format bgra
  ./run.sh 2160-fetch-rgba-imagedata fetch $C/cfr2160.mp4 --format rgba --draw imagedata
  ./run.sh 2160-fetch-bgra-bitmap fetch $C/cfr2160.mp4 --format bgra --draw bitmap
  ./run.sh 2160-shared-nv12 shared $C/cfr2160.mp4 --format nv12
  ./run.sh 2160-shared-bgra shared $C/cfr2160.mp4 --format bgra
  ./run.sh 2160-image-bmp image-bmp $C/cfr2160.mp4
  ./run.sh 2160-image-jpeg image-jpeg $C/cfr2160.mp4
  ./run.sh 2160-composite composite $C/cfr2160.mp4
  ./run.sh 2160-video-vp9 video $C/cfr2160-vp9.webm --epsilon 0.0005
  ;;
timing)
  for clip in vfr1080 off1080; do
    ./run.sh $clip-fetch-nv12 fetch $C/$clip.mp4 --format nv12
    ./run.sh $clip-fetch-bgra fetch $C/$clip.mp4 --format bgra
    ./run.sh $clip-shared-bgra shared $C/$clip.mp4 --format bgra
    ./run.sh $clip-image-bmp image-bmp $C/$clip.mp4
    ./run.sh $clip-image-jpeg image-jpeg $C/$clip.mp4
    ./run.sh $clip-composite composite $C/$clip.mp4
    ./run.sh $clip-video-vp9 video $C/$clip-vp9.webm --epsilon 0.0005
  done
  ;;
colour)
  ./run.sh 601-fetch-bgra fetch $C/cfr1080-601.mp4 --format bgra --matrix bt601
  ./run.sh 601-fetch-nv12-srgb601 fetch $C/cfr1080-601.mp4 --format nv12 --matrix bt601 --cs srgb601
  ./run.sh 601-fetch-nv12-bt709 fetch $C/cfr1080-601.mp4 --format nv12 --matrix bt601 --cs bt601
  ./run.sh 601-composite composite $C/cfr1080-601.mp4 --matrix bt601
  ./run.sh full-fetch-bgra fetch $C/cfr1080-full.mp4 --format bgra --matrix bt601 --range full
  ./run.sh full-fetch-nv12 fetch $C/cfr1080-full.mp4 --format nv12 --matrix bt601 --range full --cs srgbfull601
  ./run.sh hevc-fetch-bgra fetch $C/cfr1080-hevc.mp4 --format bgra
  ./run.sh hevc-composite composite $C/cfr1080-hevc.mp4
  ;;
esac
