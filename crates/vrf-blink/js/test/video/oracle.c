#include <libswscale/swscale.h>
#include <libavutil/pixfmt.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {
  const char *in = argv[1], *out = argv[2];
  int w = atoi(argv[3]), h = atoi(argv[4]);
  int flags = atoi(argv[5]);
  int cs = atoi(argv[6]);
  int srcrange = atoi(argv[7]);
  int dstrange = atoi(argv[8]);
  size_t ysz = (size_t)w*h, csz = (size_t)(w/2)*(h/2);
  uint8_t *buf = malloc(ysz + 2*csz);
  FILE *f = fopen(in, "rb"); fread(buf, 1, ysz+2*csz, f); fclose(f);
  struct SwsContext *ctx = sws_getContext(w,h,AV_PIX_FMT_YUV420P,w,h,AV_PIX_FMT_BGR24,flags,NULL,NULL,NULL);
  if (cs >= 0) {
    const int *inv = sws_getCoefficients(cs);
    const int *tab = sws_getCoefficients(cs);
    sws_setColorspaceDetails(ctx, inv, srcrange, tab, dstrange, 0, 1<<16, 1<<16);
  }
  uint8_t *dst = malloc((size_t)w*h*3);
  const uint8_t *src[4] = {buf, buf+ysz, buf+ysz+csz, NULL};
  int sstride[4] = {w, w/2, w/2, 0};
  uint8_t *dp[4] = {dst, NULL, NULL, NULL};
  int dstride[4] = {w*3, 0,0,0};
  sws_scale(ctx, src, sstride, 0, h, dp, dstride);
  f = fopen(out, "wb"); fwrite(dst, 1, (size_t)w*h*3, f); fclose(f);
  return 0;
}
