import Foundation
import AVFoundation

let path = CommandLine.arguments[1], outPath = CommandLine.arguments[2]
let asset = AVURLAsset(url: URL(fileURLWithPath: path))
let tracks = try await asset.loadTracks(withMediaType: .audio)
guard let track = tracks.first else { print("NO AUDIO TRACK"); exit(1) }
let reader = try AVAssetReader(asset: asset)
let out = AVAssetReaderTrackOutput(track: track, outputSettings: [
    AVFormatIDKey: kAudioFormatLinearPCM,
    AVSampleRateKey: 48000,
    AVNumberOfChannelsKey: 2,
    AVLinearPCMBitDepthKey: 32,
    AVLinearPCMIsFloatKey: true,
    AVLinearPCMIsBigEndianKey: false,
    AVLinearPCMIsNonInterleaved: false,
])
reader.add(out)
guard reader.startReading() else { print("startReading failed: \(String(describing: reader.error))"); exit(2) }
FileManager.default.createFile(atPath: outPath, contents: nil)
let fh = FileHandle(forWritingAtPath: outPath)!
var frames = 0
var first: Double? = nil
while let sb = out.copyNextSampleBuffer() {
    if first == nil { first = CMSampleBufferGetPresentationTimeStamp(sb).seconds }
    guard let block = CMSampleBufferGetDataBuffer(sb) else { continue }
    var length = 0
    var pointer: UnsafeMutablePointer<Int8>? = nil
    CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &pointer)
    fh.write(Data(bytes: pointer!, count: length))
    frames += CMSampleBufferGetNumSamples(sb)
}
try fh.close()
let range = try await track.load(.timeRange)
print("AVFoundation decoded \(frames) frames, first pts \(first ?? -1), track start \(range.start.seconds) duration \(range.duration.seconds), reader.status=\(reader.status.rawValue) error=\(String(describing: reader.error))")
