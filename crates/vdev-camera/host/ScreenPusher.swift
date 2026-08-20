import Foundation
import ScreenCaptureKit
import CoreMedia
import CoreVideo

/// 屏幕推流：ScreenCaptureKit 采集指定显示器 → FrameChannelClient → 扩展 → 虚拟摄像头。
final class ScreenPusher: NSObject, SCStreamOutput {
    static let shared = ScreenPusher()

    private let queue = DispatchQueue(label: "vdev.screenpush")
    private var stream: SCStream?
    private var sentSinceStart = 0

    var isRunning: Bool { stream != nil }
    var onStateChange: ((Bool, String?) -> Void)?

    private override init() { super.init() }

    func start(displayID: CGDirectDisplayID? = nil, width: Int = 1920, height: Int = 1080, fps: Int = 30) {
        guard stream == nil else { return }
        FrameChannelClient.shared.connectIfNeeded()
        FrameChannelClient.shared.resetCount()
        sentSinceStart = 0
        FrameChannelClient.shared.onSent = { [weak self] n in
            self?.sentSinceStart = n
        }
        let task = Task {
            do {
                let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
                let display: SCDisplay
                if let displayID {
                    guard let d = content.displays.first(where: { $0.displayID == displayID }) else {
                        DispatchQueue.main.async { self.onStateChange?(false, "找不到显示器 ID \(displayID)") }
                        return
                    }
                    display = d
                } else {
                    guard let d = content.displays.first else {
                        DispatchQueue.main.async { self.onStateChange?(false, "没有可用显示器") }
                        return
                    }
                    display = d
                }
                let filter = SCContentFilter(display: display, excludingWindows: [])
                let config = SCStreamConfiguration()
                config.width = width
                config.height = height
                config.pixelFormat = kCVPixelFormatType_32BGRA
                config.minimumFrameInterval = CMTime(value: 1, timescale: Int32(fps))
                config.queueDepth = 4
                config.showsCursor = false
                let s = SCStream(filter: filter, configuration: config, delegate: nil)
                try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
                try await s.startCapture()
                self.stream = s
                DispatchQueue.main.async { self.onStateChange?(true, nil) }
            } catch {
                let msg = "屏幕推流失败：\(error.localizedDescription)\n需在 系统设置 → 隐私与安全性 → 屏幕录制 中允许 VDCamera。"
                DispatchQueue.main.async { self.onStateChange?(false, msg) }
            }
        }
        _ = task
    }

    func stop() {
        let s = stream
        stream = nil
        if let s {
            s.stopCapture { _ in }
        }
        onStateChange?(false, nil)
    }

    // MARK: SCStreamOutput
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, let pb = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return }
        let w = CVPixelBufferGetWidth(pb)
        let h = CVPixelBufferGetHeight(pb)
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let data = Data(bytes: base, count: stride * h)
        let now = CMClockGetTime(CMClockGetHostTimeClock())
        let ptsNs = UInt64(now.seconds * Double(NSEC_PER_SEC))
        FrameChannelClient.shared.sendFrame(data: data, width: w, height: h, stride: stride, ptsNs: ptsNs)
    }
}
