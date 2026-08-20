import Foundation
import CoreMediaIO
import CoreVideo
import CoreMedia

protocol RustFrameSourceDelegate: AnyObject {
    func frameSource(
        _ source: RustFrameSource,
        didReceiveSampleBuffer sampleBuffer: CMSampleBuffer,
        discontinuity: CMIOExtensionStream.DiscontinuityFlags,
        hostTimeInNanoseconds: UInt64
    )
}

/// 帧源：定时调用 Rust 核心渲染 BGRA32 帧 → CVPixelBuffer → CMSampleBuffer。
final class RustFrameSource {
    weak var delegate: RustFrameSourceDelegate?

    private let width: Int32
    private let height: Int32
    private let frameRate: Int32
    private let pattern: Int32
    private let formatDescription: CMFormatDescription
    private let bufferPool: CVPixelBufferPool
    private let queue = DispatchQueue(label: "com.vdev.camera.frames")
    private var timer: DispatchSourceTimer?

    init(
        formatDescription: CMFormatDescription,
        width: Int32,
        height: Int32,
        frameRate: Int32,
        pattern: Int32
    ) throws {
        self.formatDescription = formatDescription
        self.width = width
        self.height = height
        self.frameRate = frameRate
        self.pattern = pattern

        let attrs: NSDictionary = [
            kCVPixelBufferWidthKey: width,
            kCVPixelBufferHeightKey: height,
            kCVPixelBufferPixelFormatTypeKey: formatDescription.mediaSubType,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        var pool: CVPixelBufferPool?
        let status = CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, attrs, &pool)
        guard status == kCVReturnSuccess, let pool else {
            throw NSError(domain: "vdev.camera", code: Int(status),
                          userInfo: [NSLocalizedDescriptionKey: "CVPixelBufferPoolCreate failed: \(status)"])
        }
        self.bufferPool = pool
    }

    func startStreaming() {
        guard timer == nil else { return }
        let t = DispatchSource.makeTimerSource(flags: .strict, queue: queue)
        let intervalNs = 1_000_000_000 / Int(frameRate)
        t.schedule(deadline: .now(), repeating: .nanoseconds(intervalNs), leeway: .nanoseconds(intervalNs / 10))
        t.setEventHandler { [weak self] in self?.emitFrame() }
        t.resume()
        timer = t
    }

    func stopStreaming() {
        timer?.cancel()
        timer = nil
    }

    private func emitFrame() {
        guard let delegate else { return }
        do {
            let sample = try nextSampleBuffer()
            let now = CMClockGetTime(CMClockGetHostTimeClock())
            let ns = UInt64(now.seconds * Double(NSEC_PER_SEC))
            delegate.frameSource(self, didReceiveSampleBuffer: sample,
                                 discontinuity: [], hostTimeInNanoseconds: ns)
        } catch {
            NSLog("vdev-camera: frame error: \(error)")
        }
    }

    private func nextSampleBuffer() throws -> CMSampleBuffer {
        var pb: CVPixelBuffer?
        let aux: NSDictionary = [kCVPixelBufferPoolAllocationThresholdKey: 5]
        let status = CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(
            kCFAllocatorDefault, bufferPool, aux, &pb
        )
        guard status == kCVReturnSuccess, let pb else {
            throw NSError(domain: "vdev.camera", code: Int(status),
                          userInfo: [NSLocalizedDescriptionKey: "pixel buffer failed: \(status)"])
        }

        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        guard let base = CVPixelBufferGetBaseAddress(pb) else {
            throw NSError(domain: "vdev.camera", code: -1,
                          userInfo: [NSLocalizedDescriptionKey: "no base address"])
        }
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let t = CFAbsoluteTimeGetCurrent()
        let rc = vdev_camera_render_bgra32(
            pattern, UInt32(width), UInt32(height), t,
            base.assumingMemoryBound(to: UInt8.self), stride * Int(height)
        )
        guard rc == 0 else {
            throw NSError(domain: "vdev.camera", code: Int(rc),
                          userInfo: [NSLocalizedDescriptionKey: "rust render failed: \(rc)"])
        }

        var fmt: CMVideoFormatDescription?
        let fstatus = CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: nil, imageBuffer: pb, formatDescriptionOut: &fmt
        )
        guard fstatus == noErr, let fmt else {
            throw NSError(domain: "vdev.camera", code: Int(fstatus),
                          userInfo: [NSLocalizedDescriptionKey: "format description failed: \(fstatus)"])
        }

        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: frameRate),
            presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()),
            decodeTimeStamp: .invalid
        )
        var sample: CMSampleBuffer?
        let sstatus = CMSampleBufferCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: pb, dataReady: true,
            makeDataReadyCallback: nil, refcon: nil, formatDescription: fmt,
            sampleTiming: &timing, sampleBufferOut: &sample
        )
        guard sstatus == noErr, let sample else {
            throw NSError(domain: "vdev.camera", code: Int(sstatus),
                          userInfo: [NSLocalizedDescriptionKey: "sample buffer failed: \(sstatus)"])
        }
        return sample
    }
}
