#!/bin/sh
set -eu
OUT="${1:?output directory}"
ONLY="${2:-}"
mkdir -p "$OUT"

# usage: clip <name> <width> <height> <frames> <pre-filter> <matrix> <range> <tags> <extra output args...>
# <tags> is "full" (bt709 matrix, primaries, transfer) or "matrix" (matrix only)
clip() {
  name=$1; w=$2; h=$3; frames=$4; pre=$5; matrix=$6; range=$7; tags=$8; shift 8
  if [ -n "$ONLY" ]; then case " $ONLY " in *" $name "*) ;; *) return 0 ;; esac; fi
  cell=$((w / 48))
  bits=""
  i=0
  while [ $i -lt 24 ]; do
    bits="$bits,drawbox=x=$((i * cell)):y=0:w=$cell:h=$cell:color=white:t=fill:enable='bitand(floor(n/pow(2,$i)),1)'"
    i=$((i + 1))
  done
  patches=""
  i=0
  for c in 0xFF0000 0x00FF00 0x0000FF 0x808080 0xFFFFFF 0x000000 0xE0AC96 0x1E90FF; do
    patches="$patches,drawbox=x=$((i * w / 8)):y=$((h / 3)):w=$((w / 8)):h=$((h / 6)):color=$c:t=fill"
    i=$((i + 1))
  done
  case "$matrix" in
    bt709) smatrix=bt709 ;;
    bt470bg) smatrix=bt601 ;;
  esac
  case "$range" in
    tv) srange=limited ;;
    pc) srange=full ;;
  esac
  if [ "$tags" = full ]; then
    params="setparams=range=$range:colorspace=$matrix:color_primaries=bt709:color_trc=bt709"
  else
    params="setparams=range=$range:colorspace=$matrix:color_primaries=unknown:color_trc=unknown"
  fi
  ffmpeg -v error -y -f lavfi -i "testsrc2=size=${w}x${h}:rate=30:duration=$((frames / 30 + 1))" \
    -frames:v "$frames" \
    -vf "format=rgb24,drawbox=x=0:y=0:w=$((24 * cell)):h=$cell:color=black:t=fill${bits}${patches},scale=out_color_matrix=$smatrix:out_range=$srange:flags=accurate_rnd+full_chroma_int,format=yuv420p,${params},${pre}" \
    -fps_mode passthrough \
    "$@" "$OUT/$name"
}

H264="-c:v libx264 -preset fast -crf 14 -g 30 -movflags +faststart"
VP9="-c:v libvpx-vp9 -b:v 0 -crf 24 -deadline realtime -cpu-used 8 -g 30"
HEVC="-c:v libx265 -preset fast -crf 16 -g 30 -tag:v hvc1 -x265-params log-level=error -movflags +faststart"
VFR="settb=1/1000,setpts='(183*floor(N/4)+if(eq(mod(N,4),0),0,if(eq(mod(N,4),1),33,if(eq(mod(N,4),2),66,166))))/1000/TB'"
OFF="setpts=PTS+1.5/TB"
CFR="setpts=PTS"

clip cfr1080.mp4 1920 1080 120 "$CFR" bt709 tv full $H264
clip vfr1080.mp4 1920 1080 120 "$VFR" bt709 tv full $H264 -enc_time_base 1/1000 -video_track_timescale 1000
clip off1080.mp4 1920 1080 120 "$OFF" bt709 tv full $H264
clip cfr2160.mp4 3840 2160 90 "$CFR" bt709 tv full $H264
clip cfr1080-601.mp4 1920 1080 30 "$CFR" bt470bg tv matrix $H264
clip cfr1080-full.mp4 1920 1080 30 "$CFR" bt470bg pc matrix $H264
clip cfr1080-hevc.mp4 1920 1080 60 "$CFR" bt709 tv full $HEVC
clip cfr1080-vp9.webm 1920 1080 120 "$CFR" bt709 tv full $VP9
clip vfr1080-vp9.webm 1920 1080 120 "$VFR" bt709 tv full $VP9 -enc_time_base 1/1000
clip off1080-vp9.webm 1920 1080 120 "$OFF" bt709 tv full $VP9
clip cfr2160-vp9.webm 3840 2160 90 "$CFR" bt709 tv full $VP9
ls -l "$OUT"
