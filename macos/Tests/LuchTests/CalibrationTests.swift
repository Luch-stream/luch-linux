// The calibration's detector (the receiver's Probe.java, ported) on recordings synthesised in memory, with the Java
// tests' rooms and numbers; luchd's arithmetic with its test vectors; the schedule; the microphone's clock mapping.
// Nothing is played or recorded.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import XCTest
@testable import LuchEngine
import LuchLocal

final class CalibrationTests: XCTestCase {
    static let rate = SimRoom.rate, sweeps = 4
    static let gain = 0.3, windowMs = 150.0

    // MARK: - the same numbers as the JVM

    func testJavaRandomIsJavas() {
        let r = JavaRandom(42)
        // From java.util.Random(42) on the JVM.
        XCTAssertEqual(r.nextDouble(), 0.72756368003286810, accuracy: 1e-16)
        XCTAssertEqual(r.nextGaussian(), 1.1027444977874560, accuracy: 1e-12)
        XCTAssertEqual(r.nextBoolean(), false)
        XCTAssertEqual(r.nextInt(6), 2)
    }

    func testTheSweepIsSampleForSampleTheReceivers() {
        let s = AcousticProbe.sweep(rate: 48000, f0: 2000, f1: 6000, ms: 200, gain: 0.3)
        XCTAssertEqual(s.count, 9600)
        // Probe.sweep(48000, 2000, 6000, 200, 0.3) on the JVM.
        XCTAssertEqual(s[100], 0.103523269, accuracy: 1e-7)
        XCTAssertEqual(s[4800], -0.0682808161, accuracy: 1e-7)
        XCTAssertEqual(s[9000], -0.289770603, accuracy: 1e-7)
    }

    /// DryRun.run(seed) on the JVM: accepted, measured ms, weakest PSR. The port measures the same recordings the same.
    func testTheDryRunMeasuresWhatTheJavaOneMeasures() {
        let java: [(Double, Double, Double)] = [(75.791666667, 75.792180086, 21.654053),
                                                (75.770833333, 75.770994443, 21.824028),
                                                (75.812500000, 75.813745050, 21.251354),
                                                (75.791666667, 75.792223662, 21.482704),
                                                (75.750000000, 75.750777890, 21.496287)]
        for (seed, j) in java.enumerated() {
            let o = DryRun.run(seed: Int64(seed))
            XCTAssertTrue(o.accepted, o.why)
            XCTAssertEqual(o.trueMs, j.0, accuracy: 1e-6)
            XCTAssertEqual(o.measuredMs, j.1, accuracy: 1e-5, "seed \(seed)")
            XCTAssertEqual(o.psrDb, j.2, accuracy: 1e-3)
        }
    }

    /// DryRunTest: whatever the seed, the made-up recording is measured right.
    func testTheDryRunMeasuresWhatItMadeUp() {
        for seed in 0..<20 {
            let o = DryRun.run(seed: Int64(seed))
            XCTAssertTrue(o.accepted, o.why)
            XCTAssertEqual(o.measuredMs, o.trueMs, accuracy: 0.1, "seed \(seed)")
            XCTAssertGreaterThanOrEqual(o.psrDb, AcousticProbe.minPsrDb)
        }
    }

    /// CalibrationTest.trial: one room's {accepted, error ms, snr dB, true delay ms}.
    static func trial(_ seed: Int64, snrLo: Double, snrHi: Double, clipP: Double, probe: AcousticProbe,
                      fft: Fft) -> (accepted: Bool, err: Double, snr: Double, delayMs: Double, measuredMs: Double) {
        let r = JavaRandom(seed &* Int64(bitPattern: 0x9E3779B97F4A7C15) &+ 17)
        let room = SimRoom(r)
        // What the device adds after its DAC plus the travel to the microphone: 2 to 120 ms, to a fraction of a sample.
        let delay = 0.002 + 0.118 * r.nextDouble()
        let snr = snrLo + (snrHi - snrLo) * r.nextDouble()
        let lead = rate / 2, gap = Int(0.7 * Double(rate))
        let exact = delay * Double(rate)
        let scheduled = (0..<sweeps).map { Double(lead + $0 * gap) }
        let at = scheduled.map { Int(($0 + exact).rounded(.down)) }
        let resp = room.response(fraction: exact - exact.rounded(.down), gain: gain, fft: fft)
        var rec = SimRoom.place(lead + sweeps * gap + rate, resp, at: at)
        room.addFans(&rec, snrDb: snr, sweepGain: gain)
        if r.nextDouble() < clipP { SimRoom.clip(&rec, level: 0.2 + 0.6 * r.nextDouble()) }
        let res = probe.measure(rec, scheduled: scheduled, windowMs: windowMs)
        let ms = res.delayMs(rate: rate)
        return (res.accepted, res.accepted ? ms - delay * 1000 : .nan, snr, delay * 1000, ms)
    }

    func testRoomsMeasureWhatTheJavaOnesMeasure() {
        // CalibrationTest.trial(seed, 0, 20, 0) on the JVM: measured ms, snr, true delay ms.
        let java: [(Double, Double, Double)] = [(86.644786101, 17.289695, 86.642442101),
                                                (50.514714697, 6.624074, 50.514643397),
                                                (116.059824174, 3.958922, 116.052119336),
                                                (26.131303898, 14.114218, 26.124858050),
                                                (89.851135977, 18.213986, 89.850569134),
                                                (78.490660508, 18.194833, 78.491172242)]
        let probe = AcousticProbe(rate: Self.rate), fft = Fft(32768)
        for (seed, j) in java.enumerated() {
            let t = Self.trial(Int64(seed), snrLo: 0, snrHi: 20, clipP: 0, probe: probe, fft: fft)
            XCTAssertTrue(t.accepted)
            XCTAssertEqual(t.snr, j.1, accuracy: 1e-5)
            XCTAssertEqual(t.delayMs, j.2, accuracy: 1e-6)
            XCTAssertEqual(t.measuredMs, j.0, accuracy: 1e-4, "seed \(seed)")
        }
    }

    // MARK: - the pass criteria (docs: sync, 8.1): the JVM runs 1000 and 10 000 rooms; a debug build here takes about
    // a second of CPU per room, so fewer, the same seeds

    func testAtZeroDecibelsOrBetterEachDelayIsWithinHalfAMillisecond() {
        let n = 100
        let t = parallelTrials(n) { i, p, f in Self.trial(Int64(i), snrLo: 0, snrHi: 20, clipP: 0, probe: p, fft: f) }
        let accepted = t.filter(\.accepted)
        let worst = accepted.map { abs($0.err) }.max() ?? 0
        let log = String(format: "calibration, SNR 0..20 dB, %d rooms: accepted %.1f %%, worst %.3f ms", n,
                         100.0 * Double(accepted.count) / Double(n), worst)
        print(log)
        XCTAssertLessThanOrEqual(worst, 0.5, log)
        XCTAssertGreaterThanOrEqual(accepted.count, n * 95 / 100, log)
    }

    func testNeverAcceptsAnErrorBeyondTwoMillisecondsInLoudClippedRooms() {
        let n = 250
        let t = parallelTrials(n) { i, p, f in
            Self.trial(100_000 + Int64(i), snrLo: -5, snrHi: 20, clipP: 0.15, probe: p, fft: f)
        }
        let accepted = t.filter(\.accepted)
        let wrong = accepted.filter { abs($0.err) > 2 }.count
        let log = String(format: "calibration, SNR -5..20 dB, 15 %% clipped, %d rooms: accepted %.1f %%, wrong %d, " +
                         "worst %.3f ms", n, 100.0 * Double(accepted.count) / Double(n), wrong,
                         accepted.map { abs($0.err) }.max() ?? 0)
        print(log)
        XCTAssertEqual(wrong, 0, log)
    }

    /// White noise right in the sweep's band: measuring turns into refusing, never into measuring wrong.
    func testInBandNoiseTurnsMeasuringIntoRefusingNeverIntoWrong() {
        var log = "calibration, white noise in band, 40 rooms per SNR:"
        var wrongAll = 0
        for snr in stride(from: 0, through: -40, by: -10) {
            let t = parallelTrials(40) { i, probe, fft -> (Bool, Double) in
                let r = JavaRandom((7_000 + Int64(snr) * 1000 + Int64(i)) &* Int64(bitPattern: 0x9E3779B97F4A7C15))
                let room = SimRoom(r)
                let delay = 0.002 + 0.118 * r.nextDouble(), exact = delay * Double(Self.rate)
                let sch = (0..<Self.sweeps).map { Double(Self.rate / 2 + $0 * Int(0.7 * Double(Self.rate))) }
                let pos = sch.map { Int(($0 + exact).rounded(.down)) }
                var rec = SimRoom.place(Self.rate / 2 + Self.sweeps * Int(0.7 * Double(Self.rate)) + Self.rate,
                                        room.response(fraction: exact - exact.rounded(.down), gain: Self.gain, fft: fft),
                                        at: pos)
                room.addWhite(&rec, snrDb: Double(snr), sweepGain: Self.gain)
                let res = probe.measure(rec, scheduled: sch, windowMs: Self.windowMs)
                return (res.accepted, res.accepted ? res.delayMs(rate: Self.rate) - delay * 1000 : 0)
            }
            let acc = t.filter(\.0)
            let wrong = acc.filter { abs($0.1) > 2 }.count
            wrongAll += wrong
            log += " \(snr) dB: \(acc.count * 100 / 40) % (wrong \(wrong));"
        }
        print(log)
        XCTAssertEqual(wrongAll, 0, log)
    }

    // MARK: - captures that must be refused

    private func capture(_ seed: Int64, _ spoil: (inout [Float]) -> Void) -> ([Float], [Double]) {
        let r = JavaRandom(seed)
        let room = SimRoom(r)
        let lead = Self.rate / 2, gap = Int(0.7 * Double(Self.rate))
        let scheduled = (0..<Self.sweeps).map { Double(lead + $0 * gap) }
        let at = scheduled.map { Int($0) + Self.rate * 30 / 1000 }
        var rec = SimRoom.place(lead + Self.sweeps * gap + Self.rate,
                                room.response(fraction: 0.3, gain: Self.gain, fft: Fft(32768)), at: at)
        room.addFans(&rec, snrDb: 10, sweepGain: Self.gain)
        spoil(&rec)
        return (rec, scheduled)
    }

    private func measure(_ c: ([Float], [Double])) -> AcousticProbe.Result {
        AcousticProbe(rate: Self.rate).measure(c.0, scheduled: c.1, windowMs: Self.windowMs)
    }

    func testAMutedMicrophoneIsRefused() {
        let res = measure(capture(1) { x in x = x.map { _ in 0 } })
        XCTAssertFalse(res.accepted, res.why)
    }

    func testFansAloneAreRefused() {
        // The device did not play at all: the recording is only the room's noise, as loud as a fan close by.
        let room = SimRoom(JavaRandom(2))
        let s = [0.5, 1.2, 1.9, 2.6].map { $0 * Double(Self.rate) }
        var rec = [Float](repeating: 0, count: 4 * Self.rate)
        room.addFans(&rec, snrDb: -10, sweepGain: Self.gain)
        let res = AcousticProbe(rate: Self.rate).measure(rec, scheduled: s, windowMs: Self.windowMs)
        XCTAssertFalse(res.accepted, res.why)
    }

    func testADoorSlamOverOneSweepIsRefusedOrHarmless() {
        let res = measure(capture(3) { x in
            let r = JavaRandom(4)
            let at = Int(Double(Self.rate / 2) + 0.7 * Double(Self.rate) + 0.01 * Double(Self.rate))
            for i in 0..<Self.rate / 10 { x[at + i] += Float(3 * r.nextGaussian() * exp(-Double(i) / 800)) }
        })
        XCTAssertTrue(!res.accepted || abs(res.delayMs(rate: Self.rate) - 30) < 0.5, res.why)
    }

    func testSomeoneTalkingOverTheSweepsIsRefusedOrHarmless() {
        let res = measure(capture(5) { x in
            // Voiced speech, roughly: harmonics of a moving 120–220 Hz pitch up to 4 kHz, in syllables, loud.
            let r = JavaRandom(6)
            let rate = Double(Self.rate)
            var phase = 0.0
            for i in x.indices {
                let f = 170 + 50 * sin(2 * Double.pi * 3 * Double(i) / rate)
                phase += 2 * Double.pi * f / rate
                let syll = max(0, sin(2 * Double.pi * 4 * Double(i) / rate))
                var v = 0.0
                var h = 1
                while Double(h) * f < 4000 {
                    v += sin(Double(h) * phase) / Double(h)
                    h += 1
                }
                x[i] += Float(0.2 * syll * v + 0.01 * r.nextGaussian())
            }
        })
        XCTAssertTrue(!res.accepted || abs(res.delayMs(rate: Self.rate) - 30) < 0.5, res.why)
    }

    func testAnEchoLouderThanTheDirectSoundIsNotTakenForIt() {
        // A far wall 25 ms later, 6 dB louder than the direct sound (the speaker faces away from the listener).
        let room = SimRoom(JavaRandom(7))
        room.reflDelay[0] = 0.025
        room.reflGain[0] = 2
        let s = [0.5, 1.2, 1.9, 2.6].map { $0 * Double(Self.rate) }
        let at = s.map { Int($0) + Self.rate * 40 / 1000 }
        var rec = SimRoom.place(4 * Self.rate, room.response(fraction: 0, gain: Self.gain, fft: Fft(32768)), at: at)
        room.addFans(&rec, snrDb: 10, sweepGain: Self.gain)
        let res = AcousticProbe(rate: Self.rate).measure(rec, scheduled: s, windowMs: Self.windowMs)
        XCTAssertTrue(res.accepted, res.why)
        XCTAssertEqual(res.delayMs(rate: Self.rate), 40, accuracy: 0.5)
    }

    func testTheSweepIsInItsBandAndStartsAndEndsQuietly() {
        let s = AcousticProbe.sweep(rate: Self.rate, f0: AcousticProbe.f0, f1: AcousticProbe.f1, ms: AcousticProbe.ms,
                                    gain: 0.3)
        XCTAssertEqual(s.count, Self.rate / 5)
        XCTAssertEqual(Double(s.map(abs).max() ?? 0), 0.3, accuracy: 0.01)
        XCTAssertEqual(s[0], 0, accuracy: 1e-6)
        XCTAssertEqual(s[s.count - 1], 0, accuracy: 1e-3)
        let f = Fft(16384)
        var re = [Double](repeating: 0, count: 16384), im = re
        for (i, v) in s.enumerated() { re[i] = Double(v) }
        f.transform(&re, &im, inverse: false)
        var inBand = 0.0, all = 0.0
        for k in 1..<8192 {
            let p = re[k] * re[k] + im[k] * im[k], hz = Double(k) * Double(Self.rate) / 16384
            all += p
            if hz >= AcousticProbe.bandLo && hz <= AcousticProbe.bandHi { inBand += p }
        }
        XCTAssertGreaterThan(inBand / all, 0.99)
    }

    /// A microphone at 44.1 kHz finds the sweep as well: the detector runs at the device's own rate.
    func testAnotherMicrophoneRateMeasuresAsWell() {
        let rate = 44100
        let probe = AcousticProbe(rate: rate)
        let r = JavaRandom(11)
        var rec = [Float](repeating: 0, count: 4 * rate)
        let s = [0.5, 1.2, 1.9, 2.6].map { $0 * Double(rate) }
        let delay = 0.0377 * Double(rate)
        for start in s {
            for i in rec.indices {
                let t = (Double(i) - start - delay) / Double(rate)
                if t >= 0 && t < 0.2 { rec[i] += Float(0.1 * SimRoom.sweepAt(t)) }
            }
        }
        for i in rec.indices { rec[i] += Float(0.01 * r.nextGaussian()) }
        let res = probe.measure(rec, scheduled: s, windowMs: Self.windowMs)
        XCTAssertTrue(res.accepted, res.why)
        XCTAssertEqual(res.delayMs(rate: rate), 37.7, accuracy: 0.05)
    }

    // MARK: - luchd's arithmetic

    /// luchd sync.rs calibration_makes_everyone_as_late_as_the_slowest_and_refuses_disagreement.
    func testCalibrationMakesEveryoneAsLateAsTheSlowestAndRefusesDisagreement() {
        let h = Calibration.Heard.init
        let r = Calibration.offsets(fromHeard: [
            // A phone: heard 12.0–12.3 ms after it played.
            h("phone", [0, 700_000], [12_000, 712_300]),
            // A TV with a soundbar: 72 ms.
            h("tv", [2_000_000, 2_700_000], [2_072_000, 2_772_100]),
            // A projector whose two arrivals disagree by 3 ms: not measured.
            h("projector", [4_000_000, 4_700_000], [4_050_000, 4_753_000]),
            // A tablet whose second sweep was refused by the detector.
            h("tablet", [6_000_000, 6_700_000], [6_030_000, nil]),
        ])
        XCTAssertEqual(r.map(\.id), ["phone", "tv", "projector", "tablet"])
        XCTAssertEqual(r[0].ms, 59.9)
        XCTAssertEqual(r[1].ms, 0.0)
        XCTAssertNil(r[2].ms)
        XCTAssertNil(r[3].ms)
    }

    func testNobodyMeasuredGivesNoCorrection() {
        let r = Calibration.offsets(fromHeard: [Calibration.Heard(id: "a", played: [], heard: []),
                                                Calibration.Heard(id: "b", played: [1], heard: [nil])])
        XCTAssertNil(r[0].ms)
        XCTAssertNil(r[1].ms)
    }

    // MARK: - the schedule

    func testMembersSoundInTurnAfterDAndASecond() {
        let s = ProbeSignal.standard
        // 4 sweeps of 200 ms, 500 ms apart, then half a second of quiet: 3.3 s a member.
        XCTAssertEqual(s.slotUS, 3_300_000)
        let at = Calibration.firstAt(nowUS: 5_000_000, dMs: 150)
        XCTAssertEqual(at, 6_150_000, "now + D + 1 s")
        XCTAssertEqual(Calibration.memberStarts(at: at, count: 3, signal: s), [6_150_000, 9_450_000, 12_750_000])
        XCTAssertEqual(s.sweepStarts(at: 9_450_000), [9_450_000, 10_150_000, 10_850_000, 11_550_000])
        // A member's last sweep ends before the next member's first begins, with the quiet between.
        let last = s.sweepStarts(at: 6_150_000).last! + Int64(s.ms) * 1000
        XCTAssertEqual(9_450_000 - last, 1_000_000)

        let m = s.message(id: "p1", at: at, gain: 0.3)
        XCTAssertEqual(m["t"] as? String, "probe")
        XCTAssertEqual(m["signal"] as? String, "sweep")
        XCTAssertEqual(m["at"] as? Int64, at)
        XCTAssertEqual(m["f0"] as? Int, 2000)
        XCTAssertEqual(m["f1"] as? Int, 6000)
        XCTAssertEqual(m["ms"] as? Int, 200)
        XCTAssertEqual(m["n"] as? Int, 4)
        XCTAssertEqual(m["gap_ms"] as? Int, 500)
        XCTAssertEqual(m["gain"] as? Double, 0.3)
        XCTAssertEqual(Calibration.firstAt(nowUS: 0, dMs: -5), 1_000_000, "no D yet: a second")
    }

    /// The microphone's buffers come with jittered host times and its crystal runs off the host's: the fit puts
    /// every sample where it was captured, both ways.
    func testTheMicrophonesSamplesMapOntoTheHostClock() {
        let rate = 48000.0, drift = 80e-6, start = 1000.0
        let r = JavaRandom(3)
        let anchors = stride(from: 0, to: 48000 * 12, by: 512).map { s -> (sample: Int, host: Double) in
            (s, start + Double(s) / (rate * (1 + drift)) + 0.0002 * (r.nextDouble() - 0.5))
        }
        let rec = MicRecording(samples: [], rate: rate, anchors: anchors)
        for s in [0.0, 100_000.5, 500_000.25] {
            let t = start + s / (rate * (1 + drift))
            XCTAssertEqual(rec.position(host: t)!, s, accuracy: 0.5, "within 10 µs")
            XCTAssertEqual(rec.host(position: s)!, t, accuracy: 10e-6)
        }
        XCTAssertNil(MicRecording(samples: [], rate: rate, anchors: []).position(host: 1))
        // One buffer only: the nominal rate from it.
        let one = MicRecording(samples: [], rate: rate, anchors: [(480, 7.0)])
        XCTAssertEqual(one.position(host: 8)!, 48480, accuracy: 1e-6)
    }

    // MARK: - in words

    func testTheResultsInWords() {
        let lines = CalibrateText.lines([
            ["name": "Kitchen", "measured": true, "delay_ms": 72.44, "offset_ms": 0.0, "psr_db": 24.2, "spread_ms": 0.12,
             "retried": false, "why": "ok"],
            ["name": "Den", "measured": false, "offset_ms": 40.0, "retried": true, "why": "sweep 0: peak only 8.1 dB over noise"],
        ])
        XCTAssertEqual(lines[0], "Kitchen: heard 72.4 ms after it played; offset now +0.0 ms (sweeps within 0.12 ms, " +
                       "24 dB over the noise)")
        XCTAssertEqual(lines[1], "Den: not measured: sweep 0: peak only 8.1 dB over noise (tried twice); offset stays " +
                       "+40.0 ms")
        XCTAssertEqual(lines.count, 3, "and why only the differences matter")
    }
}
