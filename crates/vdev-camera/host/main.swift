// 程序化 AppKit 入口：没有 storyboard 时，必须手动把 delegate 挂到 NSApplication。
import Cocoa

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.run()
