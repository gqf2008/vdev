import Foundation

/// 虚拟屏幕管理：调用 App 内捆绑的 vdev-cli 创建/销毁 CGVirtualDisplay。
final class VirtualDisplayManager {
    static let shared = VirtualDisplayManager()

    private var proc: Process?
    private(set) var displayID: UInt32?
    private var displayHex: String?

    var isRunning: Bool { proc != nil }
    /// (运行中, 信息/错误, 显示器ID)
    var onStateChange: ((Bool, String?, UInt32?) -> Void)?

    private init() {}

    func create(width: Int = 1920, height: Int = 1080, name: String = "vdev-demo") {
        guard proc == nil else { return }
        guard let bin = Bundle.main.url(forAuxiliaryExecutable: "vdev-cli") else {
            onStateChange?(false, "未找到 vdev-cli（App 构建时未捆绑）", nil)
            return
        }
        let p = Process()
        p.executableURL = bin
        p.arguments = ["screen", "create", "--width", "\(width)", "--height", "\(height)",
                       "--name", name, "--hold", "86400"]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        p.terminationHandler = { [weak self] _ in
            DispatchQueue.main.async {
                guard let self, self.proc != nil else { return }
                self.proc = nil
                self.displayID = nil
                self.displayHex = nil
                self.onStateChange?(false, "虚拟屏幕已销毁", nil)
            }
        }
        do {
            try p.run()
            proc = p
            onStateChange?(true, "正在创建虚拟屏幕…", nil)
        } catch {
            onStateChange?(false, "启动 vdev-cli 失败：\(error.localizedDescription)", nil)
            return
        }
        pipe.fileHandleForReading.readabilityHandler = { [weak self] fh in
            let data = fh.availableData
            guard let s = String(data: data, encoding: .utf8), !s.isEmpty else { return }
            if let range = s.range(of: "0x[0-9a-fA-F]+", options: .regularExpression) {
                let hex = String(s[range])
                let id = UInt32(hex.dropFirst(2), radix: 16) ?? 0
                DispatchQueue.main.async {
                    self?.displayID = id
                    self?.displayHex = hex
                    self?.onStateChange?(true, "虚拟屏幕已创建 \(hex)（1920x1080@60）", id)
                }
            }
        }
    }

    func destroy() {
        if let p = proc, p.isRunning {
            p.terminate()
        }
        proc = nil
        displayID = nil
        displayHex = nil
    }
}
