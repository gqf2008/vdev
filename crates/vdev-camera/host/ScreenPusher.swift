import Foundation
import Network
import ScreenCaptureKit
import CoreMedia
import CoreVideo

/// 宿主 App 内置屏幕推流：ScreenCaptureKit 采集 → TCP 通道 → 扩展 → 虚拟摄像头。
final class ScreenPusher: NSObject, SCStreamOutput {
    static let shared = ScreenPusher()
    private static let port: UInt16 = 27890

    private var conn: NWConnection?
    private let queue = DispatchQueue(label: "vdev.push")
    private var sending = false
    private var connected = false
    private var stream: SCStream?
    private var sent = 0
    private var lastLog = Date.distantPast

    var isRunning: Bool { stream != nil }
    /// (运行中, 错误信息)
    var onStateChange: ((Bool, String?) -> Void)?

    private override init() { super.init() }

    func toggle() {
        isRunning ? stop() : start()
    }

    func start(width: Int = 1920, height: Int = 1080, fps: Int = 30) {
        guard stream == nil else { return }
        connectIfNeeded()
        let task = Task {
            do {
                let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
                guard let display = content.displays.first else {
                    self.onStateChange?(false, "没有可用显示器")
                    return
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
                self.onStateChange?(true, nil)
            } catch {
                self.onStateChange?(false, "屏幕推流失败：\(error.localizedDescription)\n需在 系统设置 → 隐私与安全性 → 屏幕录制 中允许 VDCamera。")
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
        conn?.cancel()
        conn = nil
        connected = false
        sent = 0
        onStateChange?(false, nil)
    }

    private func connectIfNeeded() {
        guard conn == nil else { return }
        let c = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: Self.port)!,
                             using: .tcp)
        c.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                self?.connected = true
            case .failed(let err):
                self?.connected = false
                NSLog("vdev-camera: 推流通道连接失败 %@", String(describing: err))
            default:
                break
            }
        }
        c.start(queue: queue)
        conn = c
    }

    // MARK: SCStreamOutput
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, let pb = CMSampleBufferGetImageBuffer(sampleBuffer),
              connected, !sending else { return }
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return }
        let w = CVPixelBufferGetWidth(pb)
        let h = CVPixelBufferGetHeight(pb)
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let data = Data(bytes: base, count: stride * h)
        let now = CMClockGetTime(CMClockGetHostTimeClock())
        let ptsNs = UInt64(now.seconds * Double(NSEC_PER_SEC))
        sendFrame(data: data, width: w, height: h, stride: stride, ptsNs: ptsNs)
    }

    private func sendFrame(data: Data, width: Int, height: Int, stride: Int, ptsNs: UInt64) {
        guard let conn, connected, !sending else { return }
        sending = true
        conn.send(content: makeFrame(data: data, width: width, height: height,
                                     stride: stride, ptsNs: ptsNs),
                  completion: .contentProcessed { [weak self] _ in
            guard let self else { return }
            self.sending = false
            self.sent += 1
            let now = Date()
            if now.timeIntervalSince(self.lastLog) > 5 {
                NSLog("vdev-camera: 屏幕推流 %d 帧", self.sent)
                self.lastLog = now
            }
        })
    }

    private func makeFrame(data: Data, width: Int, height: Int, stride: Int, ptsNs: UInt64) -> Data {
        var header = Data(capacity: 36 + data.count)
        func appendU32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { header.append(contentsOf: $0) } }
        func appendU64(_ v: UInt64) { withUnsafeBytes(of: v.littleEndian) { header.append(contentsOf: $0) } }
        appendU32(0x56444652) // "VDFR"
        appendU32(1)
        appendU32(UInt32(width))
        appendU32(UInt32(height))
        appendU32(UInt32(stride))
        appendU64(ptsNs)
        appendU64(UInt64(data.count))
        header.append(data)
        return header
    }
}
