// ScreenCaptureKit capture: 1920x1080@60 NV12 video-range buffers straight into VideoToolbox, system
// audio in float PCM for the Opus pipe, cursor drawn in.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import CoreMedia
import CoreVideo
import Foundation
import LuchProto
import ScreenCaptureKit

public enum FitMode: String, CaseIterable {
    /// Crop the source to the target aspect: one GPU transform, no black bars, fewer pixels.
    case crop
    /// Fit the source into the target with black bars.
    case letterbox
    /// Stretch the source to the target (what a naive 16:9 request does).
    case stretch
}

public final class ScreenCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    public var onFrame: ((CVPixelBuffer, CMTime) -> Void)?
    public var onAudio: ((CMSampleBuffer) -> Void)?
    public var onStop: ((String) -> Void)?

    private var stream: SCStream?
    private let queue = DispatchQueue(label: "luch.capture", qos: .userInteractive)
    // Audio must not wait behind a 1080p60 encode: its own queue keeps 5 ms frames evenly spaced.
    private let audioQueue = DispatchQueue(label: "luch.capture.audio", qos: .userInteractive)
    private var frames = 0
    private var audioBuffers = 0

    /// What the frame shows, for the receiver's input (PROTOCOL.md 4i): a part of the desktop in global points
    /// (fitted with bars or not), or a window, which may move.
    public enum Shown {
        case area(CGRect, letterboxed: Bool)
        case window(CGWindowID)
    }

    public private(set) var shown: Shown?

    /// `windowID`: that window alone, fitted into the frame with bars. `displayID`: that display (a virtual screen),
    /// waited for as it comes up: the first time for a TV, macOS asks whether to extend the desktop onto it or mirror
    /// this one, and shows it only once that is answered. Else `displayIndex`.
    public func start(displayIndex: Int?, displayID: CGDirectDisplayID? = nil, windowID: CGWindowID? = nil,
                      width: Int, height: Int, fps: Int, audio: Bool, fit: FitMode = .crop) async throws {
        var content = try await Self.shareableContent()
        if let windowID {
            guard let window = content.windows.first(where: { $0.windowID == windowID }) else {
                throw CaptureError.noWindow
            }
            log("capture: window \(windowID) (\(window.owningApplication?.applicationName ?? "?")) -> \(width)x\(height)@\(fps)")
            let config = Self.configuration(width: width, height: height, fps: fps, audio: audio)
            config.scalesToFit = true
            config.preservesAspectRatio = true
            try await begin(SCContentFilter(desktopIndependentWindow: window), config, audio: audio)
            shown = .window(windowID)
            return
        }
        if let id = displayID {
            if !content.displays.contains(where: { $0.displayID == id }) {
                log("capture: waiting for screen \(id) (macOS may ask: Extend or Mirror)")
            }
            for _ in 0..<600 where !content.displays.contains(where: { $0.displayID == id }) {
                try await Task.sleep(nanoseconds: 100_000_000)
                content = try await Self.shareableContent()
            }
        }
        guard !content.displays.isEmpty else { throw CaptureError.noDisplays }
        let displays = content.displays
        let display: SCDisplay
        if let id = displayID {
            guard let d = displays.first(where: { $0.displayID == id }) else { throw CaptureError.noDisplays }
            display = d
        } else if let index = displayIndex {
            guard index >= 0 && index < displays.count else { throw CaptureError.badDisplay(index, displays.count) }
            display = displays[index]
        } else {
            let mainID = CGMainDisplayID()
            display = displays.first(where: { $0.displayID == mainID }) ?? displays[0]
        }
        log("capture: display \(display.displayID) \(display.width)x\(display.height) -> \(width)x\(height)@\(fps) audio=\(audio)")

        let config = Self.configuration(width: width, height: height, fps: fps, audio: audio)
        let bounds = CGDisplayBounds(display.displayID)
        shown = .area(bounds, letterboxed: fit == .letterbox)
        switch fit {
        case .crop:
            // Crop to the target aspect at the source, then scale: a single uniform transform and
            // no wasted letterbox pixels in the encoder.
            let targetAspect = Double(width) / Double(height)
            if let screen = Self.screen(for: display.displayID) {
                let frame = screen.frame
                let sourceAspect = frame.width / frame.height
                var rect = CGRect(origin: .zero, size: frame.size)
                if sourceAspect > targetAspect + 0.001 {
                    let w = frame.height * targetAspect
                    rect = CGRect(x: (frame.width - w) / 2, y: 0, width: w, height: frame.height)
                } else if sourceAspect < targetAspect - 0.001 {
                    let h = frame.width / targetAspect
                    rect = CGRect(x: 0, y: (frame.height - h) / 2, width: frame.width, height: h)
                }
                config.sourceRect = rect
                config.scalesToFit = false
                shown = .area(rect.offsetBy(dx: bounds.minX, dy: bounds.minY), letterboxed: false)
                log(String(format: "capture: crop %.0fx%.0f at (%.0f,%.0f) -> %dx%d",
                           rect.width, rect.height, rect.origin.x, rect.origin.y, width, height))
            }
        case .letterbox:
            if #available(macOS 14.0, *) { config.scalesToFit = true }
        case .stretch:
            config.scalesToFit = false
        }

        try await begin(SCContentFilter(display: display, excludingWindows: []), config, audio: audio)
    }

    private static func configuration(width: Int, height: Int, fps: Int, audio: Bool) -> SCStreamConfiguration {
        let config = SCStreamConfiguration()
        config.width = width
        config.height = height
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.queueDepth = 2
        config.showsCursor = true
        config.capturesAudio = audio
        if audio {
            config.sampleRate = 48_000
            config.channelCount = 2
            // Do not feed our own playback back into the stream (the receiver may echo).
            if #available(macOS 13.0, *) { config.excludesCurrentProcessAudio = true }
        }
        return config
    }

    private func begin(_ filter: SCContentFilter, _ config: SCStreamConfiguration, audio: Bool) async throws {
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        if audio {
            try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: audioQueue)
        }
        try await stream.startCapture()
        self.stream = stream
        log("capture: started")
    }

    public func stop() async {
        guard let stream else { return }
        self.stream = nil
        do {
            try await stream.stopCapture()
            log("capture: stopped after \(frames) frames, \(audioBuffers) audio buffers")
        } catch {
            log("capture: stop: \(error.localizedDescription)")
        }
    }

    private static func screen(for displayID: CGDirectDisplayID) -> NSScreen? {
        NSScreen.screens.first { screen in
            (screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == displayID
        }
    }

    private static func shareableContent() async throws -> SCShareableContent {
        try await withCheckedThrowingContinuation { cont in
            SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, error in
                if let error {
                    cont.resume(throwing: error)
                } else if let content {
                    cont.resume(returning: content)
                } else {
                    cont.resume(throwing: CaptureError.noContent)
                }
            }
        }
    }

    // MARK: - SCStreamOutput

    public func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        switch type {
        case .screen:
            guard sampleBuffer.isValid, isComplete(sampleBuffer),
                  let buffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
            frames += 1
            noteDelivery(sampleBuffer)
            onFrame?(buffer, CMSampleBufferGetPresentationTimeStamp(sampleBuffer))
        case .audio:
            guard sampleBuffer.isValid else { return }
            audioBuffers += 1
            onAudio?(sampleBuffer)
        default:
            break
        }
    }

    public func stream(_ stream: SCStream, didStopWithError error: Error) {
        log("capture: stream stopped: \(error.localizedDescription)")
        onStop?(error.localizedDescription)
    }

    private var delays: [Double] = []
    private static let tick: Double = {
        var tb = mach_timebase_info_data_t()
        mach_timebase_info(&tb)
        return Double(tb.numer) / Double(tb.denom) / 1e6
    }()

    /// How long after a frame was on the screen it reaches us (its displayTime against now): what capture costs
    /// before anything of ours runs, which host_ms does not see. The median every 2 s or so.
    private func noteDelivery(_ sampleBuffer: CMSampleBuffer) {
        guard let raw = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let value = raw.first?[.displayTime] else {
            if delays.isEmpty, frames == 5 {
                let keys = (CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[AnyHashable: Any]])?
                    .first.map { $0.map { "\($0.key)=\(type(of: $0.value))" } } ?? []
                log("capture: no displayTime; attachments: \(keys)")
            }
            return
        }
        let shown = (value as? NSNumber)?.uint64Value ?? (value as? UInt64) ?? 0
        guard shown > 0 else { return }
        let now = mach_absolute_time()
        guard now > shown else { return }
        delays.append(Double(now - shown) * Self.tick)
        if delays.count >= 120 {
            let sorted = delays.sorted()
            log(String(format: "capture: a frame reaches us %.1f ms after it was on the screen (median, p90 %.1f)",
                       sorted[sorted.count / 2], sorted[sorted.count * 9 / 10]))
            delays.removeAll(keepingCapacity: true)
        }
    }

    /// ScreenCaptureKit also delivers "idle" frames (nothing changed): those have no new picture.
    private func isComplete(_ sampleBuffer: CMSampleBuffer) -> Bool {
        guard let raw = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let statusRaw = raw.first?[.status] as? Int,
              let status = SCFrameStatus(rawValue: statusRaw) else {
            return true
        }
        return status == .complete
    }
}

public enum CaptureError: Error, LocalizedError {
    case noDisplays
    case noContent
    case badDisplay(Int, Int)
    case noWindow

    public var errorDescription: String? {
        switch self {
        case .noDisplays: return "no displays to capture"
        case .noWindow: return "the window is gone (closed or minimized)"
        case .noContent: return "ScreenCaptureKit returned no content (screen recording permission?)"
        case let .badDisplay(index, count): return "display \(index) does not exist (\(count) available)"
        }
    }
}
