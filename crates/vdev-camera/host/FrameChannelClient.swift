import Foundation
import Network

/// 与扩展 FrameChannel（127.0.0.1:27890）通信的共享客户端。
/// 协议见 extension/FrameChannel.swift。
final class FrameChannelClient {
    static let shared = FrameChannelClient()
    private static let port: UInt16 = 27890

    private var conn: NWConnection?
    private let queue = DispatchQueue(label: "vdev.frameclient")
    private var sending = false
    private(set) var connected = false
    private var sent = 0
    private var lastLog = Date.distantPast

    /// 每约 3 秒回调一次已推帧数
    var onSent: ((Int) -> Void)?

    private init() {}

    func connectIfNeeded() {
        guard conn == nil else { return }
        let c = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: Self.port)!,
                             using: .tcp)
        c.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                self?.connected = true
            case .failed(let err):
                self?.connected = false
                NSLog("vdev-camera: FrameChannel 连接失败 %@", String(describing: err))
            default:
                break
            }
        }
        c.start(queue: queue)
        conn = c
    }

    func resetCount() {
        sent = 0
        lastLog = .distantPast
    }

    func sendFrame(data: Data, width: Int, height: Int, stride: Int, ptsNs: UInt64) {
        guard let conn, connected, !sending else { return }
        sending = true
        conn.send(content: makeFrame(data: data, width: width, height: height,
                                     stride: stride, ptsNs: ptsNs),
                  completion: .contentProcessed { [weak self] _ in
            guard let self else { return }
            self.sending = false
            self.sent += 1
            let now = Date()
            if now.timeIntervalSince(self.lastLog) > 3 {
                self.onSent?(self.sent)
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
