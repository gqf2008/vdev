// vdev HID 文本注入验收探针：AppKit 窗口收键 + 全局 EventTap 计数。
// 编译：swiftc -O macos-hid-type-probe.swift -o /tmp/...
// 用法：probe <seconds>；stdout 以 READY/SUMMARY/TYPED 三行报告状态。
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

let seconds = CommandLine.arguments.count > 1 ? (Double(CommandLine.arguments[1]) ?? 5.0) : 5.0
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

let deadline = Date().addingTimeInterval(4)
while Date() < deadline && !(app.isActive && window.isKeyWindow) {
    RunLoop.current.run(until: Date().addingTimeInterval(0.05))
}
// tap 计数的基线放在 READY 行：验收脚本只应比较注入前后的增量，
// 避免用户/系统在同一窗口内产生的真实键事件被误算成注入丢失。
print("READY active=\(app.isActive) key=\(window.isKeyWindow) tap=\(tapCount)")
fflush(stdout)

DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
    print("SUMMARY keys=\(window.keys) tap=\(tapCount)")
    print("TYPED=\(window.typed)")
    fflush(stdout)
    app.terminate(nil)
}
app.run()
