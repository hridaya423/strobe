import Foundation
import AVFoundation
import CoreVideo

let path = CommandLine.arguments[1], outPath = CommandLine.arguments[2]
let asset = AVURLAsset(url: URL(fileURLWithPath: path))
let tracks = try await asset.loadTracks(withMediaType: .video)
guard let track = tracks.first else { print("NO VIDEO TRACK"); exit(1) }
let reader = try AVAssetReader(asset: asset)
let out = AVAssetReaderTrackOutput(track: track, outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange])
reader.add(out)
guard reader.startReading() else { print("startReading failed: \(String(describing: reader.error))"); exit(2) }
FileManager.default.createFile(atPath: outPath, contents: nil)
let fh = FileHandle(forWritingAtPath: outPath)!
var n = 0
while let sb = out.copyNextSampleBuffer() {
    guard let pb = CMSampleBufferGetImageBuffer(sb) else { continue }
    CVPixelBufferLockBaseAddress(pb, .readOnly)
    for p in 0..<2 {
        let base = CVPixelBufferGetBaseAddressOfPlane(pb, p)!
        let rows = CVPixelBufferGetHeightOfPlane(pb, p), bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, p)
        let rw = CVPixelBufferGetWidthOfPlane(pb, p) * (p == 0 ? 1 : 2)
        for y in 0..<rows { fh.write(Data(bytes: base + y * bpr, count: rw)) }
    }
    CVPixelBufferUnlockBaseAddress(pb, .readOnly)
    n += 1
}
try fh.close()
print("AVFoundation decoded \(n) frames, reader.status=\(reader.status.rawValue) error=\(String(describing: reader.error))")
