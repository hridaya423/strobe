#!/bin/sh
cd "$(dirname "$0")"
C=${CLIPS:-/tmp/vsp-clips}
case "$1" in
cfr1080|cfr2160)
  if [ "$1" = cfr1080 ]; then P=1080; else P=2160; fi
  ./run.sh $P-fetch-nv12-videoframe fetch $C/cfr$P.mp4 --format nv12
  ./run.sh $P-fetch-bgra-videoframe fetch $C/cfr$P.mp4 --format bgra
  ./run.sh $P-fetch-rgba-imagedata fetch $C/cfr$P.mp4 --format rgba --draw imagedata
  ./run.sh $P-fetch-bgra-bitmap fetch $C/cfr$P.mp4 --format bgra --draw bitmap
  ./run.sh $P-fetch-nv12-webgl fetch $C/cfr$P.mp4 --format nv12 --draw webgl
  ./run.sh $P-shared-nv12-videoframe shared $C/cfr$P.mp4 --format nv12
  ./run.sh $P-shared-bgra-videoframe shared $C/cfr$P.mp4 --format bgra
  ./run.sh $P-shared-nv12-webgl shared $C/cfr$P.mp4 --format nv12 --draw webgl
  ./run.sh $P-image-bmp image-bmp $C/cfr$P.mp4
  ./run.sh $P-image-jpeg image-jpeg $C/cfr$P.mp4
  ./run.sh $P-composite composite $C/cfr$P.mp4
  ./run.sh $P-video-vp9 video $C/cfr$P-vp9.webm --epsilon 0.0005
  ;;
timing)
  for clip in vfr1080 off1080; do
    ./run.sh $clip-fetch-nv12-videoframe fetch $C/$clip.mp4 --format nv12
    ./run.sh $clip-shared-nv12-webgl shared $C/$clip.mp4 --format nv12 --draw webgl
    ./run.sh $clip-shared-bgra-videoframe shared $C/$clip.mp4 --format bgra
    ./run.sh $clip-image-bmp image-bmp $C/$clip.mp4
    ./run.sh $clip-image-jpeg image-jpeg $C/$clip.mp4
    ./run.sh $clip-composite composite $C/$clip.mp4
    ./run.sh $clip-video-vp9 video $C/$clip-vp9.webm --epsilon 0.0005
  done
  ;;
colour)
  ./run.sh 601-shared-nv12-webgl shared $C/cfr1080-601.mp4 --format nv12 --draw webgl --matrix bt601
  ./run.sh 601-fetch-nv12-videoframe fetch $C/cfr1080-601.mp4 --format nv12 --matrix bt601 --cs srgb601
  ./run.sh 601-fetch-bgra-videoframe fetch $C/cfr1080-601.mp4 --format bgra --matrix bt601
  ./run.sh 601-composite composite $C/cfr1080-601.mp4 --matrix bt601
  ./run.sh full-shared-nv12-webgl shared $C/cfr1080-full.mp4 --format nv12 --draw webgl --matrix bt601 --range full
  ./run.sh full-fetch-nv12-videoframe fetch $C/cfr1080-full.mp4 --format nv12 --matrix bt601 --range full --cs srgbfull601
  ./run.sh full-composite composite $C/cfr1080-full.mp4 --matrix bt601 --range full
  ./run.sh hevc-shared-nv12-webgl shared $C/cfr1080-hevc.mp4 --format nv12 --draw webgl
  ./run.sh hevc-composite composite $C/cfr1080-hevc.mp4
  ;;
esac
