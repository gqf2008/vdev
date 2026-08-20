// CMIOExtension 系统扩展入口。
// 注意：摄像头扩展不用 @main，main.swift 顶层语句即入口（与普通 Swift 可执行不同）。

import Foundation
import CoreMediaIO

// 固定参数；后续可改成 UserDefaults/环境变量配置
let width: Int32 = 1280
let height: Int32 = 720
let frameRate: Int32 = 30
let pattern: Int32 = 0 // SMPTE 彩条

let virtualCamera: VirtualCamera
do {
    virtualCamera = try VirtualCamera(
        localizedName: "vdev-camera",
        dimensions: CMVideoDimensions(width: width, height: height),
        frameRate: frameRate,
        pattern: pattern
    )
} catch {
    NSLog("vdev-camera: init failed: \(error)")
    exit(1)
}

virtualCamera.start()
CFRunLoopRun()
