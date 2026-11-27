// This Mac's own speaker heard by its own microphone, both on the host clock: what the Mac's side adds to a
// calibration's "heard − played" (the microphone's path and how its buffers are timed). With it known, what a TV adds
// after its DAC can be read in absolute terms, not only against the other TVs.
// SPDX-License-Identifier: GPL-3.0-or-later
import AVFoundation
import Foundation
import LuchEngine
import LuchFind

enum SelfLoop {
    /// Plays `n` sweeps on the default output at known host times while the microphone records, and measures them.
    static func run(n: Int = 4, gain: Double = 0.15) throws -> [String: Any] {
        let mic = MicRecorder()
        try mic.start()
        defer { mic.stop() }

        let out = AVAudioEngine()
        let player = AVAudioPlayerNode()
        out.attach(player)
        let rate = out.outputNode.outputFormat(forBus: 0).sampleRate
        guard rate > 0, let mono = AVAudioFormat(standardFormatWithSampleRate: rate, channels: 1) else {
            throw CalibrationError("no sound output")
        }
        out.connect(player, to: out.mainMixerNode, format: mono)
        try out.start()
        defer { out.stop() }
        player.play()

        let sweep = AcousticProbe.sweep(rate: Int(rate), f0: AcousticProbe.f0, f1: AcousticProbe.f1,
                                        ms: AcousticProbe.ms, gain: gain)
        guard let buffer = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: AVAudioFrameCount(sweep.count)),
              let ch = buffer.floatChannelData?[0] else { throw CalibrationError("no buffer") }
        buffer.frameLength = AVAudioFrameCount(sweep.count)
        for i in 0..<sweep.count { ch[i] = sweep[i] }

        // Host times in seconds, the clock the microphone's buffers carry.
        let now = AVAudioTime.seconds(forHostTime: mach_absolute_time())
        let starts = (0..<n).map { now + 1.0 + Double($0) * 0.7 }
        for s in starts {
            player.scheduleBuffer(buffer, at: AVAudioTime(hostTime: AVAudioTime.hostTime(forSeconds: s)))
        }
        Thread.sleep(forTimeInterval: (starts.last ?? now) - now + 0.2 + 0.6)

        let snap = mic.snapshot()
        let rec = MicRecording(samples: snap.samples, rate: snap.rate, anchors: snap.anchors,
                               captureLatency: AudioLatency.input)
        let probe = AcousticProbe(rate: Int(rec.rate.rounded()))
        let positions = starts.compactMap { rec.position(host: $0) }
        guard positions.count == starts.count else { throw CalibrationError("the microphone gave no timestamps") }
        let r = probe.measure(rec.samples, scheduled: positions, windowMs: 150)
        // The microphone's path is taken off by the recording: what is left is the speaker's path and the air.
        var result: [String: Any] = [
            "output_latency_ms": AudioLatency.output * 1000,
            "input_latency_ms": AudioLatency.input * 1000,
            "peak": rec.peak,
            "why": r.why,
            "psr_db": r.arrivals.map(\.psrDb),
        ]
        let each = zip(r.arrivals, positions).map { a, p in (a.at - p) * 1000 / rec.rate }
        result["each_ms"] = each
        if r.accepted { result["loop_ms"] = r.delaySamples * 1000 / rec.rate }
        return result
    }
}
