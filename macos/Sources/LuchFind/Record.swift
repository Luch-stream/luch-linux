// This Mac's microphone recorded as it is, for a group's calibration (docs/research/sync-multiroom.md 6.7): the same
// AVAudioEngine input as Listen.swift, but kept at the device's own rate (a converter would blur which sample was
// captured when) and with each buffer's host time, which maps every sample onto the group's clock.
// SPDX-License-Identifier: GPL-3.0-or-later
import AVFoundation
import CoreMedia
import Foundation

public final class MicRecorder {
    /// A calibration lasts seconds per receiver; more than this is never kept.
    static let maxSeconds = 120.0

    private let engine = AVAudioEngine()
    private let lock = NSLock()
    private var samples: [Float] = []
    private var anchors: [(sample: Int, host: Double)] = []
    private var rate = 0.0
    private var running = false

    public init() {}

    /// Opens the default microphone, asking the person the first time.
    public func start() throws {
        try allowed(.audio, what: "microphone")
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw FindError("no microphone") }
        lock.lock()
        rate = format.sampleRate
        samples = []
        anchors = []
        lock.unlock()
        let limit = Int(format.sampleRate * Self.maxSeconds)
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, when in
            guard let self, let ch = buffer.floatChannelData else { return }
            let n = Int(buffer.frameLength), channels = Int(buffer.format.channelCount)
            // Mono: the channels' mean (a built-in array's beams arrive together).
            var mono = [Float](repeating: 0, count: n)
            for c in 0..<channels {
                for i in 0..<n { mono[i] += ch[c][i] / Float(channels) }
            }
            self.lock.lock()
            defer { self.lock.unlock() }
            guard self.samples.count + n <= limit else { return }
            if when.isHostTimeValid {
                // The host clock as the group's pts clock reads it.
                let host = CMTimeGetSeconds(CMClockMakeHostTimeFromSystemUnits(when.hostTime))
                self.anchors.append((self.samples.count, host))
            }
            self.samples += mono
        }
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw FindError("cannot open the microphone: \(error.localizedDescription)")
        }
        running = true
    }

    /// Everything recorded so far: mono samples, the rate, and each buffer's first sample with its host time (s).
    public func snapshot() -> (samples: [Float], rate: Double, anchors: [(sample: Int, host: Double)]) {
        lock.lock()
        defer { lock.unlock() }
        return (samples, rate, anchors)
    }

    public func stop() {
        guard running else { return }
        running = false
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
    }
}
