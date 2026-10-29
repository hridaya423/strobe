#import <AVFoundation/AVFoundation.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>

typedef void (*vsp_frame_cb)(void *ctx, CVPixelBufferRef buffer, int64_t value, int32_t timescale);

int vsp_decode(const char *path, uint32_t pixel_format, vsp_frame_cb callback, void *ctx) {
    @autoreleasepool {
        NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:path]];
        AVURLAsset *asset = [AVURLAsset URLAssetWithURL:url options:nil];
        AVAssetTrack *track = [[asset tracksWithMediaType:AVMediaTypeVideo] firstObject];
        if (!track) return -1;
        NSError *error = nil;
        AVAssetReader *reader = [AVAssetReader assetReaderWithAsset:asset error:&error];
        if (!reader) return -2;
        NSDictionary *settings = @{
            (id)kCVPixelBufferPixelFormatTypeKey: @(pixel_format),
            (id)kCVPixelBufferIOSurfacePropertiesKey: @{},
        };
        AVAssetReaderTrackOutput *output = [AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:track outputSettings:settings];
        output.alwaysCopiesSampleData = NO;
        [reader addOutput:output];
        if (![reader startReading]) return -3;
        for (;;) {
            @autoreleasepool {
                CMSampleBufferRef sample = [output copyNextSampleBuffer];
                if (!sample) break;
                CVPixelBufferRef buffer = CMSampleBufferGetImageBuffer(sample);
                CMTime pts = CMSampleBufferGetPresentationTimeStamp(sample);
                if (buffer) callback(ctx, buffer, pts.value, pts.timescale);
                CFRelease(sample);
            }
        }
        return reader.status == AVAssetReaderStatusCompleted ? 0 : -4;
    }
}

IOSurfaceRef vsp_surface(CVPixelBufferRef buffer) {
    return CVPixelBufferGetIOSurface(buffer);
}

#import <ImageIO/ImageIO.h>

size_t vsp_jpeg(const uint8_t *bgra, size_t width, size_t height, size_t stride, double quality, uint8_t *out, size_t capacity) {
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGDataProviderRef provider = CGDataProviderCreateWithData(NULL, bgra, stride * height, NULL);
    CGImageRef image = CGImageCreate(width, height, 8, 32, stride, space,
        kCGBitmapByteOrder32Little | kCGImageAlphaNoneSkipFirst, provider, NULL, false, kCGRenderingIntentDefault);
    CFMutableDataRef data = CFDataCreateMutable(NULL, 0);
    CGImageDestinationRef destination = CGImageDestinationCreateWithData(data, CFSTR("public.jpeg"), 1, NULL);
    NSDictionary *options = @{(id)kCGImageDestinationLossyCompressionQuality: @(quality)};
    CGImageDestinationAddImage(destination, image, (__bridge CFDictionaryRef)options);
    CGImageDestinationFinalize(destination);
    size_t length = (size_t)CFDataGetLength(data);
    if (length <= capacity) memcpy(out, CFDataGetBytePtr(data), length);
    CFRelease(destination);
    CFRelease(data);
    CGImageRelease(image);
    CGDataProviderRelease(provider);
    CGColorSpaceRelease(space);
    return length;
}
