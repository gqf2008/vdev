// CMIOExtension 系统扩展入口。
// 注意：摄像头扩展不用 @main，main.swift 顶层语句即入口（与普通 Swift 可执行不同）。

import Foundation
import CoreMediaIO

// 固定参数；后续可改成 UserDefaults/环境变量配置
// 主格式 1920x1080@60，推流通道支持任意尺寸（自动适配）
let width: Int32 = 1920
let height: Int32 = 1080
let frameRate: Int32 = 60
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
