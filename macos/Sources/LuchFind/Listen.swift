// The TV saying its card by sound (PROTOCOL.md 1d), heard through this Mac's microphone: AVAudioEngine's input at
// whatever rate the device runs, brought to the modem's 48 kHz mono, decoded every half second.
// SPDX-License-Identifier: GPL-3.0-or-later
import AVFoundation
import Foundation

/// Samples in at any format, frames out: what the microphone path does, without the microphone (tests feed it).
public final class SoundDecoder {
    private let target = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(Modem.rate), channels: 1,
                                       interleaved: false)!
    private var converter: AVAudioConverter?
    private var buf: [Float] = []
    private var sinceDecode = 0
    /// A frame is at most ~3.6 s; a little more than that is kept.
    private let keep = Modem.rate * 6

    public init() {}

    /// Takes a buffer from the microphone; returns the frames heard once there are any.
    public func feed(_ input: AVAudioPCMBuffer) -> [[UInt8]] {
        append(input)
        guard sinceDecode >= Modem.rate / 2 else { return [] }
        sinceDecode = 0
        let found = Modem.demodulate(buf)
        if found.isEmpty, buf.count > keep { buf.removeFirst(buf.count - keep) }
        return found
    }

    /// What is left at the end.
    public func flush() -> [[UInt8]] { Modem.demodulate(buf) }

    private func append(_ input: AVAudioPCMBuffer) {
        if converter == nil || converter?.inputFormat != input.format {
            converter = AVAudioConverter(from: input.format, to: target)
        }
        guard let converter, input.frameLength > 0 else { return }
        let ratio = target.sampleRate / input.format.sampleRate
        let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 64
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return }
        var given = false
        var error: NSError?
        converter.convert(to: out, error: &error) { _, status in
            if given {
                status.pointee = .noDataNow
                return nil
            }
            given = true
            status.pointee = .haveData
            return input
        }
        guard error == nil, let ch = out.floatChannelData?[0] else { return }
        buf.append(contentsOf: UnsafeBufferPointer(start: ch, count: Int(out.frameLength)))
        sinceDecode += Int(out.frameLength)
    }
}

/// Microphone and camera are opened only when the person asked to look that way; the first time macOS asks them.
func allowed(_ media: AVMediaType, what: String) throws {
    switch AVCaptureDevice.authorizationStatus(for: media) {
    case .authorized: return
    case .notDetermined:
        let answer = DispatchSemaphore(value: 0)
        var ok = false
        AVCaptureDevice.requestAccess(for: media) {
            ok = $0
            answer.signal()
        }
        _ = answer.wait(timeout: .now() + 120)
        if ok { return }
    default: break
    }
    throw FindError("Luch is not allowed to use the \(what) (System Settings, Privacy & Security, \(what.capitalized))")
}

public enum Microphone {
    /// Listens on the default microphone for `seconds`, decoding as it goes; returns the first frames heard.
    public static func listen(seconds: TimeInterval) throws -> [[UInt8]] {
        try allowed(.audio, what: "microphone")
        let engine = AVAudioEngine()
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw FindError("no microphone") }
        let decoder = SoundDecoder()
        let lock = NSLock()
        var heard: [[UInt8]] = []
        let done = DispatchSemaphore(value: 0)
        let work = DispatchQueue(label: "luch.listen")
        input.installTap(onBus: 0, bufferSize: 4800, format: format) { buffer, _ in
            // Decoding takes a while: off the audio thread, in order.
            guard let copy = AVAudioPCMBuffer(pcmFormat: buffer.format, frameCapacity: buffer.frameLength) else { return }
            copy.frameLength = buffer.frameLength
            for c in 0..<Int(buffer.format.channelCount) {
                if let from = buffer.floatChannelData?[c], let to = copy.floatChannelData?[c] {
                    to.update(from: from, count: Int(buffer.frameLength))
                }
            }
            work.async {
                let found = decoder.feed(copy)
                guard !found.isEmpty else { return }
                lock.lock()
                let first = heard.isEmpty
                heard = found
                lock.unlock()
                if first { done.signal() }
            }
        }
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw FindError("cannot open the microphone: \(error.localizedDescription)")
        }
        _ = done.wait(timeout: .now() + seconds)
        input.removeTap(onBus: 0)
        engine.stop()
        work.sync {}
        lock.lock()
        defer { lock.unlock() }
        return heard.isEmpty ? work.sync { decoder.flush() } : heard
    }
}
