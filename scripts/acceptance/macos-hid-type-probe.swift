// vdev HID 文本注入验收探针：AppKit 窗口收键 + 全局 EventTap 计数。
//
// 编译：swiftc -O macos-hid-type-probe.swift -o /tmp/...
// 用法：probe [outfile] [seconds]
//   - outfile 可选；由 `open` 启动时 stdout 不可见，验收脚本靠该文件读
//     READY/SUMMARY/TYPED；
//   - 只传数字时等价旧用法（stdout 输出）。
// 只在 active=true 且 key=true 时才应注入，否则合成事件会落到别的前台 App。
import AppKit
import CoreGraphics

final class CountWindow: NSWindow {
    var typed = ""
    var keys = 0
    override func sendEvent(_ event: NSEvent) {
        if event.type == .keyDown {
            keys += 1
            typed += event.characters ?? ""
        }
        super.sendEvent(event)
    }
}

final class CountView: NSView {
    override var acceptsFirstResponder: Bool { true }
}

// 参数：probe [outfile] [seconds]；probe <seconds> 保持旧用法。
let args = CommandLine.arguments
var outPath: String?
var seconds = 5.0
if args.count > 1 {
    if Double(args[1]) != nil {
        seconds = Double(args[1]) ?? 5.0
    } else {
        outPath = args[1]
        if args.count > 2 { seconds = Double(args[2]) ?? 5.0 }
    }
}

var outHandle: FileHandle?
if let outPath {
    FileManager.default.createFile(atPath: outPath, contents: nil)
    outHandle = FileHandle(forWritingAtPath: outPath)
}
func report(_ line: String) {
    print(line)
    fflush(stdout)
    if let outHandle {
        outHandle.write((line + "\n").data(using: .utf8)!)
        try? outHandle.synchronize()
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let view = CountView(frame: NSRect(x: 0, y: 0, width: 640, height: 420))
let window = CountWindow(
    contentRect: NSRect(x: 0, y: 0, width: 640, height: 420),
    styleMask: [.titled, .closable, .resizable],
    backing: .buffered,
    defer: false
)
window.title = "vdev HID type acceptance probe"
window.contentView = view
window.acceptsMouseMovedEvents = true
window.center()
window.makeKeyAndOrderFront(nil)
window.makeFirstResponder(view)
app.activate(ignoringOtherApps: true)

var tapCount = 0
let mask = CGEventMask(1 << CGEventType.keyDown.rawValue)
let tap = CGEvent.tapCreate(
    tap: .cgSessionEventTap,
    place: .headInsertEventTap,
    options: .listenOnly,
    eventsOfInterest: mask,
    callback: { _, _, event, refcon in
        if let refcon { refcon.assumingMemoryBound(to: Int.self).pointee += 1 }
        return Unmanaged.passUnretained(event)
    },
    userInfo: &tapCount
)
if let tap {
    let src = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
    CFRunLoopAddSource(CFRunLoopGetCurrent(), src, .commonModes)
    CGEvent.tapEnable(tap: tap, enable: true)
}
let tapOk = tap != nil

let deadline = Date().addingTimeInterval(4)
while Date() < deadline && !(app.isActive && window.isKeyWindow) {
    RunLoop.current.run(until: Date().addingTimeInterval(0.05))
}
// tap 计数的基线放在 READY 行：验收脚本只应比较注入前后的增量，
// 避免用户/系统在同一窗口内产生的真实键事件被误算成注入丢失。
report("READY active=\(app.isActive) key=\(window.isKeyWindow) tap_ok=\(tapOk) tap=\(tapCount)")

DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
    report("SUMMARY keys=\(window.keys) tap=\(tapCount) tap_ok=\(tapOk) active=\(app.isActive) key=\(window.isKeyWindow)")
    report("TYPED=\(window.typed)")
    app.terminate(nil)
}
app.run()
