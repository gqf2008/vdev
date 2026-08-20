import Foundation
import AVFoundation
import CoreMedia
import CoreVideo
import Accelerate

/// 视频文件推流：AVAssetReader 解码 → 缩放 → FrameChannelClient → 摄像头。
final class VideoPusher {
    static let shared = VideoPusher()

    private let queue = DispatchQueue(label: "vdev.videopush")
    private var reader: AVAssetReader?
    private var cancelled = false
    private var sentSinceStart = 0

    private(set) var isRunning = false
    var onStateChange: ((Bool, String?) -> Void)?

    private init() {}

    func start(url: URL, width: Int = 1920, height: Int = 1080, fps: Int = 60) {
        guard !isRunning else { return }
        isRunning = true
        cancelled = false
        sentSinceStart = 0
        FrameChannelClient.shared.connectIfNeeded()
        FrameChannelClient.shared.resetCount()
        FrameChannelClient.shared.onSent = { [weak self] n in self?.sentSinceStart = n }

        queue.async { [weak self] in
            guard let self else { return }
            let asset = AVURLAsset(url: url)
            guard let reader = try? AVAssetReader(asset: asset),
                  let track = asset.tracks(withMediaType: .video).first else {
                DispatchQueue.main.async { self.onStateChange?(false, "无法读取视频文件") }
                self.isRunning = false
                return
            }
            let settings: [String: Any] = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA]
            let output = AVAssetReaderTrackOutput(track: track, outputSettings: settings)
            output.alwaysCopiesSampleData = false
            reader.add(output)
            guard reader.startReading() else {
                DispatchQueue.main.async { self.onStateChange?(false, "视频解码失败") }
                self.isRunning = false
                return
            }
            self.reader = reader
            let intervalNs = UInt64(1_000_000_000 / Double(fps))
            let startHostNs = UInt64(CMClockGetTime(CMClockGetHostTimeClock()).seconds * Double(NSEC_PER_SEC))
            var index: UInt64 = 0

            while !self.cancelled, let sample = output.copyNextSampleBuffer() {
                defer { index += 1 }
                guard let pb = CMSampleBufferGetImageBuffer(sample) else { continue }
                let srcW = CVPixelBufferGetWidth(pb)
                let srcH = CVPixelBufferGetHeight(pb)
                let srcStride = CVPixelBufferGetBytesPerRow(pb)
                CVPixelBufferLockBaseAddress(pb, .readOnly)
                guard let base = CVPixelBufferGetBaseAddress(pb) else {
                    CVPixelBufferUnlockBaseAddress(pb, .readOnly)
                    continue
                }
                let raw = Data(bytes: base, count: srcStride * srcH)
                CVPixelBufferUnlockBaseAddress(pb, .readOnly)

                let scaled = Self.scaleBGRA(data: raw, srcW: srcW, srcH: srcH,
                                            srcStride: srcStride, dstW: width, dstH: height)
                let targetNs = startHostNs + index * intervalNs
                let nowNs = UInt64(CMClockGetTime(CMClockGetHostTimeClock()).seconds * Double(NSEC_PER_SEC))
                if targetNs > nowNs {
                    usleep(useconds_t((targetNs - nowNs) / 1000))
                }
                FrameChannelClient.shared.sendFrame(data: scaled.data, width: width, height: height,
                                                    stride: scaled.stride, ptsNs: targetNs)
            }
            reader.cancelReading()
            self.reader = nil
            DispatchQueue.main.async {
                self.isRunning = false
                if !self.cancelled {
                    self.onStateChange?(false, "视频推流结束")
                }
            }
        }
    }

    func stop() {
        cancelled = true
        reader?.cancelReading()
    }

    private static func scaleBGRA(data: Data, srcW: Int, srcH: Int, srcStride: Int,
                                  dstW: Int, dstH: Int) -> (data: Data, stride: Int) {
        guard srcW != dstW || srcH != dstH else { return (data, srcStride) }
        var src = vImage_Buffer(data: UnsafeMutableRawPointer(mutating: (data as NSData).bytes),
                                height: vImagePixelCount(srcH), width: vImagePixelCount(srcW), rowBytes: srcStride)
        var dstData = Data(count: dstW * dstH * 4)
        let dstStride = dstW * 4
        dstData.withUnsafeMutableBytes { raw in
            var dst = vImage_Buffer(data: raw.baseAddress, height: vImagePixelCount(dstH),
                                    width: vImagePixelCount(dstW), rowBytes: dstStride)
            vImageScale_ARGB8888(&src, &dst, nil, vImage_Flags(kvImageNoFlags))
        }
        return (dstData, dstStride)
    }
}
