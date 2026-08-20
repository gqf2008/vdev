import CoreMediaIO

protocol StreamSourceDelegate: AnyObject {
    func streamSourceShouldAuthorizeStartOfStream(_ source: StreamSource) -> Bool
    func streamSourceDidStartStream(_ source: StreamSource)
    func streamSourceDidStopStream(_ source: StreamSource)
}

final class StreamSource: NSObject, CMIOExtensionStreamSource {
    private let streamFormat: CMIOExtensionStreamFormat
    weak var delegate: StreamSourceDelegate?

    init(streamFormat: CMIOExtensionStreamFormat) {
        self.streamFormat = streamFormat
    }

    var formats: [CMIOExtensionStreamFormat] { [streamFormat] }

    var availableProperties: Set<CMIOExtensionProperty> {
        [.streamActiveFormatIndex, .streamFrameDuration]
    }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>)
        throws -> CMIOExtensionStreamProperties {
        let p = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) { p.activeFormatIndex = 0 }
        if properties.contains(.streamFrameDuration) {
            p.frameDuration = streamFormat.maxFrameDuration
        }
        return p
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {}

    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        delegate?.streamSourceShouldAuthorizeStartOfStream(self) ?? true
    }

    func startStream() throws {
        delegate?.streamSourceDidStartStream(self)
    }

    func stopStream() throws {
        delegate?.streamSourceDidStopStream(self)
    }
}
