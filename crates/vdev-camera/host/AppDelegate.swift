import Cocoa
import SystemExtensions

@main
class AppDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow!
    private var statusLabel: NSTextField!
    private var logView: NSTextView!
    private var installButton: NSButton!
    private var uninstallButton: NSButton!

    private var logs: [String] = []

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        buildUI()
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    // MARK: - UI

    private func buildUI() {
        let content = NSView(frame: NSRect(x: 0, y: 0, width: 520, height: 340))

        let title = NSTextField(labelWithString: "vdev-camera 宿主")
        title.font = .boldSystemFont(ofSize: 16)
        title.frame = NSRect(x: 20, y: 300, width: 480, height: 24)
        content.addSubview(title)

        let subtitle = NSTextField(wrappingLabelWithString:
            "安装/卸载 CMIO 摄像头扩展（com.apple.cmio.dal-extension）。安装后在 QuickTime / Zoom 中选择 vdev-camera 即可看到 Rust 生成的测试画面。")
        subtitle.frame = NSRect(x: 20, y: 262, width: 480, height: 36)
        content.addSubview(subtitle)

        installButton = NSButton(title: "安装虚拟摄像头", target: self, action: #selector(installTapped))
        installButton.frame = NSRect(x: 20, y: 220, width: 160, height: 32)
        content.addSubview(installButton)

        uninstallButton = NSButton(title: "卸载虚拟摄像头", target: self, action: #selector(uninstallTapped))
        uninstallButton.frame = NSRect(x: 190, y: 220, width: 160, height: 32)
        content.addSubview(uninstallButton)

        statusLabel = NSTextField(wrappingLabelWithString: "就绪")
        statusLabel.frame = NSRect(x: 20, y: 196, width: 480, height: 18)
        content.addSubview(statusLabel)

        let scroll = NSScrollView(frame: NSRect(x: 20, y: 20, width: 480, height: 168))
        logView = NSTextView(frame: scroll.bounds)
        logView.isEditable = false
        logView.isRichText = false
        logView.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        scroll.documentView = logView
        scroll.hasVerticalScroller = true
        content.addSubview(scroll)

        window = NSWindow(contentRect: content.bounds, styleMask: [.titled, .closable, .miniaturizable],
                          backing: .buffered, defer: false)
        window.title = "vdev-camera"
        window.contentView = content
        window.center()
        window.makeKeyAndOrderFront(nil)
    }

    private func log(_ s: String) {
        logs.append(s)
        logView.string = logs.joined(separator: "\n")
        logView.scrollToEndOfDocument(nil)
    }

    // MARK: - Actions

    @objc private func installTapped() {
        guard let identifier = Self.extensionBundle()?.bundleIdentifier else {
            log("错误：找不到扩展 bundle")
            return
        }
        log("激活扩展: \(identifier)")
        let req = OSSystemExtensionRequest.activationRequest(forExtensionWithIdentifier: identifier, queue: .main)
        req.delegate = self
        OSSystemExtensionManager.shared.submitRequest(req)
    }

    @objc private func uninstallTapped() {
        guard let identifier = Self.extensionBundle()?.bundleIdentifier else {
            log("错误：找不到扩展 bundle")
            return
        }
        log("停用扩展: \(identifier)")
        let req = OSSystemExtensionRequest.deactivationRequest(forExtensionWithIdentifier: identifier, queue: .main)
        req.delegate = self
        OSSystemExtensionManager.shared.submitRequest(req)
    }

    private static func extensionBundle() -> Bundle? {
        let dir = URL(fileURLWithPath: "Contents/Library/SystemExtensions", relativeTo: Bundle.main.bundleURL)
        guard let urls = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil, options: .skipsHiddenFiles),
              let first = urls.first else {
            return nil
        }
        return Bundle(url: first)
    }
}

extension AppDelegate: OSSystemExtensionRequestDelegate {
    func request(_ request: OSSystemExtensionRequest,
                 actionForReplacingExtension existing: OSSystemExtensionProperties,
                 withExtension ext: OSSystemExtensionProperties) -> OSSystemExtensionRequest.ReplacementAction {
        .replace
    }

    func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
        log("需要在 系统设置 → 隐私与安全性 中批准扩展")
        statusLabel.stringValue = "等待用户批准…"
    }

    func request(_ request: OSSystemExtensionRequest, didFinishWithResult result: OSSystemExtensionRequest.Result) {
        log("结果: \(result.rawValue)")
        statusLabel.stringValue = "完成 (result \(result.rawValue))"
    }

    func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
        log("失败: \(error)")
        statusLabel.stringValue = "失败：\(error.localizedDescription)"
    }
}
