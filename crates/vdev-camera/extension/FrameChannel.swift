import Foundation
import Network

protocol FrameChannelDelegate: AnyObject {
    func frameChannel(_ channel: FrameChannel,
                      didReceiveFrame data: Data,
                      width: Int32, height: Int32, stride: Int32, ptsNs: UInt64)
}

/// 真实帧推流通道：监听 127.0.0.1，接收外部进程推来的 BGRA32 帧。
///
/// 线协议（全部小端）：
///   36 字节头：
///     magic      u32 = 0x56444652 ("VDFR")
///     version    u32 = 1
///     width      u32
///     height     u32
///     stride     u32   （每行字节数，>= width*4）
///     ptsNs      u64   （host time 时钟纳秒）
///     payloadLen u64   （= stride * height）
///   payload：stride*height 字节 BGRA32
///
/// 同一时刻只服务一个推流连接，断开后继续等待下一个。
final class FrameChannel {
    weak var delegate: FrameChannelDelegate?

    private let queue = DispatchQueue(label: "com.vdev.camera.framechannel")
    private var listener: NWListener?
    private var connection: NWConnection?

    private var pending = Data()
    private var readingHeader = true
    private var expectedPayload = 0
    private var pendingHeader: (width: Int32, height: Int32, stride: Int32, ptsNs: UInt64)?

    static let headerSize = 36

    func start(port: UInt16 = 27890) {
        queue.async { [weak self] in
            guard let self else { return }
            let params = NWParameters.tcp
            params.allowLocalEndpointReuse = true
            do {
                let listener = try NWListener(using: params, on: NWEndpoint.Port(rawValue: port)!)
                listener.newConnectionHandler = { [weak self] conn in
                    self?.accept(conn)
                }
                listener.stateUpdateHandler = { state in
                    switch state {
                    case .ready:
                        NSLog("vdev-camera: FrameChannel 监听 127.0.0.1:%d", port)
                    case .failed(let err):
                        NSLog("vdev-camera: FrameChannel 监听失败: %@", String(describing: err))
                    default:
                        break
                    }
                }
                listener.start(queue: self.queue)
                self.listener = listener
            } catch {
                NSLog("vdev-camera: FrameChannel 启动失败: %@", String(describing: error))
            }
        }
    }

    func stop() {
        queue.async { [weak self] in
            self?.listener?.cancel()
            self?.listener = nil
            self?.connection?.cancel()
            self?.connection = nil
        }
    }

    private func accept(_ conn: NWConnection) {
        NSLog("vdev-camera: FrameChannel 推流客户端接入")
        connection?.cancel()
        connection = conn
        pending.removeAll()
        readingHeader = true
        expectedPayload = 0
        pendingHeader = nil
        conn.start(queue: queue)
        receiveLoop(conn)
    }

    private func receiveLoop(_ conn: NWConnection) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 1_048_576) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                self.pending.append(data)
                self.processPending()
            }
            if isComplete || error != nil {
                self.connection = nil
                return
            }
            self.receiveLoop(conn)
        }
    }

    private func processPending() {
        while true {
            if readingHeader {
                guard pending.count >= Self.headerSize else { return }
                let header = Data(pending.prefix(Self.headerSize))
                pending.removeFirst(Self.headerSize)
                let magic = u32(header, 0)
                guard magic == 0x56444652 else {
                    NSLog("vdev-camera: FrameChannel 头 magic 错误 0x%08x，断开", magic)
                    connection?.cancel()
                    connection = nil
                    return
                }
                let version = u32(header, 4)
                guard version == 1 else {
                    NSLog("vdev-camera: FrameChannel 版本不支持 %d", version)
                    connection?.cancel()
                    connection = nil
                    return
                }
                let width = Int32(bitPattern: u32(header, 8))
                let height = Int32(bitPattern: u32(header, 12))
                let stride = Int32(bitPattern: u32(header, 16))
                let ptsNs = u64(header, 20)
                let payloadLen = u64(header, 28)
                guard width > 0, height > 0, stride >= width * 4,
                      payloadLen == UInt64(stride) * UInt64(height), payloadLen <= 64 * 1024 * 1024 else {
                    NSLog("vdev-camera: FrameChannel 非法帧参数 w=%d h=%d stride=%d len=%llu", width, height, stride, payloadLen)
                    connection?.cancel()
                    connection = nil
                    return
                }
                pendingHeader = (width, height, stride, ptsNs)
                expectedPayload = Int(payloadLen)
                readingHeader = false
            } else {
                guard pending.count >= expectedPayload else { return }
                let payload = Data(pending.prefix(expectedPayload))
                pending.removeFirst(expectedPayload)
                if let h = pendingHeader {
                    delegate?.frameChannel(self, didReceiveFrame: payload,
                                           width: h.width, height: h.height,
                                           stride: h.stride, ptsNs: h.ptsNs)
                }
                readingHeader = true
                expectedPayload = 0
                pendingHeader = nil
            }
        }
    }

    private func u32(_ d: Data, _ off: Int) -> UInt32 {
        d.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: off, as: UInt32.self) }
    }

    private func u64(_ d: Data, _ off: Int) -> UInt64 {
        d.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: off, as: UInt64.self) }
    }
}
