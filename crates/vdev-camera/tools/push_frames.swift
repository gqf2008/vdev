#!/usr/bin/env swift
// vdev-camera 真实帧推流工具
//
// 用法：
//   swift push_frames.swift image <png路径> [--fps 30]    # 推送一张图片（循环）
//   swift push_frames.swift screen  [--fps 30]             # 推送真实屏幕画面（需屏幕录制权限）
//
// 协议：连接 127.0.0.1:27890，发送 36 字节小端头 + BGRA32 payload（见扩展 FrameChannel.swift）。

import Foundation
import Network
import CoreGraphics
import ImageIO
import CoreMedia
import ScreenCaptureKit

let kPort: UInt16 = 27890
let kMagic: UInt32 = 0x56444652
let kVersion: UInt32 = 1
let kWidth = 1280
let kHeight = 720

func eprint(_ s: String) {
    FileHandle.standardError.write((s + "\n").data(using: .utf8)!)
}

// SCStream 必须全局持有，否则函数返回即释放导致采集停止
var activeStream: SCStream?

// MARK: - 帧发送器

final class FrameSender: NSObject, SCStreamOutput {
    static let headerSize = 36
    private var conn: NWConnection?
    private let queue = DispatchQueue(label: "push")
    private var sending = false
    private var connected = false
    private var sent = 0
    private var lastLog = Date.distantPast

    func connect(retries: Int = 60) {
        for _ in 0..<retries {
            let c = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: kPort)!,
                                 using: .tcp)
            let sem = DispatchSemaphore(value: 0)
            c.stateUpdateHandler = { state in
                if case .ready = state {
                    self.connected = true
                    sem.signal()
                } else if case .failed(let e) = state {
                    print("连接失败: \(e)")
                    sem.signal()
                }
            }
            c.start(queue: queue)
            if sem.wait(timeout: .now() + 2) == .timedOut || !connected {
                c.cancel()
                eprint("等待扩展就绪（127.0.0.1:\(kPort)）… 请确认已安装并激活 vdev-camera")
                sleep(1)
                continue
            }
            conn = c
            eprint("已连接扩展 FrameChannel")
            return
        }
        eprint("连接失败：扩展未就绪")
        exit(1)
    }

    func sendFrame(_ data: Data) {
        queue.async { [weak self] in
            guard let self, let conn = self.conn, self.connected, !self.sending else { return }
            self.sending = true
            conn.send(content: data, completion: .contentProcessed { _ in
                self.sending = false
                self.sent += 1
                let now = Date()
                if now.timeIntervalSince(self.lastLog) > 3 {
                    eprint("已推 \(self.sent) 帧")
                    self.lastLog = now
                }
            })
        }
    }

    // MARK: SCStreamOutput（屏幕捕获回调）
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, let pb = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return }
        let w = CVPixelBufferGetWidth(pb)
        let h = CVPixelBufferGetHeight(pb)
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let data = Data(bytes: base, count: stride * h)
        let now = CMClockGetTime(CMClockGetHostTimeClock())
        let ptsNs = UInt64(now.seconds * Double(NSEC_PER_SEC))
        sendFrame(makeFrame(data: data, width: w, height: h, stride: stride, ptsNs: ptsNs))
    }
}

// MARK: - 协议组帧

func makeFrame(data: Data, width: Int, height: Int, stride: Int, ptsNs: UInt64) -> Data {
    var header = Data(capacity: FrameSender.headerSize + data.count)
    func appendU32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { header.append(contentsOf: $0) } }
    func appendU64(_ v: UInt64) { withUnsafeBytes(of: v.littleEndian) { header.append(contentsOf: $0) } }
    appendU32(kMagic)
    appendU32(kVersion)
    appendU32(UInt32(width))
    appendU32(UInt32(height))
    appendU32(UInt32(stride))
    appendU64(ptsNs)
    appendU64(UInt64(data.count))
    header.append(data)
    return header
}

// MARK: - 图片模式

func loadBGRA(path: String, targetW: Int, targetH: Int) -> (Data, Int)? {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else {
        print("无法读取图片: \(path)")
        return nil
    }
    var data = Data(count: targetW * targetH * 4)
    let cs = CGColorSpaceCreateDeviceRGB()
    let ok = data.withUnsafeMutableBytes { raw -> Bool in
        guard let ctx = CGContext(data: raw.baseAddress, width: targetW, height: targetH,
                                  bitsPerComponent: 8, bytesPerRow: targetW * 4, space: cs,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue
                                    | CGBitmapInfo.byteOrder32Little.rawValue) else { return false }
        ctx.interpolationQuality = .high
        ctx.draw(img, in: CGRect(x: 0, y: 0, width: targetW, height: targetH))
        return true
    }
    guard ok else { print("图片转换失败"); return nil }
    return (data, targetW * 4)
}

// MARK: - 屏幕模式

func startScreenStream(sender: FrameSender, fps: Int) async throws {
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard let display = content.displays.first else {
        print("没有可用显示器")
        exit(1)
    }
    let filter = SCContentFilter(display: display, excludingWindows: [])
    let config = SCStreamConfiguration()
    config.width = kWidth
    config.height = kHeight
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.minimumFrameInterval = CMTime(value: 1, timescale: Int32(fps))
    config.queueDepth = 4
    config.showsCursor = false

    let stream = SCStream(filter: filter, configuration: config, delegate: nil)
    try stream.addStreamOutput(sender, type: .screen, sampleHandlerQueue: DispatchQueue(label: "screen"))
    try await stream.startCapture()
    activeStream = stream
    print("屏幕推流中（\(kWidth)x\(kHeight) @ \(fps)fps），Ctrl+C 停止")
}

// MARK: - main

func usage() -> Never {
    print("用法:")
    print("  swift push_frames.swift image <png> [--fps 30]")
    print("  swift push_frames.swift screen [--fps 30]")
    exit(1)
}

let args = CommandLine.arguments
guard args.count >= 2 else { usage() }
let mode = args[1]
var fps = 30
if let i = args.firstIndex(of: "--fps"), i + 1 < args.count {
    fps = Int(args[i + 1]) ?? 30
}

let sender = FrameSender()
sender.connect()

switch mode {
case "image":
    guard args.count >= 3 else { usage() }
    guard let (data, stride) = loadBGRA(path: args[2], targetW: kWidth, targetH: kHeight) else { exit(1) }
    print("图片推流中（\(kWidth)x\(kHeight) @ \(fps)fps），Ctrl+C 停止")
    var pts: UInt64 = 0
    let interval = 1_000_000_000 / UInt64(fps)
    let now = CMClockGetTime(CMClockGetHostTimeClock())
    pts = UInt64(now.seconds * Double(NSEC_PER_SEC))
    Timer.scheduledTimer(withTimeInterval: 1.0 / Double(fps), repeats: true) { _ in
        pts += interval
        sender.sendFrame(makeFrame(data: data, width: kWidth, height: kHeight, stride: stride, ptsNs: pts))
    }
    RunLoop.main.run()

case "screen":
    let task = Task {
        do {
            try await startScreenStream(sender: sender, fps: fps)
        } catch {
            eprint("屏幕推流失败: \(error)")
            eprint("提示：需先在 系统设置 → 隐私与安全性 → 屏幕录制 中允许本终端/脚本。")
            exit(1)
        }
    }
    RunLoop.main.run()
    _ = task

default:
    usage()
}
