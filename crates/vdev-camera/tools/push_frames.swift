#!/usr/bin/env swift
// vdev-camera 真实帧推流工具
//
// 用法：
//   swift push_frames.swift image <png> [--width 1920] [--height 1080] [--fps 60]
//   swift push_frames.swift screen [--display <id>] [--width 1920] [--height 1080] [--fps 60]
//   swift push_frames.swift video  <mp4>  [--width 1920] [--height 1080] [--fps 60]
//
// 协议：连接 127.0.0.1:27890，发送 36 字节小端头 + BGRA32 payload（见扩展 FrameChannel.swift）。

import Foundation
import Network
import CoreGraphics
import ImageIO
import CoreMedia
import CoreVideo
import AVFoundation
import Accelerate
import ScreenCaptureKit

let kPort: UInt16 = 27890
let kMagic: UInt32 = 0x56444652
let kVersion: UInt32 = 1
let kDefaultWidth = 1920
let kDefaultHeight = 1080
let kDefaultFps = 60

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
                    eprint("连接失败: \(e)")
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

// MARK: - 缩放（vImage，BGRA 保序）

func scaleBGRA(data: Data, srcW: Int, srcH: Int, srcStride: Int, dstW: Int, dstH: Int) -> (data: Data, stride: Int) {
    guard srcW != dstW || srcH != dstH else { return (data, srcStride) }
    var src = vImage_Buffer(data: UnsafeMutableRawPointer(mutating: (data as NSData).bytes),
                            height: vImagePixelCount(srcH), width: vImagePixelCount(srcW), rowBytes: srcStride)
    var dstData = Data(count: dstW * dstH * 4)
    let dstStride = dstW * 4
    let rc = dstData.withUnsafeMutableBytes { raw -> vImage_Error in
        var dst = vImage_Buffer(data: raw.baseAddress, height: vImagePixelCount(dstH),
                                width: vImagePixelCount(dstW), rowBytes: dstStride)
        return vImageScale_ARGB8888(&src, &dst, nil, vImage_Flags(kvImageNoFlags))
    }
    if rc != kvImageNoError {
        eprint("vImage 缩放失败: \(rc)")
        exit(1)
    }
    return (dstData, dstStride)
}

// MARK: - 图片模式

func loadBGRA(path: String, targetW: Int, targetH: Int) -> (Data, Int)? {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else {
        eprint("无法读取图片: \(path)")
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
    guard ok else { eprint("图片转换失败"); return nil }
    return (data, targetW * 4)
}

// MARK: - 屏幕模式

func startScreenStream(sender: FrameSender, displayID: CGDirectDisplayID?, width: Int, height: Int, fps: Int) async throws {
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    let display: SCDisplay
    if let displayID {
        guard let d = content.displays.first(where: { $0.displayID == displayID }) else {
            eprint("找不到显示器 ID \(displayID)，可用：")
            for d in content.displays { eprint("  \(d.displayID) \(d.width)x\(d.height)") }
            throw NSError(domain: "vdev.push", code: 1, userInfo: [NSLocalizedDescriptionKey: "display not found"])
        }
        display = d
    } else {
        guard let d = content.displays.first else {
            eprint("没有可用显示器")
            throw NSError(domain: "vdev.push", code: 1, userInfo: [NSLocalizedDescriptionKey: "no display"])
        }
        display = d
    }
    let filter = SCContentFilter(display: display, excludingWindows: [])
    let config = SCStreamConfiguration()
    config.width = width
    config.height = height
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.minimumFrameInterval = CMTime(value: 1, timescale: Int32(fps))
    config.queueDepth = 4
    config.showsCursor = false

    let stream = SCStream(filter: filter, configuration: config, delegate: nil)
    try stream.addStreamOutput(sender, type: .screen, sampleHandlerQueue: DispatchQueue(label: "screen"))
    try await stream.startCapture()
    activeStream = stream
    eprint("屏幕推流中（显示器 \(display.displayID) → \(width)x\(height) @ \(fps)fps），Ctrl+C 停止")
}

// MARK: - 视频模式

func pushVideo(path: String, sender: FrameSender, width: Int, height: Int, fps: Int) {
    let asset = AVURLAsset(url: URL(fileURLWithPath: path))
    guard let reader = try? AVAssetReader(asset: asset) else {
        eprint("无法打开视频: \(path)")
        exit(1)
    }
    guard let track = asset.tracks(withMediaType: .video).first else {
        eprint("没有视频轨: \(path)")
        exit(1)
    }
    let settings: [String: Any] = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA]
    let output = AVAssetReaderTrackOutput(track: track, outputSettings: settings)
    output.alwaysCopiesSampleData = false
    reader.add(output)
    guard reader.startReading() else {
        eprint("视频读取失败: \(String(describing: reader.error))")
        exit(1)
    }

    let sourceFps = track.nominalFrameRate
    let intervalNs = UInt64(1_000_000_000 / Double(fps))
    let startHostNs = UInt64(CMClockGetTime(CMClockGetHostTimeClock()).seconds * Double(NSEC_PER_SEC))
    var index: UInt64 = 0
    eprint("视频推流中（\(path) → \(width)x\(height)，源 \(Int(sourceFps))fps，目标 \(fps)fps），Ctrl+C 停止")

    while let sample = output.copyNextSampleBuffer() {
        defer { index += 1 }
        guard let pb = CMSampleBufferGetImageBuffer(sample) else { continue }
        let srcW = CVPixelBufferGetWidth(pb)
        let srcH = CVPixelBufferGetHeight(pb)
        let srcStride = CVPixelBufferGetBytesPerRow(pb)
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        guard let base = CVPixelBufferGetBaseAddress(pb) else {
            CVPixelBufferUnlockBaseAddress(pb, .readOnly)
            continue
        }
        let raw = Data(bytes: base, count: srcStride * srcH)
        CVPixelBufferUnlockBaseAddress(pb, .readOnly)

        let scaled = scaleBGRA(data: raw, srcW: srcW, srcH: srcH, srcStride: srcStride,
                               dstW: width, dstH: height)
        let targetNs = startHostNs + index * intervalNs
        let nowNs = UInt64(CMClockGetTime(CMClockGetHostTimeClock()).seconds * Double(NSEC_PER_SEC))
        if targetNs > nowNs {
            usleep(useconds_t((targetNs - nowNs) / 1000))
        }
        sender.sendFrame(makeFrame(data: scaled.data, width: width, height: height,
                                   stride: scaled.stride, ptsNs: targetNs))
    }
    eprint("视频推完（\(index) 帧）")
    exit(0)
}

// MARK: - main

func usage() -> Never {
    eprint("用法:")
    eprint("  swift push_frames.swift image <png> [--width W] [--height H] [--fps N]")
    eprint("  swift push_frames.swift screen [--display <id>] [--width W] [--height H] [--fps N]")
    eprint("  swift push_frames.swift video  <mp4>  [--width W] [--height H] [--fps N]")
    exit(1)
}

let args = CommandLine.arguments
guard args.count >= 2 else { usage() }
let mode = args[1]

func opt(_ name: String, default d: Int) -> Int {
    if let i = args.firstIndex(of: name), i + 1 < args.count, let v = Int(args[i + 1]) { return v }
    return d
}
func optDisplay() -> CGDirectDisplayID? {
    if let i = args.firstIndex(of: "--display"), i + 1 < args.count, let v = UInt32(args[i + 1]) { return v }
    return nil
}

let width = opt("--width", default: kDefaultWidth)
let height = opt("--height", default: kDefaultHeight)
let fps = opt("--fps", default: kDefaultFps)

let sender = FrameSender()
sender.connect()

switch mode {
case "image":
    guard args.count >= 3 else { usage() }
    guard let (data, stride) = loadBGRA(path: args[2], targetW: width, targetH: height) else { exit(1) }
    eprint("图片推流中（\(width)x\(height) @ \(fps)fps），Ctrl+C 停止")
    let interval = 1_000_000_000 / UInt64(fps)
    var pts = UInt64(CMClockGetTime(CMClockGetHostTimeClock()).seconds * Double(NSEC_PER_SEC))
    Timer.scheduledTimer(withTimeInterval: 1.0 / Double(fps), repeats: true) { _ in
        pts += interval
        sender.sendFrame(makeFrame(data: data, width: width, height: height, stride: stride, ptsNs: pts))
    }
    RunLoop.main.run()

case "screen":
    let displayID = optDisplay()
    let fps = fps > 30 ? 30 : fps // 屏幕采集默认 30fps，避免 WindowServer 过载
    let task = Task {
        do {
            try await startScreenStream(sender: sender, displayID: displayID,
                                        width: width, height: height, fps: fps)
        } catch {
            eprint("屏幕推流失败: \(error)")
            eprint("提示：需先在 系统设置 → 隐私与安全性 → 屏幕录制 中允许本终端/脚本。")
            exit(1)
        }
    }
    RunLoop.main.run()
    _ = task

case "video":
    guard args.count >= 3 else { usage() }
    pushVideo(path: args[2], sender: sender, width: width, height: height, fps: fps)

default:
    usage()
}
