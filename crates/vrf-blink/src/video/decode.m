#import <AVFoundation/AVFoundation.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>

typedef struct {
    int32_t width;
    int32_t height;
    int32_t timescale;
    uint32_t codec;
    uint32_t pixel_format;
    int32_t matrix;
    int32_t alpha;
    int32_t profile;
} VrfClipInfo;

@interface VrfClip : NSObject
@property(nonatomic, strong) AVURLAsset *asset;
@property(nonatomic, strong) AVAssetTrack *track;
@end
@implementation VrfClip
@end

@interface VrfReader : NSObject
@property(nonatomic, strong) AVAssetReader *reader;
@property(nonatomic, strong) AVAssetReaderTrackOutput *output;
@property(nonatomic) int32_t timescale;
@property(nonatomic, copy) NSString *failure;
@end
@implementation VrfReader
@end

static int64_t ticks(CMTime time, int32_t timescale) {
    return time.timescale == timescale ? time.value : (int64_t)llround((double)time.value * timescale / time.timescale);
}

static int compare(const void *a, const void *b) {
    int64_t x = *(const int64_t *)a, y = *(const int64_t *)b;
    return x < y ? -1 : x > y;
}

static void fail(char *error, size_t length, NSString *message) {
    snprintf(error, length, "%s", message.UTF8String);
}

static int32_t matrix_of(CVPixelBufferRef buffer) {
    CFTypeRef value = CVBufferCopyAttachment(buffer, kCVImageBufferYCbCrMatrixKey, NULL);
    if (!value) return 0;
    int32_t matrix = 3;
    if (CFEqual(value, kCVImageBufferYCbCrMatrix_ITU_R_709_2)) matrix = 1;
    else if (CFEqual(value, kCVImageBufferYCbCrMatrix_ITU_R_601_4)) matrix = 2;
    CFRelease(value);
    return matrix;
}

static void *clip_open(const char *path, VrfClipInfo *info, int64_t **table, size_t *count, int64_t **keys, size_t *key_count, char *error, size_t error_length) {
    @autoreleasepool {
        NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:path]];
        AVURLAsset *asset = [AVURLAsset URLAssetWithURL:url options:nil];
        AVAssetTrack *track = [[asset tracksWithMediaType:AVMediaTypeVideo] firstObject];
        if (!track) {
            fail(error, error_length, @"no video track");
            return NULL;
        }
        int32_t timescale = track.naturalTimeScale;
        NSMutableData *times = [NSMutableData data];
        NSMutableData *key_times = [NSMutableData data];
        AVSampleCursor *cursor = [track makeSampleCursorAtFirstSampleInDecodeOrder];
        if (!cursor) {
            fail(error, error_length, @"no samples");
            return NULL;
        }
        NSArray<AVAssetTrackSegment *> *segments = track.segments;
        do {
            CMTime presentation = cursor.presentationTimeStamp;
            int64_t media = ticks(presentation, timescale);
            for (AVAssetTrackSegment *segment in segments) {
                if (segment.isEmpty) continue;
                CMTimeRange source = segment.timeMapping.source, target = segment.timeMapping.target;
                int64_t source_start = ticks(source.start, timescale);
                int64_t source_length = ticks(source.duration, timescale);
                if (media >= source_start && media < source_start + source_length) {
                    int64_t movie = ticks(target.start, timescale) + (media - source_start);
                    [times appendBytes:&movie length:sizeof movie];
                    if (cursor.currentSampleSyncInfo.sampleIsFullSync) [key_times appendBytes:&movie length:sizeof movie];
                    break;
                }
            }
        } while ([cursor stepInDecodeOrderByCount:1] == 1);
        size_t n = times.length / sizeof(int64_t);
        if (n == 0) {
            fail(error, error_length, @"no displayed samples");
            return NULL;
        }
        qsort(times.mutableBytes, n, sizeof(int64_t), compare);
        size_t key_n = key_times.length / sizeof(int64_t);
        qsort(key_times.mutableBytes, key_n, sizeof(int64_t), compare);

        CMFormatDescriptionRef description = (__bridge CMFormatDescriptionRef)track.formatDescriptions.firstObject;
        CFTypeRef alpha = CMFormatDescriptionGetExtension(description, kCMFormatDescriptionExtension_ContainsAlphaChannel);
        CFTypeRef range = CMFormatDescriptionGetExtension(description, kCMFormatDescriptionExtension_FullRangeVideo);
        NSDictionary *atoms = (__bridge NSDictionary *)CMFormatDescriptionGetExtension(description, kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms);
        NSData *config = atoms[@"avcC"] ?: atoms[@"hvcC"];
        info->codec = CMFormatDescriptionGetMediaSubType(description);
        info->alpha = alpha && CFBooleanGetValue(alpha);
        info->profile = config.length > 1 ? ((const uint8_t *)config.bytes)[1] : -1;
        info->pixel_format = range && CFBooleanGetValue(range) ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
        info->timescale = timescale;

        NSError *reading = nil;
        AVAssetReader *reader = [AVAssetReader assetReaderWithAsset:asset error:&reading];
        AVAssetReaderTrackOutput *output = [AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:track outputSettings:@{
            (id)kCVPixelBufferPixelFormatTypeKey: @(info->pixel_format),
            (id)kCVPixelBufferIOSurfacePropertiesKey: @{},
        }];
        [reader addOutput:output];
        if (!reader || ![reader startReading]) {
            fail(error, error_length, [NSString stringWithFormat:@"cannot decode: %@", reader.error ?: reading]);
            return NULL;
        }
        CMSampleBufferRef sample = [output copyNextSampleBuffer];
        CVPixelBufferRef buffer = sample ? CMSampleBufferGetImageBuffer(sample) : NULL;
        if (!buffer) {
            if (sample) CFRelease(sample);
            fail(error, error_length, [NSString stringWithFormat:@"cannot decode: %@", reader.error]);
            return NULL;
        }
        info->width = (int32_t)CVPixelBufferGetWidth(buffer);
        info->height = (int32_t)CVPixelBufferGetHeight(buffer);
        info->matrix = matrix_of(buffer);
        CFRelease(sample);
        [reader cancelReading];

        *table = malloc(n * sizeof(int64_t));
        memcpy(*table, times.bytes, n * sizeof(int64_t));
        *count = n;
        *keys = malloc(key_n * sizeof(int64_t));
        memcpy(*keys, key_times.bytes, key_n * sizeof(int64_t));
        *key_count = key_n;
        VrfClip *clip = [VrfClip new];
        clip.asset = asset;
        clip.track = track;
        return (void *)CFBridgingRetain(clip);
    }
}

void *vrf_clip_open(const char *path, VrfClipInfo *info, int64_t **table, size_t *count, int64_t **keys, size_t *key_count, char *error, size_t error_length) {
    @try {
        return clip_open(path, info, table, count, keys, key_count, error, error_length);
    } @catch (NSException *exception) {
        fail(error, error_length, [NSString stringWithFormat:@"%@: %@", exception.name, exception.reason]);
        return NULL;
    }
}

void vrf_clip_close(void *clip) {
    CFBridgingRelease(clip);
}

static void *reader_open(void *clip_ref, int64_t from, uint32_t pixel_format, char *error, size_t error_length) {
    @autoreleasepool {
        VrfClip *clip = (__bridge VrfClip *)clip_ref;
        int32_t timescale = clip.track.naturalTimeScale;
        NSError *reason = nil;
        AVAssetReader *reader = [AVAssetReader assetReaderWithAsset:clip.asset error:&reason];
        if (!reader) {
            fail(error, error_length, reason.localizedDescription ?: @"cannot create a reader");
            return NULL;
        }
        AVAssetReaderTrackOutput *output = [AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:clip.track outputSettings:@{
            (id)kCVPixelBufferPixelFormatTypeKey: @(pixel_format),
            (id)kCVPixelBufferIOSurfacePropertiesKey: @{},
        }];
        output.alwaysCopiesSampleData = NO;
        reader.timeRange = CMTimeRangeMake(CMTimeMake(from, timescale), kCMTimePositiveInfinity);
        [reader addOutput:output];
        if (![reader startReading]) {
            fail(error, error_length, reader.error.localizedDescription ?: @"the reader did not start");
            return NULL;
        }
        VrfReader *handle = [VrfReader new];
        handle.reader = reader;
        handle.output = output;
        handle.timescale = timescale;
        return (void *)CFBridgingRetain(handle);
    }
}

void *vrf_reader_open(void *clip_ref, int64_t from, uint32_t pixel_format, char *error, size_t error_length) {
    @try {
        return reader_open(clip_ref, from, pixel_format, error, error_length);
    } @catch (NSException *exception) {
        fail(error, error_length, [NSString stringWithFormat:@"%@: %@", exception.name, exception.reason]);
        return NULL;
    }
}

CVPixelBufferRef vrf_reader_next(void *reader_ref, int64_t *pts) {
    VrfReader *handle = (__bridge VrfReader *)reader_ref;
    @try {
        @autoreleasepool {
            CMSampleBufferRef sample = [handle.output copyNextSampleBuffer];
            if (!sample) return NULL;
            CVPixelBufferRef buffer = CMSampleBufferGetImageBuffer(sample);
            if (buffer) {
                CFRetain(buffer);
                *pts = ticks(CMSampleBufferGetPresentationTimeStamp(sample), handle.timescale);
            }
            CFRelease(sample);
            return buffer;
        }
    } @catch (NSException *exception) {
        handle.failure = [NSString stringWithFormat:@"%@: %@", exception.name, exception.reason];
        return NULL;
    }
}

int vrf_reader_failed(void *reader_ref, char *error, size_t error_length) {
    @try {
        @autoreleasepool {
            VrfReader *handle = (__bridge VrfReader *)reader_ref;
            if (handle.failure) {
                fail(error, error_length, handle.failure);
                return 1;
            }
            if (handle.reader.status == AVAssetReaderStatusFailed) {
                fail(error, error_length, handle.reader.error.localizedDescription ?: @"the reader failed");
                return 1;
            }
            return 0;
        }
    } @catch (NSException *exception) {
        fail(error, error_length, [NSString stringWithFormat:@"%@: %@", exception.name, exception.reason]);
        return 1;
    }
}

void vrf_reader_close(void *reader_ref) {
    @try {
        VrfReader *handle = (VrfReader *)CFBridgingRelease(reader_ref);
        [handle.reader cancelReading];
    } @catch (NSException *exception) {
    }
}
