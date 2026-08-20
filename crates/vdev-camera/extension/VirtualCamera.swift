import CoreMediaIO

/// 组装 provider / device / stream，接线 Rust 帧源。
final class VirtualCamera {
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
        self.device = CMIOExtensionDevice(
            localizedName: localizedName,
            deviceID: UUID(),
            legacyDeviceID: nil,
            source: deviceSource
        )
        self.provider = CMIOExtensionProvider(source: providerSource, clientQueue: nil)

        self.providerSource = providerSource
        self.deviceSource = deviceSource
        self.streamSource = streamSource
        self.frameSource = frameSource

        try provider.addDevice(device)
        try device.addStream(stream)

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
        stream.send(sampleBuffer, discontinuity: discontinuity, hostTimeInNanoseconds: hostTimeInNanoseconds)
    }
}
