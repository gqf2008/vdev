import CoreMediaIO

/// 组装 provider / device / stream，接线 Rust 帧源。
final class VirtualCamera {
    private var sendCount: Int = 0
    private let provider: CMIOExtensionProvider
    private let device: CMIOExtensionDevice
    private let stream: CMIOExtensionStream
    private let frameSource: RustFrameSource
    private let providerSource: ProviderSource
    private let deviceSource: DeviceSource
    private let streamSource: StreamSource

    init(localizedName: String, dimensions: CMVideoDimensions, frameRate: Int32, pattern: Int32) throws {
        var fmt: CMFormatDescription?
        let fstatus = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: kCVPixelFormatType_32BGRA,
            width: dimensions.width,
            height: dimensions.height,
            extensions: nil,
            formatDescriptionOut: &fmt
        )
        guard fstatus == noErr, let fmt else {
            throw NSError(domain: "vdev.camera", code: Int(fstatus),
                          userInfo: [NSLocalizedDescriptionKey: "format description failed: \(fstatus)"])
        }

        let streamFormat = CMIOExtensionStreamFormat(
            formatDescription: fmt,
            maxFrameDuration: CMTime(value: 1, timescale: frameRate),
            minFrameDuration: CMTime(value: 1, timescale: frameRate),
            validFrameDurations: nil
        )

        // 注意：CMIOExtensionProvider 每个进程只能有一个（进程级单例），
        // 必须由 ProviderSource 创建，这里直接复用 providerSource.provider。
        let providerSource = ProviderSource(clientQueue: nil)
        let deviceSource = DeviceSource()
        let streamSource = StreamSource(streamFormat: streamFormat)
        let frameSource = try RustFrameSource(
            formatDescription: fmt,
            width: dimensions.width,
            height: dimensions.height,
            frameRate: frameRate,
            pattern: pattern
        )

        self.stream = CMIOExtensionStream(
            localizedName: localizedName,
            streamID: UUID(),
            direction: .source,
            clockType: .hostTime,
            source: streamSource
        )
        // macOS 26：legacyDeviceID 必须填 UUID 字符串（HAL 的 kCMIODevicePropertyDeviceUID），
        // 填 nil 时 cmiod 可能拒绝正确暴露设备（参考 SimCam / ldenoue / fuziki）。
        let deviceID = UUID()
        self.device = CMIOExtensionDevice(
            localizedName: localizedName,
            deviceID: deviceID,
            legacyDeviceID: deviceID.uuidString,
            source: deviceSource
        )
        self.provider = providerSource.provider

        self.providerSource = providerSource
        self.deviceSource = deviceSource
        self.streamSource = streamSource
        self.frameSource = frameSource

        // macOS 26：必须先 addStream 再 addDevice。cmiod 在 provider.addDevice() 那一刻
        // 冻结 device 的 streams 列表；顺序反了 HAL/AVFoundation 看到的是“零流设备”，
        // 能枚举但 startStream 永远不触发（参考 SimCam 踩坑文档）。
        try device.addStream(stream)
        try provider.addDevice(device)

        streamSource.delegate = self
        frameSource.delegate = self
    }

    func start() {
        CMIOExtensionProvider.startService(provider: provider)
    }
}

extension VirtualCamera: StreamSourceDelegate {
    func streamSourceShouldAuthorizeStartOfStream(_ source: StreamSource) -> Bool { true }
    func streamSourceDidStartStream(_ source: StreamSource) { frameSource.startStreaming() }
    func streamSourceDidStopStream(_ source: StreamSource) { frameSource.stopStreaming() }
}

extension VirtualCamera: RustFrameSourceDelegate {
    func frameSource(
        _ source: RustFrameSource,
        didReceiveSampleBuffer sampleBuffer: CMSampleBuffer,
        discontinuity: CMIOExtensionStream.DiscontinuityFlags,
        hostTimeInNanoseconds: UInt64
    ) {
        sendCount += 1
        if sendCount % 30 == 0 || sendCount == 1 {
            NSLog("vdev-camera: send frame #%d", sendCount)
        }
        stream.send(sampleBuffer, discontinuity: discontinuity, hostTimeInNanoseconds: hostTimeInNanoseconds)
    }
}
