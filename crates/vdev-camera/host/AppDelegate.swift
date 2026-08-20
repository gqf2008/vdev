import Cocoa
import SystemExtensions
import CoreMediaIO
import AVFoundation

/// 摄像头扩展安装状态
private enum CameraState {
    case idle          // 未安装
    case activating    // 正在安装/校验中
    case verifying     // 安装/卸载请求完成，正在确认系统状态
    case awaitingApproval // 等待用户在系统设置批准
    case enabled       // 已安装且系统可见
    case uninstalling  // 正在卸载
    case disabled      // 已卸载
    case error(String) // 失败（携带说明）
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow!
    private var statusGlyph: NSTextField!
    private var statusTitle: NSTextField!
    private var statusDetail: NSTextField!
    private var installButton: NSButton!
    private var uninstallButton: NSButton!
    private var settingsButton: NSButton!
    private var quicktimeButton: NSButton!
    private var pushButton: NSButton!
    private var videoPushButton: NSButton!
    private var vdCreateButton: NSButton!
    private var vdPushButton: NSButton!
    private var refreshButton: NSButton!
    private var logView: NSTextView!
    private var logs: [String] = []
    private let pusher = ScreenPusher.shared

    private var state: CameraState = .idle {
        didSet { renderState() }
    }
    private var isUninstalling = false

    // MARK: - 生命周期

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildUI()
        window.orderFrontRegardless()
        NSApp.activate(ignoringOtherApps: true)
        refreshStatus()
        pusher.onStateChange = { [weak self] running, error in
            self?.pushButton.title = running ? "停止推流" : "屏幕推流"
            if let error {
                self?.log(error)
                self?.state = .error(error)
            } else if running {
                self?.log("屏幕推流已开始（1080p@30）")
            } else {
                self?.log("屏幕推流已停止")
            }
        }
        VideoPusher.shared.onStateChange = { [weak self] running, error in
            self?.videoPushButton.title = running ? "停止视频推流" : "视频推流"
            if let error {
                self?.log(error)
                if !(error == "视频推流结束") { self?.state = .error(error) }
            } else if running {
                self?.log("视频推流已开始")
            } else {
                self?.log("视频推流已停止")
            }
        }
        VirtualDisplayManager.shared.onStateChange = { [weak self] running, info, id in
            self?.vdCreateButton.title = running ? "销毁虚拟屏幕" : "创建虚拟屏幕"
            if let info { self?.log(info) }
            self?.vdPushButton.isEnabled = (id != nil)
        }
        // 摄像头设备增删时自动刷新状态（不用重启 App）
        NotificationCenter.default.addObserver(
            self, selector: #selector(devicesDidChange),
            name: .AVCaptureDeviceWasConnected, object: nil)
        NotificationCenter.default.addObserver(
            self, selector: #selector(devicesDidChange),
            name: .AVCaptureDeviceWasDisconnected, object: nil)
    }

    @objc private func devicesDidChange() {
        // 操作进行中不打扰；空闲/已装/已卸/错误态下自动刷新
        switch state {
        case .idle, .enabled, .disabled, .error:
            refreshStatus()
        default:
            break
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        ScreenPusher.shared.stop()
        VideoPusher.shared.stop()
        VirtualDisplayManager.shared.destroy()
    }

    // MARK: - UI

    private func buildUI() {
        // 高度留出顶部 12pt：去掉标题栏后红绿灯悬浮在内容区左上角，28pt 足够避开
        let content = NSView(frame: NSRect(x: 0, y: 0, width: 580, height: 512))

        let title = NSTextField(labelWithString: "VDCamera")
        title.font = .boldSystemFont(ofSize: 18)
        title.frame = NSRect(x: 24, y: 458, width: 360, height: 26)
        content.addSubview(title)

        let subtitle = NSTextField(wrappingLabelWithString:
            "安装/卸载 CMIO 摄像头扩展，安装后在 QuickTime / Zoom 中选择 vdev-camera 即可看到 Rust 生成的测试画面。")
        subtitle.textColor = .secondaryLabelColor
        subtitle.font = .systemFont(ofSize: 12)
        subtitle.frame = NSRect(x: 24, y: 424, width: 532, height: 32)
        content.addSubview(subtitle)

        // 状态卡片
        let card = NSBox(frame: NSRect(x: 24, y: 300, width: 532, height: 116))
        card.boxType = .custom
        card.cornerRadius = 12
        card.borderWidth = 1
        card.borderColor = .separatorColor
        content.addSubview(card)

        statusGlyph = NSTextField(labelWithString: "●")
        statusGlyph.font = .systemFont(ofSize: 16)
        statusGlyph.frame = NSRect(x: 40, y: 384, width: 24, height: 22)
        content.addSubview(statusGlyph)

        statusTitle = NSTextField(labelWithString: "")
        statusTitle.font = .boldSystemFont(ofSize: 15)
        statusTitle.lineBreakMode = .byTruncatingTail
        statusTitle.frame = NSRect(x: 68, y: 384, width: 460, height: 22)
        content.addSubview(statusTitle)

        statusDetail = NSTextField(wrappingLabelWithString: "")
        statusDetail.textColor = .secondaryLabelColor
        statusDetail.font = .systemFont(ofSize: 12)
        statusDetail.frame = NSRect(x: 40, y: 308, width: 500, height: 72)
        content.addSubview(statusDetail)

        // 主按钮
        installButton = NSButton(title: "安装虚拟摄像头", target: self, action: #selector(installTapped))
        installButton.bezelStyle = .rounded
        installButton.frame = NSRect(x: 24, y: 252, width: 170, height: 34)
        content.addSubview(installButton)

        uninstallButton = NSButton(title: "卸载虚拟摄像头", target: self, action: #selector(uninstallTapped))
        uninstallButton.bezelStyle = .rounded
        uninstallButton.frame = NSRect(x: 204, y: 252, width: 170, height: 34)
        content.addSubview(uninstallButton)

        settingsButton = NSButton(title: "打开系统设置", target: self, action: #selector(settingsTapped))
        settingsButton.bezelStyle = .rounded
        settingsButton.frame = NSRect(x: 384, y: 252, width: 170, height: 34)
        content.addSubview(settingsButton)

        // 辅助按钮
        quicktimeButton = NSButton(title: "在 QuickTime 中打开", target: self, action: #selector(quicktimeTapped))
        quicktimeButton.bezelStyle = .inline
        quicktimeButton.frame = NSRect(x: 24, y: 210, width: 170, height: 30)
        content.addSubview(quicktimeButton)

        pushButton = NSButton(title: "屏幕推流", target: self, action: #selector(pushTapped))
        pushButton.bezelStyle = .inline
        pushButton.frame = NSRect(x: 204, y: 210, width: 140, height: 30)
        content.addSubview(pushButton)

        refreshButton = NSButton(title: "刷新状态", target: self, action: #selector(refreshTapped))
        refreshButton.bezelStyle = .inline
        refreshButton.frame = NSRect(x: 354, y: 210, width: 120, height: 30)
        content.addSubview(refreshButton)

        // 推流/虚拟屏幕控制行
        videoPushButton = NSButton(title: "视频推流", target: self, action: #selector(videoPushTapped))
        videoPushButton.bezelStyle = .inline
        videoPushButton.frame = NSRect(x: 24, y: 174, width: 160, height: 30)
        content.addSubview(videoPushButton)

        vdCreateButton = NSButton(title: "创建虚拟屏幕", target: self, action: #selector(vdCreateTapped))
        vdCreateButton.bezelStyle = .inline
        vdCreateButton.frame = NSRect(x: 194, y: 174, width: 150, height: 30)
        content.addSubview(vdCreateButton)

        vdPushButton = NSButton(title: "推虚拟屏幕", target: self, action: #selector(vdPushTapped))
        vdPushButton.bezelStyle = .inline
        vdPushButton.frame = NSRect(x: 354, y: 174, width: 170, height: 30)
        content.addSubview(vdPushButton)

        // 日志
        let scroll = NSScrollView(frame: NSRect(x: 24, y: 16, width: 532, height: 150))
        logView = NSTextView(frame: scroll.bounds)
        logView.isEditable = false
        logView.isRichText = false
        logView.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        logView.textContainerInset = NSSize(width: 4, height: 4)
        scroll.documentView = logView
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        content.addSubview(scroll)

        window = NSWindow(contentRect: content.bounds,
                          styleMask: [.titled, .closable, .miniaturizable, .fullSizeContentView],
                          backing: .buffered, defer: false)
        window.title = "VDCamera"
        // 去掉标题栏：透明 + 隐藏标题，保留左上角红绿灯，可拖动窗口背景
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isMovableByWindowBackground = true
        window.contentView = content
        window.center()
        window.makeKeyAndOrderFront(nil)
    }

    private func log(_ s: String) {
        let ts = DateFormatter.localizedString(from: Date(), dateStyle: .none, timeStyle: .medium)
        logs.append("[\(ts)] \(s)")
        if logs.count > 200 { logs.removeFirst(logs.count - 200) }
        logView.string = logs.joined(separator: "\n")
        logView.scrollToEndOfDocument(nil)
    }

    // MARK: - 状态渲染

    private func renderState() {
        switch state {
        case .idle:
            statusGlyph.stringValue = "○"
            statusGlyph.textColor = .secondaryLabelColor
            statusTitle.stringValue = "虚拟摄像头未安装"
            statusDetail.stringValue = "点击「安装虚拟摄像头」，然后在系统弹出的提示中允许扩展。"
            settingsButton.isHidden = true
            quicktimeButton.isHidden = true
            pushButton.isEnabled = false
            videoPushButton.isEnabled = false
            vdPushButton.isEnabled = false
            installButton.isEnabled = true
            uninstallButton.isEnabled = false
        case .activating:
            statusGlyph.stringValue = "⏳"
            statusGlyph.textColor = .systemOrange
            statusTitle.stringValue = "正在安装…"
            statusDetail.stringValue = "正在请求系统激活摄像头扩展，请稍候。"
            settingsButton.isHidden = true
            quicktimeButton.isHidden = true
            pushButton.isEnabled = false
            videoPushButton.isEnabled = false
            vdPushButton.isEnabled = false
            installButton.isEnabled = false
            uninstallButton.isEnabled = false
        case .verifying:
            statusGlyph.stringValue = "⏳"
            statusGlyph.textColor = .systemOrange
            statusTitle.stringValue = "正在确认…"
            statusDetail.stringValue = "请求已完成，正在确认系统是否能看到 vdev-camera，最长约 15 秒。"
            settingsButton.isHidden = true
            quicktimeButton.isHidden = true
            installButton.isEnabled = false
            uninstallButton.isEnabled = false
        case .awaitingApproval:
            statusGlyph.stringValue = "⚠️"
            statusGlyph.textColor = .systemOrange
            statusTitle.stringValue = "等待系统批准"
            statusDetail.stringValue = "已为你打开「系统设置」。请进入：通用 → 登录项与扩展 → 扩展 → 按类别 → 相机扩展，打开 vdev-camera 的开关。\n没有看到入口？点下方「打开系统设置」重试。"
            settingsButton.isHidden = false
            quicktimeButton.isHidden = true
            pushButton.isEnabled = true
            videoPushButton.isEnabled = true
            vdPushButton.isEnabled = (VirtualDisplayManager.shared.displayID != nil)
            installButton.isEnabled = true
            uninstallButton.isEnabled = false
        case .enabled:
            statusGlyph.stringValue = "✓"
            statusGlyph.textColor = .systemGreen
            statusTitle.stringValue = "已安装，摄像头可用"
            statusDetail.stringValue = "系统已能看到 vdev-camera。打开 QuickTime → 新建影片录制 → 选择 vdev-camera，即可看到 Rust 生成的测试画面。"
            settingsButton.isHidden = false
            quicktimeButton.isHidden = false
            pushButton.isEnabled = true
            videoPushButton.isEnabled = false
            vdPushButton.isEnabled = false
            installButton.isEnabled = false
            uninstallButton.isEnabled = true
        case .uninstalling:
            statusGlyph.stringValue = "⏳"
            statusGlyph.textColor = .systemOrange
            statusTitle.stringValue = "正在卸载…"
            statusDetail.stringValue = "正在从系统移除摄像头扩展，请稍候。"
            settingsButton.isHidden = true
            quicktimeButton.isHidden = true
            pushButton.isEnabled = false
            videoPushButton.isEnabled = false
            vdPushButton.isEnabled = false
            installButton.isEnabled = false
            uninstallButton.isEnabled = false
        case .disabled:
            statusGlyph.stringValue = "○"
            statusGlyph.textColor = .secondaryLabelColor
            statusTitle.stringValue = "已卸载"
            statusDetail.stringValue = "vdev-camera 已从系统移除。"
            settingsButton.isHidden = true
            quicktimeButton.isHidden = true
            pushButton.isEnabled = false
            videoPushButton.isEnabled = false
            vdPushButton.isEnabled = false
            installButton.isEnabled = true
            uninstallButton.isEnabled = false
        case .error(let message):
            statusGlyph.stringValue = "✕"
            statusGlyph.textColor = .systemRed
            statusTitle.stringValue = "操作失败"
            statusDetail.stringValue = message
            settingsButton.isHidden = false
            quicktimeButton.isHidden = true
            pushButton.isEnabled = false
            videoPushButton.isEnabled = false
            vdPushButton.isEnabled = false
            installButton.isEnabled = true
            uninstallButton.isEnabled = true
        }
    }

    // MARK: - 安装状态检测（CMIO HAL，无需摄像头权限）

    private static func cmioCameraNames() -> [String] {
        var addr = CMIOObjectPropertyAddress(
            mSelector: CMIOObjectPropertySelector(kCMIOHardwarePropertyDevices),
            mScope: CMIOObjectPropertyScope(kCMIOObjectPropertyScopeGlobal),
            mElement: CMIOObjectPropertyElement(kCMIOObjectPropertyElementMain))
        var size: UInt32 = 0
        guard CMIOObjectGetPropertyDataSize(CMIOObjectID(kCMIOObjectSystemObject), &addr, 0, nil, &size) == noErr else {
            return []
        }
        let count = min(size / UInt32(MemoryLayout<CMIOObjectID>.size), 64)
        guard count > 0 else { return [] }
        var ids = [CMIOObjectID](repeating: 0, count: Int(count))
        var outCount = count
        // 必须用 withUnsafeMutableBytes 拿到数组缓冲区裸指针（直接 &ids 不可靠）
        let rc = ids.withUnsafeMutableBytes { raw -> OSStatus in
            CMIOObjectGetPropertyData(CMIOObjectID(kCMIOObjectSystemObject), &addr, 0, nil,
                                      size, &outCount, raw.baseAddress)
        }
        guard rc == noErr else { return [] }
        let actual = Int(outCount)
        guard actual <= ids.count else { return [] }
        var names: [String] = []
        for idx in 0..<actual {
            let oid = ids[idx]
            var nameAddr = CMIOObjectPropertyAddress(
                mSelector: CMIOObjectPropertySelector(kCMIOObjectPropertyName),
                mScope: CMIOObjectPropertyScope(kCMIOObjectPropertyScopeGlobal),
                mElement: CMIOObjectPropertyElement(kCMIOObjectPropertyElementMain))
            var name: CFString?
            var nameSize = UInt32(MemoryLayout<CFString?>.size)
            if CMIOObjectGetPropertyData(oid, &nameAddr, 0, nil, nameSize, &nameSize, &name) == noErr,
               let name = name {
                names.append(name as String)
            }
        }
        return names
    }

    private static func avCameraNames() -> [String] {
        if #available(macOS 14.0, *) {
            let session = AVCaptureDevice.DiscoverySession(
                deviceTypes: [.external, .builtInWideAngleCamera, .continuityCamera],
                mediaType: .video, position: .unspecified)
            return session.devices.map { $0.localizedName }
        } else {
            let session = AVCaptureDevice.DiscoverySession(
                deviceTypes: [.builtInWideAngleCamera],
                mediaType: .video, position: .unspecified)
            return session.devices.map { $0.localizedName }
        }
    }

    private static func cameraFound() -> Bool {
        cmioCameraNames().contains { $0.localizedCaseInsensitiveContains("vdev-camera") }
            || avCameraNames().contains { $0.localizedCaseInsensitiveContains("vdev-camera") }
    }

    /// 轮询等待摄像头出现/消失（最多约 15 秒；重启用后 cmiod 注册可能较慢）
    private func waitForCamera(present: Bool, completion: @escaping (Bool) -> Void) {
        var tries = 0
        func tick() {
            tries += 1
            if Self.cameraFound() == present || tries >= 30 {
                completion(Self.cameraFound() == present)
                return
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { tick() }
        }
        tick()
    }

    private func refreshStatus() {
        let cmio = Self.cmioCameraNames()
        let av = Self.avCameraNames()
        log("检测摄像头: CMIO=[\(cmio.joined(separator: ", "))] AV=[\(av.joined(separator: ", "))]")
        state = (cmio + av).contains { $0.localizedCaseInsensitiveContains("vdev-camera") } ? .enabled : .idle
    }

    // MARK: - 动作

    @objc private func installTapped() {
        guard let identifier = Self.extensionBundle()?.bundleIdentifier else {
            state = .error("找不到扩展 bundle，请重新安装 App。")
            log("错误：找不到扩展 bundle")
            return
        }
        log("激活扩展: \(identifier)")
        state = .activating
        let req = OSSystemExtensionRequest.activationRequest(forExtensionWithIdentifier: identifier, queue: .main)
        req.delegate = self
        OSSystemExtensionManager.shared.submitRequest(req)
    }

    @objc private func uninstallTapped() {
        if pusher.isRunning { pusher.stop() }
        guard let identifier = Self.extensionBundle()?.bundleIdentifier else {
            state = .error("找不到扩展 bundle，请重新安装 App。")
            log("错误：找不到扩展 bundle")
            return
        }
        log("停用扩展: \(identifier)")
        state = .uninstalling
        isUninstalling = true
        let req = OSSystemExtensionRequest.deactivationRequest(forExtensionWithIdentifier: identifier, queue: .main)
        req.delegate = self
        OSSystemExtensionManager.shared.submitRequest(req)
    }

    @objc private func settingsTapped() {
        openSettings()
    }

    @objc private func quicktimeTapped() {
        if NSWorkspace.shared.launchApplication(withBundleIdentifier: "com.apple.QuickTimePlayerX",
                                                options: [.default],
                                                additionalEventParamDescriptor: nil,
                                                launchIdentifier: nil) {
            log("已打开 QuickTime，请选择 文件 → 新建影片录制 → vdev-camera")
        } else {
            log("打开 QuickTime 失败")
        }
    }

    @objc private func videoPushTapped() {
        if VideoPusher.shared.isRunning {
            VideoPusher.shared.stop()
            return
        }
        guard case .enabled = state else {
            log("请先安装虚拟摄像头再推流")
            return
        }
        let panel = NSOpenPanel()
        panel.allowedFileTypes = ["mp4", "mov", "m4v", "mkv"]
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.beginSheetModal(for: window) { [weak self] resp in
            guard resp == .OK, let url = panel.url else { return }
            self?.log("视频推流: \(url.lastPathComponent)")
            VideoPusher.shared.start(url: url)
        }
    }

    @objc private func vdCreateTapped() {
        if VirtualDisplayManager.shared.isRunning {
            VirtualDisplayManager.shared.destroy()
            return
        }
        VirtualDisplayManager.shared.create()
    }

    @objc private func vdPushTapped() {
        guard let id = VirtualDisplayManager.shared.displayID else {
            log("请先创建虚拟屏幕")
            return
        }
        if ScreenPusher.shared.isRunning {
            ScreenPusher.shared.stop()
            return
        }
        guard case .enabled = state else {
            log("请先安装虚拟摄像头再推流")
            return
        }
        pusher.start(displayID: id)
    }

    @objc private func pushTapped() {
        guard case .enabled = state else {
            log("请先安装虚拟摄像头再推流")
            return
        }
        if pusher.isRunning {
            pusher.stop()
        } else {
            pusher.start()
        }
    }

    @objc private func refreshTapped() {
        log("刷新状态…")
        refreshStatus()
    }

    /// 打开系统设置 → 通用 → 登录项与扩展（macOS 26 摄像头扩展入口所在页）
    private func openSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.ExtensionsPreferences") {
            NSWorkspace.shared.open(url)
            log("已打开系统设置：通用 → 登录项与扩展 → 扩展 → 按类别 → 相机扩展")
        }
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

// MARK: - OSSystemExtensionRequestDelegate

extension AppDelegate: OSSystemExtensionRequestDelegate {
    func request(_ request: OSSystemExtensionRequest,
                 actionForReplacingExtension existing: OSSystemExtensionProperties,
                 withExtension ext: OSSystemExtensionProperties) -> OSSystemExtensionRequest.ReplacementAction {
        .replace
    }

    func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
        if isUninstalling {
            log("卸载需要系统确认，请在弹出的系统提示中确认")
        } else {
            log("需要用户在系统设置中批准")
            state = .awaitingApproval
            openSettings()
        }
    }

    func request(_ request: OSSystemExtensionRequest, didFinishWithResult result: OSSystemExtensionRequest.Result) {
        log("请求完成 result=\(result.rawValue)")
        switch result {
        case .completed:
            if isUninstalling {
                // 校验摄像头是否已消失（最长 15s）
                state = .uninstalling
                waitForCamera(present: false) { gone in
                    self.isUninstalling = false
                    if gone {
                        self.state = .disabled
                        self.log("vdev-camera 已从系统移除")
                    } else {
                        self.state = .error("卸载请求已完成，但摄像头仍在列表中。\n可点「刷新状态」重试；若仍未消失，请到 系统设置 → … → 相机扩展 手动关闭。")
                    }
                }
            } else {
                // 校验系统是否真的能看到摄像头（最长 15s）
                state = .verifying
                waitForCamera(present: true) { ok in
                    if ok {
                        self.log("检测到 vdev-camera，安装成功")
                        self.state = .enabled
                    } else {
                        self.log("请求完成但 15 秒内未检测到摄像头")
                        self.state = .error("安装请求已完成，但暂未检测到摄像头。\n请点「刷新状态」重试；若仍未出现，请到 系统设置 → 通用 → 登录项与扩展 → 扩展 → 按类别 → 相机扩展 确认 vdev-camera 已打开。")
                    }
                }
            }
        case .willCompleteAfterReboot:
            log("需要重启电脑后生效")
            isUninstalling = false
            state = .error("需要重启电脑后生效。重启后 vdev-camera 会自动完成安装/卸载。")
        default:
            isUninstalling = false
            state = .error("系统返回了未预期的结果（\(result.rawValue)），请重试。")
        }
    }

    func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
        log("失败: \(error)")
        let ns = error as NSError
        let desc = error.localizedDescription
        if ns.code == 3 {
            state = .error("必须从 /Applications/VDCamera.app 启动本程序才能安装扩展。\n请退出当前窗口，打开 /Applications 里的 VDCamera.app 重试。")
            log("错误：App 不在 /Applications 目录（Code=3），旧构建产物会被系统拒绝")
            return
        }
        if isUninstalling {
            isUninstalling = false
            if ns.code == 4 {
                state = .disabled
                log("未找到已激活的扩展（可能版本不一致），已视为卸载完成；旧版本会在重启后自动清理。")
            } else {
                state = .error("卸载失败：\(desc)\n\n可尝试：点「打开系统设置」手动关闭 vdev-camera，或重启电脑后重试。")
            }
            return
        }
        state = .error("\(desc)\n\n可尝试：点「打开系统设置」查看摄像头扩展列表，或重启电脑后重试。")
    }
}
