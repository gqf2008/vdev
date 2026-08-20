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

    // 真实帧注入（来自 FrameChannel 推流）
    private struct InjectedFrame {
        let data: Data
        let width: Int32
        let height: Int32
        let stride: Int
        let ptsNs: UInt64
        let receivedAt: CFAbsoluteTime
    }
    private let injectLock = NSLock()
    private var injected: InjectedFrame?
    private let injectedFreshWindow: CFAbsoluteTime = 2.0 // 超过 2s 没有新帧才回落到 Rust 彩条（切换推流源不再闪）

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
        NSLog("vdev-camera: frameSource.startStreaming")
        guard timer == nil else { return }
        let t = DispatchSource.makeTimerSource(flags: .strict, queue: queue)
        let intervalNs = 1_000_000_000 / Int(frameRate)
        t.schedule(deadline: .now(), repeating: .nanoseconds(intervalNs), leeway: .nanoseconds(intervalNs / 10))
        t.setEventHandler { [weak self] in self?.emitFrame() }
        t.resume()
        timer = t
    }

    func stopStreaming() {
        NSLog("vdev-camera: frameSource.stopStreaming")
        timer?.cancel()
        timer = nil
    }

    /// 外部推流线程调用：写入一帧真实画面（线程安全）。
    func injectFrame(data: Data, width: Int32, height: Int32, stride: Int, ptsNs: UInt64) {
        injectLock.lock()
        injected = InjectedFrame(data: data, width: width, height: height,
                                 stride: stride, ptsNs: ptsNs,
                                 receivedAt: CFAbsoluteTimeGetCurrent())
        injectLock.unlock()
    }

    private func takeFreshInjectedFrame() -> InjectedFrame? {
        injectLock.lock()
        defer { injectLock.unlock() }
        guard let inj = injected,
              CFAbsoluteTimeGetCurrent() - inj.receivedAt < injectedFreshWindow else {
            return nil
        }
        return inj
    }

    private func emitFrame() {
        guard let delegate else { return }
        do {
            let sample: CMSampleBuffer
            let ns: UInt64
            if let inj = takeFreshInjectedFrame() {
                sample = try nextSampleBufferFromInjected(inj)
                ns = inj.ptsNs
            } else {
                sample = try nextSampleBuffer()
                let now = CMClockGetTime(CMClockGetHostTimeClock())
                ns = UInt64(now.seconds * Double(NSEC_PER_SEC))
            }
            delegate.frameSource(self, didReceiveSampleBuffer: sample,
                                 discontinuity: [], hostTimeInNanoseconds: ns)
        } catch {
            NSLog("vdev-camera: frame error: \(error)")
        }
    }

    /// 从推流通道收到的 BGRA32 裸数据构造 CMSampleBuffer。
    private func nextSampleBufferFromInjected(_ inj: InjectedFrame) throws -> CMSampleBuffer {
        let attrs: NSDictionary = [
            kCVPixelBufferWidthKey: inj.width,
            kCVPixelBufferHeightKey: inj.height,
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        var pb: CVPixelBuffer?
        let cstatus = CVPixelBufferCreate(
            kCFAllocatorDefault, Int(inj.width), Int(inj.height),
            kCVPixelFormatType_32BGRA, attrs, &pb)
        guard cstatus == kCVReturnSuccess, let pb else {
            throw NSError(domain: "vdev.camera", code: Int(cstatus),
                          userInfo: [NSLocalizedDescriptionKey: "pixel buffer create failed: \(cstatus)"])
        }
        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        guard let dst = CVPixelBufferGetBaseAddress(pb) else {
            throw NSError(domain: "vdev.camera", code: -1,
                          userInfo: [NSLocalizedDescriptionKey: "no base address"])
        }
        let dstStride = CVPixelBufferGetBytesPerRow(pb)
        guard inj.data.count >= inj.stride * Int(inj.height) else {
            throw NSError(domain: "vdev.camera", code: -2,
                          userInfo: [NSLocalizedDescriptionKey: "payload too short"])
        }
        inj.data.withUnsafeBytes { srcRaw in
            guard let src = srcRaw.baseAddress else { return }
            let srcPtr = src.assumingMemoryBound(to: UInt8.self)
            let dstPtr = dst.assumingMemoryBound(to: UInt8.self)
            if inj.stride == dstStride {
                memcpy(dstPtr, srcPtr, inj.stride * Int(inj.height))
            } else {
                for row in 0..<Int(inj.height) {
                    memcpy(dstPtr + row * dstStride, srcPtr + row * inj.stride, Int(inj.stride))
                }
            }
        }

        var fmt: CMVideoFormatDescription?
        let fstatus = CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: nil, imageBuffer: pb, formatDescriptionOut: &fmt)
        guard fstatus == noErr, let fmt else {
            throw NSError(domain: "vdev.camera", code: Int(fstatus),
                          userInfo: [NSLocalizedDescriptionKey: "format description failed: \(fstatus)"])
        }
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: frameRate),
            presentationTimeStamp: CMTime(value: Int64(inj.ptsNs), timescale: 1_000_000_000),
            decodeTimeStamp: .invalid
        )
        var sample: CMSampleBuffer?
        let sstatus = CMSampleBufferCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: pb, dataReady: true,
            makeDataReadyCallback: nil, refcon: nil, formatDescription: fmt,
            sampleTiming: &timing, sampleBufferOut: &sample)
        guard sstatus == noErr, let sample else {
            throw NSError(domain: "vdev.camera", code: Int(sstatus),
                          userInfo: [NSLocalizedDescriptionKey: "sample buffer failed: \(sstatus)"])
        }
        return sample
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
